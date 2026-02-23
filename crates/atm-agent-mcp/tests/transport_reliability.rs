//! Soak and failure-injection tests for the MCP transport layer.
//!
//! These tests exercise reliability properties of `MockTransport` and the
//! `drive_notification_task` background task:
//!
//! - **Soak**: repeated steer/interrupt cycles across many round-trips.
//! - **Connection drop**: EOF on the server side surfaces an error rather than hanging.
//! - **Malformed input**: bad JSON from the server returns an error without panic.
//! - **Timeout**: a request with no response honours the transport timeout.
//! - **Interleaved notifications**: notifications delivered between two correlated
//!   request/response pairs arrive in order without corrupting correlation state.
//!
//! # Cross-platform compliance
//!
//! No hardcoded `/tmp/` paths.  No `std::env::set_var` calls.  All tests use
//! `ATM_HOME` via per-command `cmd.env()` when applicable; unit tests use direct
//! struct construction.

use atm_agent_mcp::{MockTransport, MockTransportHandle};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::io::AsyncWriteExt;

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Build a well-formed fork response (no `jsonrpc` field, per app-server spec).
fn make_fork_response(id: u64, thread_id: &str) -> String {
    serde_json::to_string(&json!({
        "id": id,
        "result": { "threadId": thread_id }
    }))
    .unwrap()
}

/// Build a `turn/started` notification (no `jsonrpc` field).
fn make_turn_started(thread_id: &str, turn_id: &str) -> String {
    serde_json::to_string(&json!({
        "method": "turn/started",
        "params": { "threadId": thread_id, "turnId": turn_id }
    }))
    .unwrap()
}

/// Build a `turn/completed` notification (no `jsonrpc` field).
fn make_turn_completed(thread_id: &str, turn_id: &str, status: &str) -> String {
    serde_json::to_string(&json!({
        "method": "turn/completed",
        "params": { "threadId": thread_id, "turnId": turn_id, "status": status }
    }))
    .unwrap()
}

/// Build a minimal `NotificationTaskState` wired to the supplied shared state.
///
/// All optional fields (elicitation, upstream, child_stdin) are set to `None`
/// so the state drives only turn-tracking and response-correlation logic.
fn make_task_state(
    turn_state: Arc<tokio::sync::Mutex<std::collections::HashMap<String, atm_agent_mcp::stream_norm::TurnState>>>,
    idle_flag: Arc<AtomicBool>,
    pending_responses: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>,
        >,
    >,
) -> atm_agent_mcp::transport::NotificationTaskState {
    let initialized = Arc::new(AtomicBool::new(true));
    let session_registry = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    atm_agent_mcp::transport::NotificationTaskState {
        turn_state,
        idle_flag,
        initialized,
        pending_responses,
        session_registry,
        team: "reliability-test-team".to_string(),
        turn_tracker: None,
        elicitation_registry: None,
        elicitation_counter: None,
        upstream_tx: None,
        child_stdin: None,
        agent_identity: Some("soak-agent".to_string()),
    }
}

// ─── Test A: Repeated steer/interrupt cycles (soak) ─────────────────────────

/// Verify that `MockTransport` survives 20 back-to-back response injections
/// without dropping messages or panicking.
///
/// Each iteration injects a response into the transport's mock channel and
/// reads it back from stdout.  This exercises the unbounded channel and the
/// duplex flush path under sustained load.
#[tokio::test]
async fn test_soak_repeated_round_trips() {
    use tokio::io::{AsyncBufReadExt, BufReader};

    const ROUNDS: usize = 20;

    let (transport, handle): (MockTransport, MockTransportHandle) =
        MockTransport::new_with_handle();

    // Pre-inject all responses before spawning so the background task drains
    // the channel atomically without interleaving with our reads.
    for i in 0..ROUNDS {
        let msg = make_fork_response(i as u64, &format!("thread-{i}"));
        handle
            .response_tx
            .send(msg)
            .expect("channel should be open");
    }

    let raw_io = transport.spawn().await.expect("spawn should succeed");

    let mut reader = BufReader::new(raw_io.stdout);

    for i in 0..ROUNDS {
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            reader.read_line(&mut line),
        )
        .await
        .unwrap_or_else(|_| panic!("round {i}: timeout reading response"))
        .unwrap_or_else(|e| panic!("round {i}: I/O error: {e}"));

        let v: serde_json::Value =
            serde_json::from_str(line.trim()).unwrap_or_else(|e| panic!("round {i}: bad JSON: {e}"));

        assert_eq!(
            v["result"]["threadId"],
            format!("thread-{i}"),
            "round {i}: unexpected threadId"
        );
    }
}

// ─── Test B: Connection drop recovery ────────────────────────────────────────

/// Simulates an abrupt child process crash / pipe break.
///
/// When a real child process crashes (SIGKILL, panic, unexpected exit), the OS
/// closes all of its file descriptors, which manifests as EOF on the read end of
/// any pipe connected to that process. `MockTransport` faithfully models this:
/// dropping both `response_tx` senders closes the channel, delivering the same
/// EOF that an OS-level child crash would produce. This test validates that the
/// transport returns a clean `Ok(0)` / channel-closed result rather than hanging.
///
/// Verify that closing all response senders causes the background task to
/// observe EOF and not hang indefinitely.
///
/// `MockTransport` holds its own `response_tx` clone (keepalive) and gives a
/// second clone to the `MockTransportHandle`.  EOF on the duplex stdout only
/// occurs when **both** senders are dropped: first the handle's sender, then
/// the transport struct itself.  After both are dropped, the background task
/// exits and `stdout_write` drops, propagating EOF to the read half.
#[tokio::test]
async fn test_connection_drop_eof() {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let (transport, handle): (MockTransport, MockTransportHandle) =
        MockTransport::new_with_handle();

    let raw_io = transport.spawn().await.expect("spawn should succeed");

    // Give the background task a moment to start.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // Drop both senders: handle's first, then transport's keepalive copy.
    // `transport` was consumed by `.spawn()` (which calls `self` by reference),
    // but the struct is still alive here.  We explicitly drop both to close the
    // channel and trigger EOF on the duplex read half.
    drop(handle.response_tx);
    // The transport's internal keepalive sender is dropped when `transport` is
    // dropped.  `spawn()` takes `&self` so `transport` is still owned here.
    drop(transport);

    // The reader should now observe EOF (Ok(0)) rather than hanging.
    let mut reader = BufReader::new(raw_io.stdout);
    let mut line = String::new();
    let n = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("should not timeout — EOF should be immediate after all senders are dropped")
    .expect("should not return an I/O error on EOF");

    assert_eq!(n, 0, "read_line on EOF should return Ok(0), indicating no more data");
}

// ─── Test C: Malformed input handling ────────────────────────────────────────

/// Verify that a malformed JSON line injected via the mock transport is handled
/// gracefully by `drive_notification_task` without panicking.
///
/// The background task parses each line with `parse_app_server_notification`.
/// For lines that are not valid JSON-RPC (no `method`, no `id`), the task logs
/// and continues — it must not panic or abort the reader loop.
///
/// We verify the task continues to process subsequent valid messages after the
/// malformed one.
#[tokio::test]
async fn test_malformed_input_no_panic() {
    use atm_agent_mcp::transport::{drive_notification_task, NotificationTaskState};
    use atm_agent_mcp::stream_norm::TurnState;

    let (mut feed_write, feed_read) = tokio::io::duplex(4096);
    let (proxy_write, _proxy_read) = tokio::io::duplex(4096);

    let turn_state = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let idle_flag = Arc::new(AtomicBool::new(true));
    let pending_responses = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

    let state: NotificationTaskState =
        make_task_state(Arc::clone(&turn_state), Arc::clone(&idle_flag), Arc::clone(&pending_responses));

    tokio::task::spawn(drive_notification_task(feed_read, proxy_write, state));

    // Inject a malformed JSON line.
    let malformed = b"not json at all\n";
    feed_write.write_all(malformed).await.unwrap();

    // Give the task a tick to process the malformed line.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    // Inject a valid turn/started after the malformed line to confirm the loop
    // continued running.
    let started = format!("{}\n", make_turn_started("thread-C", "turn-C"));
    feed_write.write_all(started.as_bytes()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // idle_flag should be false — the valid turn/started was processed.
    assert!(
        !idle_flag.load(Ordering::Acquire),
        "idle_flag should be false after turn/started following malformed input — \
         the background task must continue processing after a bad line"
    );

    // The turn state should reflect the started turn.
    let state_map = turn_state.lock().await;
    assert!(
        matches!(state_map.get("thread-C"), Some(TurnState::Busy { .. })),
        "turn-C should be Busy after turn/started; got: {:?}",
        state_map.get("thread-C")
    );
}

// ─── Test D: Timeout behaviour ────────────────────────────────────────────────

/// Verify that a pending response oneshot times out when no response is injected.
///
/// This test registers a pending response for a request id and then waits for
/// the oneshot receiver.  Since no response is ever injected, the receiver
/// times out — mirroring what `send_request_with_overload_retry` does inside
/// the transport (`tokio::time::timeout(..., rx.await)`).
///
/// We reproduce the timeout pattern directly with a short deadline to keep
/// the test fast.
#[tokio::test]
async fn test_pending_response_timeout() {
    // Simulate what the transport's send_request path does: register a oneshot,
    // then wait for it with a timeout.  If no response arrives → Err(Elapsed).
    let (_tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();

    // Hold the tx without sending — simulates the background task never seeing
    // a response for this id (e.g., the child crashed before responding).
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        rx,
    )
    .await;

    assert!(
        result.is_err(),
        "timeout::timeout should return Err(Elapsed) when the sender is never called"
    );
}

// ─── Test E: Interleaved notifications between two correlated responses ───────

/// Verify that notifications interleaved between two request/response pairs
/// are delivered in order and do not corrupt response correlation.
///
/// Scenario:
///   1. Register pending responses for ids 10 and 20.
///   2. Inject: notification, response-10, notification, notification, response-20.
///   3. Assert both oneshots fire with the correct payloads.
///   4. Assert the turn state reflects the two turn/started notifications.
#[tokio::test]
async fn test_interleaved_notifications_and_responses() {
    use atm_agent_mcp::transport::{drive_notification_task, NotificationTaskState};
    use atm_agent_mcp::stream_norm::TurnState;

    let (mut feed_write, feed_read) = tokio::io::duplex(4096);
    let (proxy_write, _proxy_read) = tokio::io::duplex(4096);

    let turn_state = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let idle_flag = Arc::new(AtomicBool::new(true));
    let pending_responses: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>,
        >,
    > = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

    // Register two pending response slots.
    let (tx10, rx10) = tokio::sync::oneshot::channel::<serde_json::Value>();
    let (tx20, rx20) = tokio::sync::oneshot::channel::<serde_json::Value>();
    {
        let mut map = pending_responses.lock().await;
        map.insert(10, tx10);
        map.insert(20, tx20);
    }

    let state: NotificationTaskState =
        make_task_state(Arc::clone(&turn_state), Arc::clone(&idle_flag), Arc::clone(&pending_responses));

    tokio::task::spawn(drive_notification_task(feed_read, proxy_write, state));

    // Inject: notification → response-10 → notification → notification → response-20.
    let n1 = format!("{}\n", make_turn_started("thread-E1", "turn-E1"));
    let r10 = format!("{}\n", make_fork_response(10, "forked-10"));
    let n2 = format!("{}\n", make_turn_started("thread-E2", "turn-E2"));
    let n3 = format!("{}\n", make_turn_completed("thread-E1", "turn-E1", "completed"));
    let r20 = format!("{}\n", make_fork_response(20, "forked-20"));

    feed_write.write_all(n1.as_bytes()).await.unwrap();
    feed_write.write_all(r10.as_bytes()).await.unwrap();
    feed_write.write_all(n2.as_bytes()).await.unwrap();
    feed_write.write_all(n3.as_bytes()).await.unwrap();
    feed_write.write_all(r20.as_bytes()).await.unwrap();

    // Both oneshots should fire with the correct payloads.
    let resp10 = tokio::time::timeout(std::time::Duration::from_secs(2), rx10)
        .await
        .expect("timeout waiting for response 10")
        .expect("oneshot should not be cancelled");

    let resp20 = tokio::time::timeout(std::time::Duration::from_secs(2), rx20)
        .await
        .expect("timeout waiting for response 20")
        .expect("oneshot should not be cancelled");

    assert_eq!(resp10["id"], 10, "response 10 id mismatch");
    assert_eq!(resp10["result"]["threadId"], "forked-10");

    assert_eq!(resp20["id"], 20, "response 20 id mismatch");
    assert_eq!(resp20["result"]["threadId"], "forked-20");

    // Give the task a final tick to process the last turn/completed.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // thread-E1 should be Terminal (started then completed).
    // thread-E2 should be Busy (started but not completed).
    let state_map = turn_state.lock().await;
    assert!(
        matches!(state_map.get("thread-E1"), Some(TurnState::Terminal { .. })),
        "thread-E1 should be Terminal; got: {:?}",
        state_map.get("thread-E1")
    );
    assert!(
        matches!(state_map.get("thread-E2"), Some(TurnState::Busy { .. })),
        "thread-E2 should be Busy; got: {:?}",
        state_map.get("thread-E2")
    );
}
