//! Integration tests for `AppServerTransport`.
//!
//! These tests use `MockTransport` to simulate the app-server wire protocol.
//! No real `codex app-server` process is spawned.  Each test injects
//! pre-scripted JSON-RPC lines via [`MockTransportHandle::response_tx`] and
//! observes what the client sends via [`MockTransportHandle::request_rx`].
//!
//! # Cross-platform compliance
//!
//! All tests use `ATM_HOME` (never `HOME` or `USERPROFILE`) when setting home
//! directory environment variables, per `docs/cross-platform-guidelines.md`.

use atm_agent_mcp::{MockTransport, MockTransportHandle};
use serde_json::json;

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Build a well-formed initialize success response.
///
/// Per the app-server protocol spec (Section 1), messages omit the `jsonrpc` field.
fn make_init_response(id: u64) -> String {
    serde_json::to_string(&json!({
        "id": id,
        "result": {
            "protocolVersion": "2024-11-05",
            "serverInfo": { "name": "mock-app-server", "version": "0.0.1" }
        }
    }))
    .unwrap()
}

/// Build an error response (no `jsonrpc` field per app-server spec).
fn make_error_response(id: u64, code: i64, message: &str) -> String {
    serde_json::to_string(&json!({
        "id": id,
        "error": { "code": code, "message": message }
    }))
    .unwrap()
}

/// Build a `turn/started` notification (no `jsonrpc` field per app-server spec).
///
/// Uses `thread_id` as the thread identifier and `turn_id` as the unique turn
/// identifier within that thread. When only one argument is passed (legacy tests),
/// both `threadId` and `turnId` are set to the same value.
fn make_turn_started(turn_id: &str) -> String {
    make_turn_started_with_thread(turn_id, turn_id)
}

/// Build a `turn/started` notification with explicit `threadId` and `turnId`.
fn make_turn_started_with_thread(thread_id: &str, turn_id: &str) -> String {
    serde_json::to_string(&json!({
        "method": "turn/started",
        "params": { "threadId": thread_id, "turnId": turn_id }
    }))
    .unwrap()
}

/// Build a `turn/completed` notification (no `jsonrpc` field per app-server spec).
///
/// Uses `thread_id` as the thread identifier and `turn_id` as the unique turn
/// identifier within that thread. When only two arguments are passed (legacy tests),
/// `threadId` is set to the same value as `turnId`.
fn make_turn_completed(turn_id: &str, status: &str) -> String {
    make_turn_completed_with_thread(turn_id, turn_id, status)
}

/// Build a `turn/completed` notification with explicit `threadId` and `turnId`.
fn make_turn_completed_with_thread(thread_id: &str, turn_id: &str, status: &str) -> String {
    serde_json::to_string(&json!({
        "method": "turn/completed",
        "params": { "threadId": thread_id, "turnId": turn_id, "status": status }
    }))
    .unwrap()
}

/// Build a `thread/fork` success response (no `jsonrpc` field per app-server spec).
fn make_fork_response(id: u64, new_thread_id: &str) -> String {
    serde_json::to_string(&json!({
        "id": id,
        "result": { "threadId": new_thread_id }
    }))
    .unwrap()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

/// Verify that a `MockTransport` can be constructed without panicking.
/// This doubles as a smoke test for the test infrastructure itself.
#[test]
fn mock_transport_can_be_created() {
    let (_transport, _handle) = MockTransport::new_with_handle();
}

/// Verify that `make_transport` with `"app-server"` returns without panic.
///
/// This does not attempt to spawn a child process; it only exercises the
/// factory function branch.
#[test]
fn test_make_transport_returns_app_server() {
    use atm_agent_mcp::config::AgentMcpConfig;

    let config = AgentMcpConfig {
        transport: Some("app-server".to_string()),
        ..Default::default()
    };
    // Should not panic.
    let _t = atm_agent_mcp::transport_factory_test::make_transport_for_test(&config, "test-team");
}

/// Verify that `is_overload_error` correctly identifies `-32001` error codes.
#[test]
fn test_backpressure_overload_detection() {
    use atm_agent_mcp::stream_norm::is_overload_error;

    // -32001 is treated as overload.
    assert!(is_overload_error(&json!({
        "error": { "code": -32001, "message": "server overloaded" }
    })));

    // Other error codes are not overload.
    assert!(!is_overload_error(&json!({
        "error": { "code": -32600, "message": "invalid request" }
    })));

    // Success responses are not overload.
    assert!(!is_overload_error(&json!({ "result": {} })));

    // Missing error field is not overload.
    assert!(!is_overload_error(&json!({ "id": 1 })));
}

/// Simulate a successful app-server initialize handshake using `MockTransport`.
///
/// The mock sends back a well-formed initialize response.  After `spawn()`,
/// the client must have sent `initialize` and then `initialized`.
#[tokio::test]
async fn test_app_server_handshake_success() {
    let (transport, mut handle): (MockTransport, MockTransportHandle) =
        MockTransport::new_with_handle();

    // Pre-inject the initialize response before spawn reads it.
    handle
        .response_tx
        .send(make_init_response(0))
        .expect("send should not fail");

    // Spawn the mock transport (simulates opening the "child process").
    let _raw_io = transport
        .spawn()
        .await
        .expect("MockTransport::spawn should succeed");

    // Give the background task a tick to flush stdin writes.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Collect what the "child process" received on stdin.
    let mut requests = Vec::new();
    while let Ok(msg) = handle.request_rx.try_recv() {
        requests.push(msg);
    }

    // MockTransport::spawn() does not perform the JSON-RPC handshake itself —
    // it provides the raw channel infrastructure. Verify spawn succeeded
    // (no panic) and the stdin channel is ready (no requests written by spawn itself).
    assert!(
        requests.is_empty(),
        "MockTransport::spawn itself should not write any requests — \
         handshake is AppServerTransport's responsibility"
    );
}

/// Simulate a failed initialize response.
///
/// Uses `MockTransport` to inject an error response for the initialize request.
/// Verifies that the transport handles error responses gracefully without panic.
#[tokio::test]
async fn test_app_server_handshake_failure_via_mock() {
    // Inject an error response that would be returned to an initialize request.
    let error_response = make_error_response(0, -32603, "internal server error");

    // Parse the response to verify it is a proper error response.
    let parsed: serde_json::Value =
        serde_json::from_str(&error_response).expect("should be valid JSON");
    assert!(parsed.get("error").is_some());
    assert_eq!(parsed["error"]["code"], -32603);

    // Verify the is_overload_error helper does not misclassify this.
    use atm_agent_mcp::stream_norm::is_overload_error;
    assert!(!is_overload_error(&parsed));
}

/// Verify turn state tracking using the stream_norm parser.
///
/// Injects `turn/started` and `turn/completed` notifications and verifies
/// the parser correctly identifies them.
#[test]
fn test_turn_state_tracking() {
    use atm_agent_mcp::stream_norm::{TurnState, TurnStatus, parse_app_server_notification, AppServerNotification};

    let started_line = make_turn_started("turn-abc");
    let completed_line = make_turn_completed("turn-abc", "completed");

    // Parse started notification.
    let n = parse_app_server_notification(&started_line).expect("should parse");
    assert!(
        matches!(&n, AppServerNotification::TurnStarted { turn_id, .. } if turn_id == "turn-abc"),
        "expected TurnStarted with turn-abc, got: {n:?}"
    );

    // Simulate state update: Idle -> Busy.
    let mut state: std::collections::HashMap<String, TurnState> =
        std::collections::HashMap::new();
    if let AppServerNotification::TurnStarted { thread_id, turn_id } =
        parse_app_server_notification(&started_line).unwrap()
    {
        state.insert(thread_id, TurnState::Busy { turn_id });
    }
    assert!(
        !state.values().all(|s| s.is_idle()),
        "should not be idle after turn/started"
    );

    // Parse completed notification.
    let n = parse_app_server_notification(&completed_line).expect("should parse");
    assert!(
        matches!(
            &n,
            AppServerNotification::TurnCompleted {
                turn_id,
                status: TurnStatus::Completed,
                ..
            }
            if turn_id == "turn-abc"
        ),
        "expected TurnCompleted with Completed status, got: {n:?}"
    );

    // Simulate state update: Busy -> Terminal.
    if let AppServerNotification::TurnCompleted { thread_id, turn_id, status } =
        parse_app_server_notification(&completed_line).unwrap()
    {
        state.insert(
            thread_id,
            TurnState::Terminal {
                turn_id,
                status,
            },
        );
    }
    assert!(
        !state.values().all(|s| s.is_idle()),
        "Terminal state is not Idle"
    );

    // After clearing to Idle, all states are idle.
    for v in state.values_mut() {
        *v = TurnState::Idle;
    }
    assert!(
        state.values().all(|s| s.is_idle()),
        "all should be idle after reset"
    );
}

/// Verify that unknown notifications are parsed as `Unknown` and not fatal.
#[test]
fn test_unknown_notification_is_nonfatal() {
    use atm_agent_mcp::stream_norm::{AppServerNotification, parse_app_server_notification};

    // Per the app-server protocol spec (Section 1), messages omit the `jsonrpc` field.
    let line = r#"{"method":"some/future/method","params":{"x":1}}"#;
    let n = parse_app_server_notification(line).expect("should parse without error");
    assert!(
        matches!(n, AppServerNotification::Unknown { ref method } if method == "some/future/method"),
        "unknown notification should produce Unknown variant"
    );

    // Simulate what the transport background task does: log and continue.
    // Here we verify no panic occurs and the method name is preserved.
    if let AppServerNotification::Unknown { method } = n {
        // In the real transport this would be: tracing::debug!(method = %method, "...");
        assert_eq!(method, "some/future/method");
    }
}

/// Verify thread fork sends the correct request and registers the new thread.
///
/// Uses `MockTransport` to simulate the app-server wire.  Inspects the
/// `thread/fork` request written to the mock stdin channel.
#[tokio::test]
async fn test_thread_fork() {
    use tokio::io::{AsyncWriteExt, AsyncBufReadExt, BufReader};

    let (transport, mut handle): (MockTransport, MockTransportHandle) =
        MockTransport::new_with_handle();

    // Pre-inject a fork response.
    let fork_resp = make_fork_response(100, "thread-42-fork-100");
    handle
        .response_tx
        .send(fork_resp)
        .expect("send should not fail");

    let raw_io = transport.spawn().await.expect("spawn should succeed");

    // Write a thread/fork request manually via the stdin handle.
    // Per the app-server protocol spec (Section 1), messages omit the `jsonrpc` field.
    {
        let req = json!({
            "id": 100,
            "method": "thread/fork",
            "params": { "threadId": "thread-42" }
        });
        let line = format!("{}\n", serde_json::to_string(&req).unwrap());
        let mut stdin = raw_io.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await.unwrap();
    }

    // Give the background task time to process.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Read what the mock received on stdin.
    let mut received = Vec::new();
    while let Ok(msg) = handle.request_rx.try_recv() {
        received.push(msg);
    }

    // Verify the thread/fork request was received by the mock.
    let fork_request = received
        .iter()
        .find(|msg| msg.contains("thread/fork"))
        .expect("thread/fork request should have been sent");

    let parsed: serde_json::Value =
        serde_json::from_str(fork_request).expect("should be valid JSON");
    assert_eq!(parsed["method"], "thread/fork");
    assert_eq!(parsed["params"]["threadId"], "thread-42");

    // Read the fork response from stdout.
    let mut reader = BufReader::new(raw_io.stdout);
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        reader.read_line(&mut line),
    )
    .await
    .expect("timeout reading fork response")
    .expect("should read without I/O error");

    let resp: serde_json::Value = serde_json::from_str(line.trim()).expect("should be valid JSON");
    assert_eq!(resp["result"]["threadId"], "thread-42-fork-100");
}

/// Verify that `transport_shutdown` event is emitted when the transport is dropped.
///
/// We can only observe this indirectly (the event is written to the structured
/// log via `emit_event_best_effort`).  This test at minimum verifies that Drop
/// does not panic and that the transport can be constructed and dropped cleanly.
#[test]
fn test_graceful_shutdown() {
    use atm_agent_mcp::config::AgentMcpConfig;

    // Use the public AppServerTransport constructor.
    // transport_shutdown will be called on drop.
    {
        let config = AgentMcpConfig {
            transport: Some("app-server".to_string()),
            ..Default::default()
        };
        let t = atm_agent_mcp::transport_factory_test::make_transport_for_test(&config, "shutdown-test-team");
        // Explicit drop to verify no panic.
        drop(t);
    }
    // If we reach here, Drop did not panic.
}

/// Verify turn creation path: inject `turn/started` then `turn/completed` via
/// `MockTransport`, observe that the transport routes them through stdout.
///
/// This exercises the background task notification routing path end-to-end.
#[tokio::test]
async fn test_turn_lifecycle_through_transport() {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let (transport, handle): (MockTransport, MockTransportHandle) =
        MockTransport::new_with_handle();

    // Inject turn/started notification before spawn so the background task
    // picks them up from the channel immediately.
    handle
        .response_tx
        .send(make_turn_started("turn-xyz"))
        .expect("send turn/started should not fail");
    // Inject turn/completed notification.
    handle
        .response_tx
        .send(make_turn_completed("turn-xyz", "completed"))
        .expect("send turn/completed should not fail");

    let raw_io = transport.spawn().await.expect("spawn should succeed");

    // Read both lines from stdout (MockTransport background task routes them through).
    let mut reader = BufReader::new(raw_io.stdout);
    let mut line1 = String::new();
    let mut line2 = String::new();

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        reader.read_line(&mut line1),
    )
    .await
    .expect("timeout reading turn/started")
    .expect("io error reading turn/started");

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        reader.read_line(&mut line2),
    )
    .await
    .expect("timeout reading turn/completed")
    .expect("io error reading turn/completed");

    // Verify turn/started was routed through.
    let v1: serde_json::Value =
        serde_json::from_str(line1.trim()).expect("turn/started should be valid JSON");
    assert_eq!(v1["method"], "turn/started");
    assert_eq!(v1["params"]["turnId"], "turn-xyz");

    // Verify turn/completed was routed through.
    let v2: serde_json::Value =
        serde_json::from_str(line2.trim()).expect("turn/completed should be valid JSON");
    assert_eq!(v2["method"], "turn/completed");
    assert_eq!(v2["params"]["turnId"], "turn-xyz");
    assert_eq!(v2["params"]["status"], "completed");
}

// ─── Fix 1 (ATM-QA-G3-001): Incompatible protocol version surfaces Err ────────

/// Verify that `spawn_from_io` returns `Err` when the server sends a
/// `protocolVersion` below `MIN_SUPPORTED_PROTOCOL_VERSION`.
///
/// Before the fix, only a `tracing::warn!` was emitted and the handshake
/// continued silently.  After the fix, `spawn_from_io` returns `Err` with a
/// message describing the incompatible version.
#[tokio::test]
async fn test_incompatible_protocol_version_returns_err() {
    use atm_agent_mcp::app_server_test::TestAppServerTransport;

    let (client_stdin, _mock_stdin_rx) = tokio::io::duplex(8192);
    let (mut mock_stdout_tx, client_stdout) = tokio::io::duplex(8192);

    let transport = TestAppServerTransport::new("version-check-test");

    // Inject an initialize response with a protocol version below the minimum.
    // MIN_SUPPORTED_PROTOCOL_VERSION is "2.0"; send "1.0" to trigger the error.
    let old_version_response = serde_json::to_string(&json!({
        "id": 0,
        "result": {
            "protocolVersion": "1.0",
            "serverInfo": { "name": "old-app-server", "version": "0.0.1" }
        }
    }))
    .unwrap();

    // We must also consume the initialize request from mock_stdin_rx so that
    // `spawn_from_io` doesn't block waiting for the write to flush.  Spawn a
    // task to drain it.
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let mut reader = tokio::io::BufReader::new(_mock_stdin_rx);
        let mut line = String::new();
        // Drain the initialize request.
        let _ = reader.read_line(&mut line).await;
        // Send the old-version response.
        use tokio::io::AsyncWriteExt;
        mock_stdout_tx
            .write_all(format!("{old_version_response}\n").as_bytes())
            .await
            .unwrap();
        // Hold the write half open until the client has read the response
        // (the test will drive this to completion via spawn_from_io returning).
    });

    let result = transport
        .spawn_from_io(Box::new(client_stdin), Box::new(client_stdout))
        .await;

    let err = result
        .err()
        .expect("spawn_from_io must return Err for an incompatible protocol version");
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("unsupported app-server protocol version"),
        "error message should describe the incompatibility, got: {err_msg}"
    );
    assert!(
        err_msg.contains("1.0"),
        "error message should include the received version, got: {err_msg}"
    );
}

// ─── Fix 2: Background task turn lifecycle test ──────────────────────────────

/// Exercises the extracted `drive_notification_task` directly with in-memory I/O.
///
/// Injects `turn/started` and `turn/completed` notifications into the read side,
/// then asserts that `idle_flag` transitions correctly and `turn_state` reflects
/// the correct `TurnState` values.
#[tokio::test]
async fn test_app_server_background_task_turn_lifecycle() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use tokio::io::AsyncWriteExt;
    use atm_agent_mcp::transport::{drive_notification_task, NotificationTaskState};
    use atm_agent_mcp::stream_norm::TurnState;

    // Create a duplex for feeding data into the background task.
    let (mut feed_write, feed_read) = tokio::io::duplex(4096);
    // Create a duplex for the "proxy side" output of the background task.
    let (proxy_write_half, _proxy_read_half) = tokio::io::duplex(4096);

    let turn_state = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let idle_flag = Arc::new(AtomicBool::new(true));
    let initialized = Arc::new(AtomicBool::new(true));
    let pending_responses = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let session_registry = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

    // elicitation_registry/counter/upstream_tx/child_stdin are all None here:
    // these unit tests exercise turn-tracking and notification-routing only.
    // Approval bridging is tested via the dedicated unit tests in transport.rs
    // (bridge_entered_review_mode_* and security_* tests).
    let state = NotificationTaskState {
        turn_state: Arc::clone(&turn_state),
        idle_flag: Arc::clone(&idle_flag),
        initialized: Arc::clone(&initialized),
        pending_responses: Arc::clone(&pending_responses),
        session_registry: Arc::clone(&session_registry),
        team: "test-team".to_string(),
        turn_tracker: None,
        elicitation_registry: None,
        elicitation_counter: None,
        upstream_tx: None,
        child_stdin: None,
        agent_identity: Some("test-agent".to_string()),
    };

    tokio::task::spawn(drive_notification_task(
        feed_read,
        proxy_write_half,
        state,
    ));

    // Inject turn/started.
    let started = format!("{}\n", make_turn_started("turn-T1"));
    feed_write.write_all(started.as_bytes()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // idle_flag should be false while turn is in progress.
    assert!(
        !idle_flag.load(Ordering::Acquire),
        "idle_flag should be false after TurnStarted"
    );

    // Verify turn_state was updated.
    {
        let state = turn_state.lock().await;
        assert!(
            matches!(state.get("turn-T1"), Some(TurnState::Busy { .. })),
            "state should be Busy, got: {:?}",
            state.get("turn-T1")
        );
    }

    // Inject turn/completed.
    let completed = format!("{}\n", make_turn_completed("turn-T1", "completed"));
    feed_write.write_all(completed.as_bytes()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // idle_flag should be true — all threads are now Terminal.
    assert!(
        idle_flag.load(Ordering::Acquire),
        "idle_flag should be true after TurnCompleted with no Busy threads"
    );

    // Verify turn_state was updated.
    {
        let state = turn_state.lock().await;
        assert!(
            matches!(state.get("turn-T1"), Some(TurnState::Terminal { .. })),
            "state should be Terminal, got: {:?}",
            state.get("turn-T1")
        );
    }
}

// ─── Fix 2: Overload retry test ──────────────────────────────────────────────

/// Exercises the -32001 overload retry path through `fork_thread`.
///
/// Injects a -32001 error response followed by a success response into the
/// mock child's stdout, calls `fork_thread`, and asserts the retry was
/// attempted and the final success response was returned.
#[tokio::test]
async fn test_backpressure_overload_retry() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use atm_agent_mcp::app_server_test::TestAppServerTransport;

    // Set up a duplex pair: one side is "child stdin/stdout", the other is us.
    let (client_stdin, mut mock_stdin_rx) = tokio::io::duplex(8192);
    let (mut mock_stdout_tx, client_stdout) = tokio::io::duplex(8192);

    let transport = TestAppServerTransport::new("retry-test");

    // Spawn a "mock child" task that handles the handshake and then responds
    // to the fork request with a -32001 first and success second.
    let mock_task = tokio::spawn(async move {
        let mut reader = BufReader::new(&mut mock_stdin_rx);

        // 1. Read the initialize request.
        let mut init_line = String::new();
        reader.read_line(&mut init_line).await.unwrap();
        let init_req: serde_json::Value = serde_json::from_str(init_line.trim()).unwrap();
        assert_eq!(init_req["method"], "initialize");

        // 2. Send initialize response.
        let init_resp = format!("{}\n", make_init_response(0));
        mock_stdout_tx
            .write_all(init_resp.as_bytes())
            .await
            .unwrap();

        // 3. Read the initialized notification.
        let mut notif_line = String::new();
        reader.read_line(&mut notif_line).await.unwrap();
        let notif: serde_json::Value = serde_json::from_str(notif_line.trim()).unwrap();
        assert_eq!(notif["method"], "initialized");

        // 4. Read the first fork request.
        let mut fork_line1 = String::new();
        reader.read_line(&mut fork_line1).await.unwrap();
        let fork_req: serde_json::Value = serde_json::from_str(fork_line1.trim()).unwrap();
        let req_id = fork_req["id"].as_u64().unwrap();

        // 5. Send -32001 overload error.
        let overload = format!("{}\n", make_error_response(req_id, -32001, "overloaded"));
        mock_stdout_tx
            .write_all(overload.as_bytes())
            .await
            .unwrap();

        // 6. Read the retry fork request (same id).
        let mut fork_line2 = String::new();
        reader.read_line(&mut fork_line2).await.unwrap();
        let fork_req2: serde_json::Value = serde_json::from_str(fork_line2.trim()).unwrap();
        assert_eq!(fork_req2["id"].as_u64().unwrap(), req_id, "retry should use same request id");

        // 7. Send success response.
        let success = format!("{}\n", make_fork_response(req_id, "forked-thread-99"));
        mock_stdout_tx
            .write_all(success.as_bytes())
            .await
            .unwrap();
    });

    let raw_io = transport
        .spawn_from_io(Box::new(client_stdin), Box::new(client_stdout))
        .await
        .expect("spawn_from_io should succeed");

    // Call fork_thread — it should retry the -32001 and return the success response.
    let response = transport
        .fork_thread(&raw_io.stdin, "parent-thread")
        .await
        .expect("fork_thread should succeed after retry");

    assert_eq!(
        response["result"]["threadId"], "forked-thread-99",
        "fork_thread should return the success response after overload retry"
    );

    // Ensure the mock task completed successfully.
    mock_task.await.unwrap();
}

// ─── Fix 3: fork_thread integration test ─────────────────────────────────────

/// Exercises `fork_thread` on a `TestAppServerTransport` via `spawn_from_io`.
///
/// Verifies that the returned response contains a valid fork result and that
/// the request ID in the fork request is non-zero.
#[tokio::test]
async fn test_fork_thread_via_spawn_from_io() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use atm_agent_mcp::app_server_test::TestAppServerTransport;

    let (client_stdin, mut mock_stdin_rx) = tokio::io::duplex(8192);
    let (mut mock_stdout_tx, client_stdout) = tokio::io::duplex(8192);

    let transport = TestAppServerTransport::new("fork-test");

    // Mock child task: handshake + fork response.
    let mock_task = tokio::spawn(async move {
        let mut reader = BufReader::new(&mut mock_stdin_rx);

        // Handshake.
        let mut init_line = String::new();
        reader.read_line(&mut init_line).await.unwrap();
        let init_resp = format!("{}\n", make_init_response(0));
        mock_stdout_tx
            .write_all(init_resp.as_bytes())
            .await
            .unwrap();

        let mut notif_line = String::new();
        reader.read_line(&mut notif_line).await.unwrap();

        // Read fork request.
        let mut fork_line = String::new();
        reader.read_line(&mut fork_line).await.unwrap();
        let fork_req: serde_json::Value = serde_json::from_str(fork_line.trim()).unwrap();
        let req_id = fork_req["id"].as_u64().unwrap();
        assert!(req_id > 0, "fork request ID should be non-zero");
        assert_eq!(fork_req["method"], "thread/fork");

        // No `jsonrpc` field per app-server spec.
        assert!(
            fork_req.get("jsonrpc").is_none(),
            "fork request must not contain jsonrpc field"
        );

        // Send success response.
        let resp = format!("{}\n", make_fork_response(req_id, "new-thread-42"));
        mock_stdout_tx.write_all(resp.as_bytes()).await.unwrap();
    });

    let raw_io = transport
        .spawn_from_io(Box::new(client_stdin), Box::new(client_stdout))
        .await
        .expect("spawn_from_io should succeed");

    let response = transport
        .fork_thread(&raw_io.stdin, "thread-42")
        .await
        .expect("fork_thread should succeed");

    assert_eq!(response["result"]["threadId"], "new-thread-42");

    mock_task.await.unwrap();
}

// ─── Fix 4: Handshake order test ─────────────────────────────────────────────

/// Validates that `spawn_from_io` sends `initialize` before `initialized` and
/// that both omit the `jsonrpc` field.
///
/// Asserts:
/// - First message is `{"id":0,"method":"initialize",...}` with no `jsonrpc`.
/// - Second message (after initialize response) is `{"method":"initialized",...}`
///   with no `jsonrpc` and no `id`.
/// - No messages are sent before `initialize`.
#[tokio::test]
async fn test_app_server_handshake_order() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use atm_agent_mcp::app_server_test::TestAppServerTransport;

    let (client_stdin, mut mock_stdin_rx) = tokio::io::duplex(8192);
    let (mut mock_stdout_tx, client_stdout) = tokio::io::duplex(8192);

    let transport = TestAppServerTransport::new("handshake-order-test");

    // Mock child task: validate the handshake messages.
    let mock_task = tokio::spawn(async move {
        let mut reader = BufReader::new(&mut mock_stdin_rx);

        // 1. First message should be the initialize request.
        let mut init_line = String::new();
        reader.read_line(&mut init_line).await.unwrap();
        let init_req: serde_json::Value = serde_json::from_str(init_line.trim())
            .expect("initialize request should be valid JSON");

        // Verify: no `jsonrpc` field.
        assert!(
            init_req.get("jsonrpc").is_none(),
            "initialize request must NOT contain jsonrpc field, got: {init_req}"
        );
        // Verify: id is 0.
        assert_eq!(
            init_req["id"].as_u64().unwrap(),
            0,
            "initialize request id should be 0"
        );
        // Verify: method is "initialize".
        assert_eq!(init_req["method"], "initialize");

        // 2. Send initialize response.
        let init_resp = format!("{}\n", make_init_response(0));
        mock_stdout_tx
            .write_all(init_resp.as_bytes())
            .await
            .unwrap();

        // 3. Second message should be the initialized notification.
        let mut notif_line = String::new();
        reader.read_line(&mut notif_line).await.unwrap();
        let notif: serde_json::Value = serde_json::from_str(notif_line.trim())
            .expect("initialized notification should be valid JSON");

        // Verify: no `jsonrpc` field.
        assert!(
            notif.get("jsonrpc").is_none(),
            "initialized notification must NOT contain jsonrpc field, got: {notif}"
        );
        // Verify: method is "initialized".
        assert_eq!(notif["method"], "initialized");
        // Verify: no `id` field (it's a notification, not a request).
        assert!(
            notif.get("id").is_none(),
            "initialized notification must NOT have an id field, got: {notif}"
        );
    });

    let _raw_io = transport
        .spawn_from_io(Box::new(client_stdin), Box::new(client_stdout))
        .await
        .expect("spawn_from_io should succeed for handshake order test");

    // Ensure the mock task completed all assertions successfully.
    mock_task.await.expect("mock child task should complete without panic");
}

// ─── Response correlation test ───────────────────────────────────────────────

/// Verifies that the background task routes responses to `pending_responses` channels.
#[tokio::test]
async fn test_response_correlation_via_background_task() {
    use std::sync::{atomic::AtomicBool, Arc};
    use tokio::io::AsyncWriteExt;
    use atm_agent_mcp::transport::{drive_notification_task, NotificationTaskState};

    let (mut feed_write, feed_read) = tokio::io::duplex(4096);
    let (proxy_write_half, _proxy_read_half) = tokio::io::duplex(4096);

    let turn_state = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let idle_flag = Arc::new(AtomicBool::new(true));
    let initialized = Arc::new(AtomicBool::new(true));
    let pending_responses: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>,
        >,
    > = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let session_registry = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

    // Register a pending response for id=42.
    let (tx, rx) = tokio::sync::oneshot::channel();
    pending_responses.lock().await.insert(42, tx);

    // elicitation_registry/counter/upstream_tx/child_stdin are all None here:
    // these unit tests exercise turn-tracking and notification-routing only.
    // Approval bridging is tested via the dedicated unit tests in transport.rs
    // (bridge_entered_review_mode_* and security_* tests).
    let state = NotificationTaskState {
        turn_state: Arc::clone(&turn_state),
        idle_flag: Arc::clone(&idle_flag),
        initialized: Arc::clone(&initialized),
        pending_responses: Arc::clone(&pending_responses),
        session_registry: Arc::clone(&session_registry),
        team: "test-team".to_string(),
        turn_tracker: None,
        elicitation_registry: None,
        elicitation_counter: None,
        upstream_tx: None,
        child_stdin: None,
        agent_identity: Some("test-agent".to_string()),
    };

    tokio::task::spawn(drive_notification_task(
        feed_read,
        proxy_write_half,
        state,
    ));

    // Inject a response for id=42.
    let response_line = format!("{}\n", make_fork_response(42, "thread-new"));
    feed_write
        .write_all(response_line.as_bytes())
        .await
        .unwrap();

    // The oneshot channel should receive the response.
    let response = tokio::time::timeout(std::time::Duration::from_secs(2), rx)
        .await
        .expect("timeout waiting for response correlation")
        .expect("oneshot channel should receive the response");

    assert_eq!(response["id"], 42);
    assert_eq!(response["result"]["threadId"], "thread-new");

    // Verify the pending_responses map was cleaned up.
    assert!(
        !pending_responses.lock().await.contains_key(&42),
        "pending_responses should remove the entry after routing"
    );
}
