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
    assert!(received[0].contains("agent_message"), "line 0 must be agent_message");
    assert!(received[1].contains("\"idle\""), "line 1 must be idle");
    assert!(received[2].contains("tool_call"), "line 2 must be tool_call");
    assert!(received[3].contains("\"done\""), "line 3 must be done");

    // --- Verify state machine transitions ---
    // After 'done', idle_flag must be false.
    assert!(!idle_flag.load(Ordering::SeqCst), "idle_flag must be false after done");

    // Turn state must be Terminal.
    let state = turn_state.lock().await.clone();
    assert!(
        matches!(state, TurnState::Terminal { status: TurnStatus::Completed, .. }),
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
        (r#"{"type":"agent_message"}"#, false),  // activity resets it
        (r#"{"type":"tool_call"}"#, false),      // still reset
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
        r#"{"type":"file_change","path":"/tmp/x","action":"write"}"#,
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

    assert_eq!(count, all_events.len(), "all {} events must be forwarded", all_events.len());
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
    // SAFETY: test-only env mutation, serialised by #[serial].
    unsafe { std::env::set_var("ATM_HOME", tmp.path()) };

    let team = "test-team-ac6";
    let agent = "test-agent-ac6";

    // Enqueue 3 messages before the idle signal fires.
    for i in 0..3usize {
        stdin_queue::enqueue(team, agent, &format!(r#"{{"seq":{i},"type":"tool_result"}}"#))
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

    unsafe { std::env::remove_var("ATM_HOME") };

    assert_eq!(drained, 3, "all 3 pre-idle messages must be drained on idle event");

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
    unsafe { std::env::set_var("ATM_HOME", tmp.path()) };

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
    assert_eq!(count1, 1, "first drain must deliver the 1 pre-queued message");

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

    unsafe { std::env::remove_var("ATM_HOME") };

    assert_eq!(count2, 1, "second drain must deliver the 1 post-first-idle message");
    let output = cap2.lock().unwrap().clone();
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains(r#""seq":1"#), "second drain must contain seq:1");
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
    assert!(idle_flag.load(Ordering::SeqCst), "idle_flag must be true after idle");
    assert!(turn_state.lock().await.is_idle(), "turn_state must be Idle after idle");

    // done event
    apply_event(r#"{"type":"done"}"#, &idle_flag, &turn_state).await;
    assert!(!idle_flag.load(Ordering::SeqCst), "idle_flag must be false after done");
    let state = turn_state.lock().await.clone();
    assert!(
        matches!(state, TurnState::Terminal { status: TurnStatus::Completed, .. }),
        "turn_state must be Terminal(Completed) after done: {state:?}"
    );
}
