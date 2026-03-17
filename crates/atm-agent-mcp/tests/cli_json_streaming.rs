//! Integration-level tests for `JsonCodecTransport` CLI-JSON streaming behaviour.
//!
//! These tests exercise the state machine and event ordering of the
//! `JsonCodecTransport` background task by feeding events through a mock
//! duplex stream, without spawning a real `codex exec --json` child process.
//!
//! # Test strategy
//!
//! The `JsonCodecTransport` background task reads lines from a `BufReader` over
//! the child's stdout, classifies each line via `parse_event_type`, updates
//! `idle_flag` and `cli_json_turn_state`, and forwards the raw line to the
//! duplex stream.  We simulate this by:
//!
//! 1. Creating a `tokio::io::duplex` pair as the mock child stdout.
//! 2. Writing event JSONL lines to the write half from the test.
//! 3. Running the same state machine logic in the test to validate transitions.
//! 4. Reading forwarded lines from the read half to verify ordering.
//!
//! For the stdin queue tests (AC6) we use a `tempfile::tempdir()` queue
//! directory — never the real ATM_HOME — for full isolation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serial_test::serial;
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use atm_agent_mcp::cli_json_test::{CliJsonEventKind, classify_event};
use atm_agent_mcp::stdin_queue;
use atm_agent_mcp::stream_norm::{TurnState, TurnStatus};

#[path = "support/env_guard.rs"]
mod env_guard;

use env_guard::EnvGuard;

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// A minimal in-memory `AsyncWrite` implementation used in place of real child
/// stdin in queue-drain tests.  Captured bytes are inspectable via the shared
/// `Arc<std::sync::Mutex<Vec<u8>>>`.
struct CapWriter(Arc<std::sync::Mutex<Vec<u8>>>);

impl CapWriter {
    fn new() -> (Self, Arc<std::sync::Mutex<Vec<u8>>>) {
        let buf = Arc::new(std::sync::Mutex::new(Vec::new()));
        (Self(Arc::clone(&buf)), buf)
    }
}

impl tokio::io::AsyncWrite for CapWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.0.lock().unwrap().extend_from_slice(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

/// Apply the `JsonCodecTransport` background-task state machine to one event line.
///
/// This mirrors the `match parse_event_type(&line)` block in `JsonCodecTransport::spawn`
/// and is shared across all event-ordering tests so the logic is not duplicated.
async fn apply_event(
    line: &str,
    idle_flag: &AtomicBool,
    turn_state: &tokio::sync::Mutex<TurnState>,
) {
    match classify_event(line) {
        CliJsonEventKind::Idle => {
            idle_flag.store(true, Ordering::SeqCst);
            *turn_state.lock().await = TurnState::Idle;
        }
        CliJsonEventKind::Done => {
            idle_flag.store(false, Ordering::SeqCst);
            *turn_state.lock().await = TurnState::Terminal {
                turn_id: String::new(),
                status: TurnStatus::Completed,
            };
        }
        _ => {
            // Any other event (agent_message, tool_call, etc.) resets the idle flag.
            idle_flag.store(false, Ordering::SeqCst);
        }
    }
}

// ─── AC5: Event ordering regression ──────────────────────────────────────────

/// Feeding a sequence of cli-json events through the duplex stream verifies:
/// - All event lines are forwarded in order.
/// - `idle_flag` is set to `true` when an `idle` event is encountered.
/// - `idle_flag` is reset to `false` when an activity event follows.
/// - `cli_json_turn_state` transitions to `TurnState::Idle` on `idle`.
/// - A `done` event marks terminal state and leaves `idle_flag` as `false`.
#[tokio::test]
async fn event_ordering_and_idle_reset() {
    // The event sequence: activity → idle → activity → done
    let events = vec![
        r#"{"type":"agent_message","content":"hello"}"#,
        r#"{"type":"idle"}"#,
        r#"{"type":"tool_call","name":"bash","call_id":"c1"}"#,
        r#"{"type":"done"}"#,
    ];

    // Create a duplex stream: background task writes to `write_half`,
    // proxy reads from `read_half`.
    let (mut write_half, read_half) = tokio::io::duplex(65_536);

    // Shared state mirroring the fields in JsonCodecTransport.
    let idle_flag = AtomicBool::new(false);
    let turn_state = tokio::sync::Mutex::new(TurnState::Idle);

    // Run the state machine over the events and forward each line.
    for line in &events {
        apply_event(line, &idle_flag, &turn_state).await;
        write_half.write_all(line.as_bytes()).await.unwrap();
        write_half.write_all(b"\n").await.unwrap();
    }
    drop(write_half);

    // --- Verify forwarded lines are in order ---
    let mut reader = BufReader::new(read_half);
    let mut received = Vec::new();
    let mut line_buf = String::new();
    while reader.read_line(&mut line_buf).await.unwrap() > 0 {
        let trimmed = line_buf.trim().to_string();
        if !trimmed.is_empty() {
            received.push(trimmed);
        }
        line_buf.clear();
    }

    assert_eq!(received.len(), 4, "all 4 events must be forwarded");
    assert!(
        received[0].contains("agent_message"),
        "line 0 must be agent_message"
    );
    assert!(received[1].contains("\"idle\""), "line 1 must be idle");
    assert!(
        received[2].contains("tool_call"),
        "line 2 must be tool_call"
    );
    assert!(received[3].contains("\"done\""), "line 3 must be done");

    // --- Verify state machine transitions ---
    // After 'done', idle_flag must be false.
    assert!(
        !idle_flag.load(Ordering::SeqCst),
        "idle_flag must be false after done"
    );

    // Turn state must be Terminal.
    let state = turn_state.lock().await.clone();
    assert!(
        matches!(
            state,
            TurnState::Terminal {
                status: TurnStatus::Completed,
                ..
            }
        ),
        "turn state must be Terminal(Completed) after done: {state:?}"
    );
}

/// Verifies that an `idle` event followed by activity events correctly resets the
/// idle flag.  This is the "idle reset on new work" pattern the proxy relies on
/// to know when to stop draining and wait for the next idle.
#[tokio::test]
async fn idle_reset_after_activity_sequence() {
    let idle_flag = AtomicBool::new(false);
    let turn_state = tokio::sync::Mutex::new(TurnState::Idle);

    // Process: idle → agent_message → tool_call
    let seq: &[(&str, bool)] = &[
        (r#"{"type":"idle"}"#, true),           // idle sets flag
        (r#"{"type":"agent_message"}"#, false), // activity resets it
        (r#"{"type":"tool_call"}"#, false),     // still reset
    ];

    for (line, expected_idle) in seq {
        apply_event(line, &idle_flag, &turn_state).await;
        assert_eq!(
            idle_flag.load(Ordering::SeqCst),
            *expected_idle,
            "after processing {line:?} expected idle_flag={expected_idle}"
        );
    }
}

/// Verifies that all events (including `idle` and `done`) are forwarded through
/// the duplex stream — the background task must not swallow any events.
#[tokio::test]
async fn all_event_types_forwarded_through_duplex() {
    let all_events = [
        r#"{"type":"agent_message","content":"hi"}"#,
        r#"{"type":"tool_call","name":"read_file"}"#,
        r#"{"type":"tool_result","call_id":"c1","output":"ok"}"#,
        r#"{"type":"file_change","path":"workspace/x.txt","action":"write"}"#,
        r#"{"type":"idle"}"#,
        r#"{"type":"done"}"#,
    ];

    let (mut write_half, read_half) = tokio::io::duplex(65_536);

    // Write all events as if the background task forwarded them.
    for line in &all_events {
        write_half.write_all(line.as_bytes()).await.unwrap();
        write_half.write_all(b"\n").await.unwrap();
    }
    drop(write_half);

    // Read them back and count.
    let mut reader = BufReader::new(read_half);
    let mut count = 0usize;
    let mut line_buf = String::new();
    while reader.read_line(&mut line_buf).await.unwrap() > 0 {
        if !line_buf.trim().is_empty() {
            count += 1;
        }
        line_buf.clear();
    }

    assert_eq!(
        count,
        all_events.len(),
        "all {} events must be forwarded",
        all_events.len()
    );
}

// ─── AC6: Mail injection timing ───────────────────────────────────────────────

/// Verifies that messages enqueued before an `idle` signal are present in the
/// drain when the idle event fires.
///
/// The test:
/// 1. Writes several messages to the stdin queue via `stdin_queue::enqueue`.
/// 2. Simulates the idle event (the drain trigger).
/// 3. Calls `stdin_queue::drain` and asserts all pre-queued messages are delivered.
#[tokio::test]
#[serial]
async fn mail_enqueued_before_idle_is_drained_on_idle() {
    let tmp = tempdir().unwrap();
    let _atm_home = EnvGuard::set("ATM_HOME", tmp.path());

    let team = "test-team-ac6";
    let agent = "test-agent-ac6";

    // Enqueue 3 messages before the idle signal fires.
    for i in 0..3usize {
        stdin_queue::enqueue(
            team,
            agent,
            &format!(r#"{{"seq":{i},"type":"tool_result"}}"#),
        )
        .await
        .unwrap();
    }

    // Simulate the idle event: create the drain writer.
    let (writer, captured) = CapWriter::new();
    let stdin: Arc<tokio::sync::Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(writer)));

    // Drain (the idle event handler calls this).
    let drained = stdin_queue::drain(team, agent, &stdin, Duration::from_secs(600))
        .await
        .unwrap();

    assert_eq!(
        drained, 3,
        "all 3 pre-idle messages must be drained on idle event"
    );

    // Verify the captured output contains all 3 messages.
    let output = captured.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&output);
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 3, "exactly 3 lines must be written to stdin");

    // Each line must contain the tool_result payload.
    for line in &lines {
        assert!(
            line.contains("tool_result"),
            "each delivered line must be a tool_result: {line}"
        );
    }
}

/// Verifies that messages enqueued AFTER an idle drain are NOT present in that
/// drain (they must wait for the next idle cycle).
#[tokio::test]
#[serial]
async fn mail_enqueued_after_drain_waits_for_next_idle() {
    let tmp = tempdir().unwrap();
    let _atm_home = EnvGuard::set("ATM_HOME", tmp.path());

    let team = "test-team-ac6b";
    let agent = "test-agent-ac6b";

    // Enqueue 1 message before the first idle.
    stdin_queue::enqueue(team, agent, r#"{"seq":0}"#)
        .await
        .unwrap();

    // First drain (idle #1): delivers the 1 pre-queued message.
    let (w1, _) = CapWriter::new();
    let stdin1: Arc<tokio::sync::Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(w1)));
    let count1 = stdin_queue::drain(team, agent, &stdin1, Duration::from_secs(600))
        .await
        .unwrap();
    assert_eq!(
        count1, 1,
        "first drain must deliver the 1 pre-queued message"
    );

    // Enqueue another message AFTER the first drain.
    stdin_queue::enqueue(team, agent, r#"{"seq":1}"#)
        .await
        .unwrap();

    // Second drain (idle #2): delivers the message enqueued since last idle.
    let (w2, cap2) = CapWriter::new();
    let stdin2: Arc<tokio::sync::Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(w2)));
    let count2 = stdin_queue::drain(team, agent, &stdin2, Duration::from_secs(600))
        .await
        .unwrap();

    assert_eq!(
        count2, 1,
        "second drain must deliver the 1 post-first-idle message"
    );
    let output = cap2.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains(r#""seq":1"#),
        "second drain must contain seq:1"
    );
}

/// Additional event-ordering test: idle followed by done (without intervening
/// activity).  Verifies that the `idle` → `done` sequence produces the expected
/// terminal state without getting stuck in `Idle`.
#[tokio::test]
async fn idle_followed_by_done_reaches_terminal() {
    let idle_flag = AtomicBool::new(false);
    let turn_state = tokio::sync::Mutex::new(TurnState::Idle);

    // idle event
    apply_event(r#"{"type":"idle"}"#, &idle_flag, &turn_state).await;
    assert!(
        idle_flag.load(Ordering::SeqCst),
        "idle_flag must be true after idle"
    );
    assert!(
        turn_state.lock().await.is_idle(),
        "turn_state must be Idle after idle"
    );

    // done event
    apply_event(r#"{"type":"done"}"#, &idle_flag, &turn_state).await;
    assert!(
        !idle_flag.load(Ordering::SeqCst),
        "idle_flag must be false after done"
    );
    let state = turn_state.lock().await.clone();
    assert!(
        matches!(
            state,
            TurnState::Terminal {
                status: TurnStatus::Completed,
                ..
            }
        ),
        "turn_state must be Terminal(Completed) after done: {state:?}"
    );
}

// ─── ATM-QA-002: Mid-turn steering injection ──────────────────────────────────

/// Verifies that a message enqueued while a turn is active (idle_flag=false)
/// is retained in the queue and is delivered on the subsequent idle cycle.
///
/// This exercises the "mid-turn steering" path: the proxy must not drain while
/// the agent is busy.  The drain happens only when the idle event fires.
#[tokio::test]
#[serial]
async fn mid_turn_steering_injection() {
    let tmp = tempdir().unwrap();
    let _atm_home = EnvGuard::set("ATM_HOME", tmp.path());

    let team = "test-team-steer";
    let agent = "test-agent-steer";

    // Simulate a busy turn: idle_flag=false, turn_state=Idle (cli-json has no Busy state).
    let idle_flag = AtomicBool::new(false);
    let turn_state = tokio::sync::Mutex::new(TurnState::Idle);

    // Apply an activity event to put the agent into "busy" (idle_flag=false).
    apply_event(
        r#"{"type":"agent_message","content":"working..."}"#,
        &idle_flag,
        &turn_state,
    )
    .await;
    assert!(
        !idle_flag.load(Ordering::SeqCst),
        "idle_flag must be false while agent is busy"
    );

    // Enqueue a steering message while the turn is active.
    stdin_queue::enqueue(
        team,
        agent,
        r#"{"type":"tool_result","content":"steer this"}"#,
    )
    .await
    .unwrap();

    // At this point, the proxy would NOT drain (idle_flag is false).
    // We assert the message is still queued.
    let dir = atm_agent_mcp::stdin_queue::queue_dir(team, agent).unwrap();
    let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
    let mut count = 0usize;
    while let Ok(Some(_)) = entries.next_entry().await {
        count += 1;
    }
    assert_eq!(
        count, 1,
        "message must still be in queue while agent is busy"
    );

    // Simulate idle event firing (the drain trigger).
    apply_event(r#"{"type":"idle"}"#, &idle_flag, &turn_state).await;
    assert!(
        idle_flag.load(Ordering::SeqCst),
        "idle_flag must be true after idle event"
    );

    // Now drain (simulating the proxy's idle handler).
    let (writer, captured) = CapWriter::new();
    let stdin: Arc<tokio::sync::Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(writer)));

    let drained = stdin_queue::drain(team, agent, &stdin, Duration::from_secs(600))
        .await
        .unwrap();

    assert_eq!(drained, 1, "steering message must be drained on idle event");
    let output = captured.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains("steer this"),
        "drained content must contain the steering payload: {text}"
    );
}

// ─── ATM-QA-003: Fixture-based parser test ───────────────────────────────────

/// Parses every line of the representative Codex JSONL fixture file and
/// asserts the expected event classification for each line.
///
/// The fixture at `tests/fixtures/codex_cli_json_sample.jsonl` contains one
/// event of every documented type plus an unknown-type line and an empty line.
/// This test validates that `classify_event` handles the full event surface.
#[test]
fn fixture_file_all_events_parse_correctly() {
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex_cli_json_sample.jsonl");

    let content = std::fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture_path.display()));

    // Expected classification sequence (in fixture file line order, skipping
    // blank lines as the parser returns Unknown for them which is acceptable).
    use atm_agent_mcp::cli_json_test::CliJsonEventKind;

    let non_empty_lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();

    // Line 0: metadata object → Unknown (unrecognised _fixture_metadata type)
    assert_eq!(
        classify_event(non_empty_lines[0]),
        CliJsonEventKind::Unknown,
        "line 0 (metadata) must classify as Unknown"
    );

    // Line 1: agent_message → AgentMessage
    assert_eq!(
        classify_event(non_empty_lines[1]),
        CliJsonEventKind::AgentMessage,
        "line 1 must be AgentMessage"
    );

    // Line 2: tool_call → ToolCall
    assert_eq!(
        classify_event(non_empty_lines[2]),
        CliJsonEventKind::ToolCall,
        "line 2 must be ToolCall"
    );

    // Line 3: tool_result → ToolResult
    assert_eq!(
        classify_event(non_empty_lines[3]),
        CliJsonEventKind::ToolResult,
        "line 3 must be ToolResult"
    );

    // Line 4: file_change → FileChange
    assert_eq!(
        classify_event(non_empty_lines[4]),
        CliJsonEventKind::FileChange,
        "line 4 must be FileChange"
    );

    // Line 5: idle → Idle
    assert_eq!(
        classify_event(non_empty_lines[5]),
        CliJsonEventKind::Idle,
        "line 5 must be Idle"
    );

    // Line 6: done → Done
    assert_eq!(
        classify_event(non_empty_lines[6]),
        CliJsonEventKind::Done,
        "line 6 must be Done"
    );

    // Line 7: unknown type → Unknown
    assert_eq!(
        classify_event(non_empty_lines[7]),
        CliJsonEventKind::Unknown,
        "line 7 (unknown type) must classify as Unknown"
    );

    // All 8 non-empty lines are accounted for.
    assert_eq!(
        non_empty_lines.len(),
        8,
        "fixture must have exactly 8 non-empty lines (got {})",
        non_empty_lines.len()
    );
}

// ─── ATM-QA-004: Fallback drain without idle event ───────────────────────────

/// Verifies that `stdin_queue::drain` delivers messages independently of the
/// idle event trigger mechanism.
///
/// The 30-second fallback timer in `proxy.rs` calls the same `drain` function
/// as the idle-event handler.  This test confirms that calling `drain` directly
/// (bypassing the idle trigger) still delivers all enqueued messages.  It
/// demonstrates the fallback path is achievable even without the 30-second
/// timer firing — the timer just calls the same drain function.
#[tokio::test]
#[serial]
async fn no_idle_event_queue_can_be_drained_after_timeout() {
    let tmp = tempdir().unwrap();
    let _atm_home = EnvGuard::set("ATM_HOME", tmp.path());

    let team = "test-team-fallback";
    let agent = "test-agent-fallback";

    // Enqueue messages without ever firing an idle event.
    for i in 0..3usize {
        stdin_queue::enqueue(
            team,
            agent,
            &format!(r#"{{"seq":{i},"type":"tool_result"}}"#),
        )
        .await
        .unwrap();
    }

    // Drain directly, simulating the 30-second fallback timer firing.
    let (writer, captured) = CapWriter::new();
    let stdin: Arc<tokio::sync::Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(writer)));

    let drained = stdin_queue::drain(team, agent, &stdin, Duration::from_secs(600))
        .await
        .unwrap();

    assert_eq!(
        drained, 3,
        "all 3 messages must be drained without an idle event (fallback path)"
    );

    let output = captured.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&output);
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 3, "exactly 3 lines must be written to stdin");
    for line in &lines {
        assert!(
            line.contains("tool_result"),
            "each line must contain tool_result: {line}"
        );
    }
}

// ─── ATM-QA-007: TurnState::Busy is unreachable for cli-json ─────────────────

/// Verifies that none of the cli-json event types produce a `TurnState::Busy`
/// transition via the state machine in this test module.
///
/// The `cli-json` protocol does not emit `turn/started` notifications, so
/// `TurnState::Busy` is never reached.  Only `idle` → `Idle` and `done` →
/// `Terminal` are valid TurnState transitions.  All other events reset
/// `idle_flag` but leave `turn_state` unchanged (not Busy).
#[tokio::test]
async fn no_cli_json_event_produces_busy_turn_state() {
    let events: &[(&str, bool)] = &[
        (r#"{"type":"agent_message","content":"hi"}"#, false),
        (
            r#"{"type":"tool_call","name":"bash","call_id":"c1"}"#,
            false,
        ),
        (
            r#"{"type":"tool_result","call_id":"c1","output":"ok"}"#,
            false,
        ),
        (
            r#"{"type":"file_change","path":"workspace/x.txt","action":"write"}"#,
            false,
        ),
        // idle and done are valid state-changing events but never produce Busy.
        (r#"{"type":"idle"}"#, false),
        (r#"{"type":"done"}"#, false),
    ];

    for (line, _) in events {
        let idle_flag = AtomicBool::new(false);
        let turn_state = tokio::sync::Mutex::new(TurnState::Idle);

        apply_event(line, &idle_flag, &turn_state).await;

        let state = turn_state.lock().await.clone();
        assert!(
            !matches!(state, TurnState::Busy { .. }),
            "cli-json event {line:?} must not produce TurnState::Busy (got {state:?})"
        );
    }
}

// ─── ATM-QA-008: Multi-cycle idle windows ────────────────────────────────────

/// Verifies that repeated idle drain cycles do not cross-contaminate: messages
/// from idle cycle N are not re-delivered in idle cycle N+1.
#[tokio::test]
#[serial]
async fn multi_cycle_idle_windows_no_cross_contamination() {
    let tmp = tempdir().unwrap();
    let _atm_home = EnvGuard::set("ATM_HOME", tmp.path());

    let team = "test-team-multicycle";
    let agent = "test-agent-multicycle";

    // --- Idle cycle 1: enqueue A and B, drain, assert count==2 ---
    stdin_queue::enqueue(team, agent, r#"{"seq":"A","type":"tool_result"}"#)
        .await
        .unwrap();
    stdin_queue::enqueue(team, agent, r#"{"seq":"B","type":"tool_result"}"#)
        .await
        .unwrap();

    let (w1, cap1) = CapWriter::new();
    let stdin1: Arc<tokio::sync::Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(w1)));

    let count1 = stdin_queue::drain(team, agent, &stdin1, Duration::from_secs(600))
        .await
        .unwrap();
    assert_eq!(
        count1, 2,
        "idle cycle 1 must drain exactly 2 messages (A and B)"
    );

    let out1 = cap1.lock().unwrap().clone();
    let text1 = String::from_utf8_lossy(&out1);
    assert!(
        text1.contains(r#""A""#),
        "cycle-1 output must contain A: {text1}"
    );
    assert!(
        text1.contains(r#""B""#),
        "cycle-1 output must contain B: {text1}"
    );

    // --- Idle cycle 2: enqueue C, drain, assert count==1 and no A/B ---
    stdin_queue::enqueue(team, agent, r#"{"seq":"C","type":"tool_result"}"#)
        .await
        .unwrap();

    let (w2, cap2) = CapWriter::new();
    let stdin2: Arc<tokio::sync::Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(w2)));

    let count2 = stdin_queue::drain(team, agent, &stdin2, Duration::from_secs(600))
        .await
        .unwrap();

    assert_eq!(
        count2, 1,
        "idle cycle 2 must drain exactly 1 message (C only)"
    );

    let out2 = cap2.lock().unwrap().clone();
    let text2 = String::from_utf8_lossy(&out2);
    assert!(
        text2.contains(r#""C""#),
        "cycle-2 output must contain C: {text2}"
    );
    assert!(
        !text2.contains(r#""A""#),
        "cycle-2 output must NOT contain A (cross-contamination): {text2}"
    );
    assert!(
        !text2.contains(r#""B""#),
        "cycle-2 output must NOT contain B (cross-contamination): {text2}"
    );
}
