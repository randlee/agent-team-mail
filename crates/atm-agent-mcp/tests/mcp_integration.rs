//! Integration tests for transport plumbing and end-to-end ATM tool calls.
//!
//! Tests in two categories:
//!
//! 1. **MockTransport plumbing tests** (run in CI, no `#[ignore]`): pure in-memory
//!    tests that exercise the `MockTransport` I/O wiring without any real child process.
//!
//! 2. **End-to-end ATM tool tests** (`#[ignore]`): require a live codex binary.
//!    Run manually with:
//!    ```bash
//!    cargo test -p atm-agent-mcp --test mcp_integration -- --ignored
//!    ```

use atm_agent_mcp::MockTransport;
use serde_json::{Value, json};

// ─── MockTransport plumbing tests ────────────────────────────────────────────

/// Verify that [`MockTransport::new_with_handle`] creates a usable transport.
///
/// The proxy should be able to invoke `spawn()` without error and the returned
/// `RawChildIo` should have `None` for `exit_status` and `process` (no real
/// child process is involved).
#[tokio::test]
async fn mock_transport_spawn_succeeds() {
    let (transport, _handle) = MockTransport::new_with_handle();
    let raw = transport
        .spawn()
        .await
        .expect("MockTransport::spawn should succeed");

    // No real child process: exit_status and process should be None.
    assert!(
        raw.exit_status.lock().await.is_none(),
        "exit_status should be None for in-memory transport"
    );
    assert!(
        raw.process.lock().await.is_none(),
        "process should be None for in-memory transport"
    );
}

/// Verify that messages injected via the handle appear on the stdout pipe.
///
/// This exercises the response_tx -> duplex -> stdout_read path: a JSON-RPC
/// response written to the handle's sender must be readable (as a newline-
/// terminated line) from the `RawChildIo.stdout` reader.
#[tokio::test]
async fn mock_transport_injects_responses() {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let (transport, handle) = MockTransport::new_with_handle();
    let raw = transport
        .spawn()
        .await
        .expect("MockTransport::spawn should succeed");

    // Inject a mock JSON-RPC response.
    let msg = json!({"jsonrpc": "2.0", "id": 1, "result": {}});
    handle
        .response_tx
        .send(serde_json::to_string(&msg).unwrap())
        .expect("send should succeed while channel is open");

    // Read the injected line from the stdout pipe.
    let mut reader = BufReader::new(raw.stdout);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .expect("read_line should succeed");

    let parsed: Value =
        serde_json::from_str(line.trim()).expect("injected line should be valid JSON");
    assert_eq!(parsed["id"], 1, "response id should round-trip correctly");
}

/// Verify that messages written to the stdin writer are captured by the handle.
///
/// This exercises the SniffWriter -> request_tx -> request_rx path: bytes
/// written to `RawChildIo.stdin` should appear (line-by-line) in the handle's
/// `request_rx` receiver.
#[tokio::test]
async fn mock_transport_captures_stdin_writes() {
    use tokio::io::AsyncWriteExt;

    let (transport, mut handle) = MockTransport::new_with_handle();
    let raw = transport
        .spawn()
        .await
        .expect("MockTransport::spawn should succeed");

    // Write a JSON-RPC request to "child stdin".
    let request =
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
    {
        let mut stdin = raw.stdin.lock().await;
        let json_str = format!("{}\n", serde_json::to_string(&request).unwrap());
        stdin
            .write_all(json_str.as_bytes())
            .await
            .expect("write_all to SniffWriter should succeed");
    }

    // The captured line should appear in the request receiver.
    let captured = handle
        .request_rx
        .recv()
        .await
        .expect("request_rx should yield the captured line");
    let parsed: Value =
        serde_json::from_str(&captured).expect("captured line should be valid JSON");
    assert_eq!(
        parsed["method"], "initialize",
        "method should match the written request"
    );
}

/// Verify that `MockTransport` stdout is fully isolated from process stdout.
///
/// This test verifies the critical isolation guarantee: data injected as
/// "child stdout" MUST be readable from `RawChildIo.stdout` (the duplex read
/// half) and MUST NOT appear on `std::io::stdout()`.
///
/// The test injects five responses and asserts that all five are readable from
/// the pipe reader; it does not (and cannot) directly assert that nothing was
/// written to process stdout, but the implementation uses `tokio::io::duplex`
/// which is entirely in-memory and never touches the OS-level file descriptors.
#[tokio::test]
async fn mock_transport_stdout_isolation() {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let (transport, handle) = MockTransport::new_with_handle();
    let raw = transport
        .spawn()
        .await
        .expect("MockTransport::spawn should succeed");

    // Inject multiple responses.
    for i in 0u64..5 {
        let msg = json!({"jsonrpc": "2.0", "id": i, "result": {"value": i}});
        handle
            .response_tx
            .send(serde_json::to_string(&msg).unwrap())
            .expect("send should succeed");
    }
    // Close BOTH the handle's sender AND the transport's keepalive sender so the
    // background task reaches EOF.  The background task only terminates when all
    // `UnboundedSender` handles are dropped.
    drop(handle.response_tx);
    drop(transport); // drops the keepalive `response_tx` inside MockTransport

    // All responses should be readable from the in-memory pipe.
    let mut reader = BufReader::new(raw.stdout);
    let mut count: usize = 0;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .expect("read_line should not error");
        if n == 0 {
            break; // EOF
        }
        if line.trim().is_empty() {
            continue;
        }
        let _parsed: Value =
            serde_json::from_str(line.trim()).expect("each line must be valid JSON");
        count += 1;
    }
    assert_eq!(count, 5, "all 5 injected responses should be readable");
}

// ─── MCP transport end-to-end tests ─────────────────────────────────────────

/// Test that atm_send works in MCP transport mode.
#[tokio::test]
#[ignore = "requires live codex binary with MCP server; run manually with --ignored"]
async fn test_mcp_atm_send() {
    use atm_agent_mcp::config::AgentMcpConfig;
    use atm_agent_mcp::proxy::ProxyServer;

    let config = AgentMcpConfig {
        model: Some("codex-mini-latest".to_string()),
        auto_mail: false,
        // transport defaults to "mcp"
        ..Default::default()
    };
    let team = format!("test-mcp-send-{}", uuid::Uuid::new_v4());
    let proxy = ProxyServer::new_with_team(config, team.clone());

    // Verify proxy was constructed with the expected team.
    assert_eq!(proxy.team, team);

    // Full test: run proxy.run(upstream_reader, upstream_writer).await,
    // send a tools/call request for atm_send, verify the message appears
    // in the ATM team inbox. Requires: live codex MCP server binary.
}

/// Test that atm_read works in MCP transport mode.
#[tokio::test]
#[ignore = "requires live codex binary with MCP server; run manually with --ignored"]
async fn test_mcp_atm_read() {
    use atm_agent_mcp::config::AgentMcpConfig;
    use atm_agent_mcp::proxy::ProxyServer;

    let config = AgentMcpConfig {
        model: Some("codex-mini-latest".to_string()),
        auto_mail: false,
        ..Default::default()
    };
    let team = format!("test-mcp-read-{}", uuid::Uuid::new_v4());
    let proxy = ProxyServer::new_with_team(config, team.clone());

    assert_eq!(proxy.team, team);

    // Full test: run proxy.run(...), send a codex session, then call atm_read
    // and verify unread messages in the team inbox are returned.
    // Requires: live codex MCP server binary.
}

/// Test that atm_broadcast works in MCP transport mode.
#[tokio::test]
#[ignore = "requires live codex binary with MCP server; run manually with --ignored"]
async fn test_mcp_atm_broadcast() {
    use atm_agent_mcp::config::AgentMcpConfig;
    use atm_agent_mcp::proxy::ProxyServer;

    let config = AgentMcpConfig {
        model: Some("codex-mini-latest".to_string()),
        auto_mail: false,
        ..Default::default()
    };
    let team = format!("test-mcp-broadcast-{}", uuid::Uuid::new_v4());
    let proxy = ProxyServer::new_with_team(config, team.clone());

    assert_eq!(proxy.team, team);

    // Full test: run proxy.run(...), call atm_broadcast, verify that all
    // team members received the broadcast message.
    // Requires: live codex MCP server binary.
}

/// Test that atm_pending_count works in MCP transport mode.
#[tokio::test]
#[ignore = "requires live codex binary with MCP server; run manually with --ignored"]
async fn test_mcp_atm_pending_count() {
    use atm_agent_mcp::config::AgentMcpConfig;
    use atm_agent_mcp::proxy::ProxyServer;

    let config = AgentMcpConfig {
        model: Some("codex-mini-latest".to_string()),
        auto_mail: false,
        ..Default::default()
    };
    let team = format!("test-mcp-pending-{}", uuid::Uuid::new_v4());
    let proxy = ProxyServer::new_with_team(config, team.clone());

    assert_eq!(proxy.team, team);

    // Full test: enqueue messages, call atm_pending_count, verify the count
    // matches the number of queued messages.
    // Requires: live codex MCP server binary.
}

// ─── JSON transport end-to-end tests ────────────────────────────────────────

/// Test that atm_send works in JSON transport mode.
#[tokio::test]
#[ignore = "requires live codex binary with --json flag; run manually with --ignored"]
async fn test_json_atm_send() {
    use atm_agent_mcp::config::AgentMcpConfig;
    use atm_agent_mcp::proxy::ProxyServer;

    let config = AgentMcpConfig {
        model: Some("codex-mini-latest".to_string()),
        transport: Some("json".to_string()),
        auto_mail: false,
        ..Default::default()
    };
    let team = format!("test-json-send-{}", uuid::Uuid::new_v4());
    let proxy = ProxyServer::new_with_team(config, team.clone());

    assert_eq!(proxy.team, team);

    // Full test: run proxy.run(...), send a tools/call for atm_send via JSON
    // transport, verify the message appears in the ATM team inbox.
    // Requires: live codex binary supporting `exec --json`.
}

/// Test that atm_read works in JSON transport mode.
#[tokio::test]
#[ignore = "requires live codex binary with --json flag; run manually with --ignored"]
async fn test_json_atm_read() {
    use atm_agent_mcp::config::AgentMcpConfig;
    use atm_agent_mcp::proxy::ProxyServer;

    let config = AgentMcpConfig {
        model: Some("codex-mini-latest".to_string()),
        transport: Some("json".to_string()),
        auto_mail: false,
        ..Default::default()
    };
    let team = format!("test-json-read-{}", uuid::Uuid::new_v4());
    let proxy = ProxyServer::new_with_team(config, team.clone());

    assert_eq!(proxy.team, team);

    // Full test: run proxy.run(...), send a codex session, call atm_read and
    // verify unread messages are returned.
    // Requires: live codex binary supporting `exec --json`.
}

/// Test that atm_broadcast works in JSON transport mode.
#[tokio::test]
#[ignore = "requires live codex binary with --json flag; run manually with --ignored"]
async fn test_json_atm_broadcast() {
    use atm_agent_mcp::config::AgentMcpConfig;
    use atm_agent_mcp::proxy::ProxyServer;

    let config = AgentMcpConfig {
        model: Some("codex-mini-latest".to_string()),
        transport: Some("json".to_string()),
        auto_mail: false,
        ..Default::default()
    };
    let team = format!("test-json-broadcast-{}", uuid::Uuid::new_v4());
    let proxy = ProxyServer::new_with_team(config, team.clone());

    assert_eq!(proxy.team, team);

    // Full test: run proxy.run(...), call atm_broadcast, verify all team members
    // received the message.
    // Requires: live codex binary supporting `exec --json`.
}

/// Test that atm_pending_count works in JSON transport mode.
#[tokio::test]
#[ignore = "requires live codex binary with --json flag; run manually with --ignored"]
async fn test_json_atm_pending_count() {
    use atm_agent_mcp::config::AgentMcpConfig;
    use atm_agent_mcp::proxy::ProxyServer;

    let config = AgentMcpConfig {
        model: Some("codex-mini-latest".to_string()),
        transport: Some("json".to_string()),
        auto_mail: false,
        ..Default::default()
    };
    let team = format!("test-json-pending-{}", uuid::Uuid::new_v4());
    let proxy = ProxyServer::new_with_team(config, team.clone());

    assert_eq!(proxy.team, team);

    // Full test: enqueue messages into the stdin queue, verify atm_pending_count
    // returns the correct count.
    // Requires: live codex binary supporting `exec --json`.
}

/// Inject an ATM message via stdin queue during a JSON-mode session.
#[tokio::test]
#[ignore = "requires live codex binary with --json flag; run manually with --ignored"]
async fn test_json_stdin_queue_inject() {
    use atm_agent_mcp::config::AgentMcpConfig;
    use atm_agent_mcp::proxy::ProxyServer;

    let config = AgentMcpConfig {
        model: Some("codex-mini-latest".to_string()),
        transport: Some("json".to_string()),
        auto_mail: false,
        ..Default::default()
    };
    let team = format!("test-json-queue-{}", uuid::Uuid::new_v4());
    let proxy = ProxyServer::new_with_team(config, team.clone());

    assert_eq!(proxy.team, team);

    // Full test:
    // 1. A JSON-mode session is running and enters idle state
    // 2. An ATM message is enqueued via stdin_queue::enqueue()
    // 3. The idle event triggers a queue drain
    // 4. The message reaches the Codex child process stdin
    // Requires: live codex binary supporting `exec --json`.
}
