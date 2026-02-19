//! Integration tests for the MCP proxy.
//!
//! These tests spawn the `echo-mcp-server` binary as the child process and
//! exercise the proxy's message routing, tool interception, event forwarding,
//! timeout, and crash detection.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

/// Find the path to the `echo-mcp-server` test binary.
fn echo_mcp_server_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("echo-mcp-server");
    path
}

/// Create a proxy config pointed at our echo server with an isolated team name.
///
/// Each call generates a unique team name so that concurrent integration tests
/// don't conflict on lock files (which use `<team>/<identity>.lock`).
fn test_config(timeout_secs: u64) -> atm_agent_mcp::proxy::ProxyServer {
    use atm_agent_mcp::config::AgentMcpConfig;

    let config = AgentMcpConfig {
        codex_bin: echo_mcp_server_path().to_string_lossy().to_string(),
        request_timeout_secs: timeout_secs,
        ..Default::default()
    };
    // Use a unique team per test invocation so lock files don't collide across
    // concurrently running integration tests.
    let unique_team = format!("test-{}", uuid::Uuid::new_v4());
    atm_agent_mcp::proxy::ProxyServer::new_with_team(config, unique_team)
}

/// Send a JSON-RPC message to the proxy via Content-Length framing.
async fn send_content_length(writer: &mut DuplexStream, msg: &Value) {
    let json = serde_json::to_string(msg).unwrap();
    let header = format!("Content-Length: {}\r\n\r\n", json.len());
    writer.write_all(header.as_bytes()).await.unwrap();
    writer.write_all(json.as_bytes()).await.unwrap();
    writer.flush().await.unwrap();
}

/// Send a JSON-RPC message in newline-delimited format.
async fn send_newline(writer: &mut DuplexStream, msg: &Value) {
    let json = serde_json::to_string(msg).unwrap();
    writer.write_all(json.as_bytes()).await.unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.flush().await.unwrap();
}

/// Read a Content-Length framed response from the proxy.
async fn read_response(reader: &mut BufReader<DuplexStream>) -> Option<Value> {
    let mut header_line = String::new();

    // Try to read the Content-Length header with a timeout
    match tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            header_line.clear();
            let n = reader.read_line(&mut header_line).await.ok()?;
            if n == 0 {
                return None;
            }
            let trimmed = header_line.trim();
            if trimmed.starts_with("Content-Length:") {
                break;
            }
            // skip blank lines or other headers
        }
        Some(())
    })
    .await
    {
        Ok(Some(())) => {}
        Ok(None) | Err(_) => return None,
    }

    let len: usize = header_line
        .trim()
        .strip_prefix("Content-Length:")
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    // Read until blank line
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.ok()?;
        if line.trim().is_empty() {
            break;
        }
    }

    let mut body = vec![0u8; len];
    tokio::io::AsyncReadExt::read_exact(reader, &mut body)
        .await
        .ok()?;
    let s = String::from_utf8(body).ok()?;
    serde_json::from_str(&s).ok()
}

/// Read all responses available within a timeout.
async fn read_all_responses(
    reader: &mut BufReader<DuplexStream>,
    timeout_duration: Duration,
) -> Vec<Value> {
    let mut results = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout_duration;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, read_response(reader)).await {
            Ok(Some(v)) => results.push(v),
            _ => break,
        }
    }
    results
}

/// Helper: run a proxy with a pair of duplex streams.
///
/// Returns (write_end, read_end, join_handle) where:
/// - write_end: send messages TO the proxy
/// - read_end: read messages FROM the proxy
fn spawn_proxy(
    timeout_secs: u64,
) -> (
    DuplexStream,
    BufReader<DuplexStream>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let (client_write, proxy_read) = tokio::io::duplex(16384);
    let (proxy_write, client_read) = tokio::io::duplex(16384);

    let handle = tokio::spawn(async move {
        let mut proxy = test_config(timeout_secs);
        proxy.run(proxy_read, proxy_write).await
    });

    (client_write, BufReader::new(client_read), handle)
}

// ─── Initialize pass-through ────────────────────────────────────────────

#[tokio::test]
async fn test_initialize_passes_through() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    // Send initialize request — but first we need to trigger child spawn.
    // The child is lazy-spawned on codex/codex-reply. For initialize, the child
    // isn't spawned yet, so we need to send a codex call first, or accept that
    // initialize returns an error.
    //
    // Actually, the proxy forwards all non-tools/call methods to child if spawned.
    // Since child isn't spawned on initialize, it returns an error.
    // Let's first spawn the child with a codex call, then test initialize.

    // First, trigger child spawn with a codex tools/call
    let codex_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "codex",
            "arguments": {"prompt": "hello"}
        }
    });
    send_newline(&mut writer, &codex_req).await;

    // Read all responses (events + final response)
    let responses = read_all_responses(&mut reader, Duration::from_secs(5)).await;
    assert!(
        !responses.is_empty(),
        "should have received at least one response"
    );

    // Now send initialize
    let init_req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {}
        }
    });
    send_content_length(&mut writer, &init_req).await;

    let resp = read_response(&mut reader).await.expect("initialize response");
    assert_eq!(resp["id"], 2);
    assert!(resp.get("result").is_some(), "initialize should succeed");
    assert_eq!(
        resp["result"]["serverInfo"]["name"],
        "echo-mcp-server"
    );

    drop(writer);
    let _ = handle.await;
}

// ─── Notifications initialized pass-through ─────────────────────────────

#[tokio::test]
async fn test_notifications_initialized_passes_through() {
    let (mut writer, _reader, handle) = spawn_proxy(300);

    // Notifications don't get responses, so we just verify no crash
    let notif = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    send_newline(&mut writer, &notif).await;

    // Small delay to let proxy process
    tokio::time::sleep(Duration::from_millis(100)).await;

    drop(writer);
    let _ = handle.await;
}

// ─── tools/list interception ────────────────────────────────────────────

#[tokio::test]
async fn test_tools_list_adds_synthetic_tools() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    // First spawn child
    let codex_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "init"}}
    });
    send_newline(&mut writer, &codex_req).await;
    let _ = read_all_responses(&mut reader, Duration::from_secs(5)).await;

    // Now send tools/list
    let list_req = json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "tools/list"
    });
    send_newline(&mut writer, &list_req).await;

    let resp = read_response(&mut reader).await.expect("tools/list response");
    assert_eq!(resp["id"], 10);
    let tools = resp["result"]["tools"].as_array().expect("tools array");

    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .collect();

    // Synthetic tools must be present
    assert!(names.contains(&"atm_send"), "missing atm_send");
    assert!(names.contains(&"atm_read"), "missing atm_read");
    assert!(names.contains(&"atm_broadcast"), "missing atm_broadcast");
    assert!(
        names.contains(&"atm_pending_count"),
        "missing atm_pending_count"
    );
    assert!(
        names.contains(&"agent_sessions"),
        "missing agent_sessions"
    );
    assert!(names.contains(&"agent_status"), "missing agent_status");
    assert!(names.contains(&"agent_close"), "missing agent_close");

    drop(writer);
    let _ = handle.await;
}

#[tokio::test]
async fn test_tools_list_preserves_codex_tools() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    // Spawn child
    let codex_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "init"}}
    });
    send_newline(&mut writer, &codex_req).await;
    let _ = read_all_responses(&mut reader, Duration::from_secs(5)).await;

    let list_req = json!({
        "jsonrpc": "2.0",
        "id": 20,
        "method": "tools/list"
    });
    send_newline(&mut writer, &list_req).await;

    let resp = read_response(&mut reader).await.expect("tools/list response");
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .collect();

    assert!(names.contains(&"codex"), "original codex tool missing");
    assert!(
        names.contains(&"codex-reply"),
        "original codex-reply tool missing"
    );

    drop(writer);
    let _ = handle.await;
}

#[tokio::test]
async fn test_multiple_synthetic_tools_count() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    // Spawn child
    let codex_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "init"}}
    });
    send_newline(&mut writer, &codex_req).await;
    let _ = read_all_responses(&mut reader, Duration::from_secs(5)).await;

    let list_req = json!({
        "jsonrpc": "2.0",
        "id": 30,
        "method": "tools/list"
    });
    send_newline(&mut writer, &list_req).await;

    let resp = read_response(&mut reader).await.expect("tools/list response");
    let tools = resp["result"]["tools"].as_array().expect("tools array");

    // 2 from echo server + 7 synthetic = 9
    assert_eq!(tools.len(), 9, "expected 2 native + 7 synthetic tools");

    drop(writer);
    let _ = handle.await;
}

// ─── Unknown method pass-through ────────────────────────────────────────

#[tokio::test]
async fn test_unknown_method_passes_through() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    // Spawn child first
    let codex_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "init"}}
    });
    send_newline(&mut writer, &codex_req).await;
    let _ = read_all_responses(&mut reader, Duration::from_secs(5)).await;

    // Send unknown method
    let req = json!({
        "jsonrpc": "2.0",
        "id": 40,
        "method": "custom/foobar",
        "params": {}
    });
    send_newline(&mut writer, &req).await;

    let resp = read_response(&mut reader).await.expect("should get error response");
    assert_eq!(resp["id"], 40);
    // The echo server returns -32601 for unknown methods
    assert_eq!(resp["error"]["code"], -32601);

    drop(writer);
    let _ = handle.await;
}

// ─── Lazy spawn tests ───────────────────────────────────────────────────

#[tokio::test]
async fn test_lazy_spawn_on_first_codex_call() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    // Before any codex call: send tools/list — should return error (no child)
    let list_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list"
    });
    send_newline(&mut writer, &list_req).await;

    let resp = read_response(&mut reader).await.expect("error response");
    assert_eq!(resp["id"], 1);
    // Should be an error since child not spawned
    assert!(resp.get("error").is_some(), "expected error for no child");

    // Now send codex — this should spawn the child
    let codex_req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "hello"}}
    });
    send_newline(&mut writer, &codex_req).await;

    // Should get events + response now
    let responses = read_all_responses(&mut reader, Duration::from_secs(5)).await;
    assert!(!responses.is_empty(), "should have received response(s)");

    // Find the response with id=2
    let main_resp = responses.iter().find(|r| r.get("id") == Some(&json!(2)));
    assert!(
        main_resp.is_some(),
        "should have the codex response with id=2"
    );

    drop(writer);
    let _ = handle.await;
}

// ─── Child crash tests ──────────────────────────────────────────────────

#[tokio::test]
async fn test_child_crash_returns_error() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    // Spawn child
    let codex_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "init"}}
    });
    send_newline(&mut writer, &codex_req).await;
    let _ = read_all_responses(&mut reader, Duration::from_secs(5)).await;

    // Crash the child
    let crash_req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "crash", "arguments": {}}
    });
    send_newline(&mut writer, &crash_req).await;

    // Wait for child to die
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Next request should return dead child error
    let codex_req2 = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "after crash"}}
    });
    send_newline(&mut writer, &codex_req2).await;

    let responses = read_all_responses(&mut reader, Duration::from_secs(5)).await;
    let error_resp = responses.iter().find(|r| {
        r.get("id") == Some(&json!(3))
            && r.pointer("/error/code").and_then(|v| v.as_i64()) == Some(-32005)
    });
    assert!(
        error_resp.is_some(),
        "expected -32005 CHILD_PROCESS_DEAD error, got: {responses:?}"
    );

    drop(writer);
    let _ = handle.await;
}

#[tokio::test]
async fn test_child_crash_includes_exit_code() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    // Spawn child
    let codex_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "init"}}
    });
    send_newline(&mut writer, &codex_req).await;
    let _ = read_all_responses(&mut reader, Duration::from_secs(5)).await;

    // Crash the child (exit code 42)
    let crash_req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "crash", "arguments": {}}
    });
    send_newline(&mut writer, &crash_req).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Send another request
    let req = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "after"}}
    });
    send_newline(&mut writer, &req).await;

    let responses = read_all_responses(&mut reader, Duration::from_secs(5)).await;
    let error_resp = responses.iter().find(|r| r.get("id") == Some(&json!(3)));
    assert!(error_resp.is_some(), "expected error response");

    let err = error_resp.unwrap();
    let exit_code = err.pointer("/error/data/exit_code").and_then(|v| v.as_i64());
    assert_eq!(exit_code, Some(42), "expected exit code 42");

    drop(writer);
    let _ = handle.await;
}

// ─── Timeout tests ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_request_timeout_returns_error() {
    // Use a 1-second timeout
    let (mut writer, mut reader, handle) = spawn_proxy(1);

    // Send a slow codex call (echo server sleeps 5s)
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "slow", "slow": true}}
    });
    send_newline(&mut writer, &req).await;

    let responses = read_all_responses(&mut reader, Duration::from_secs(10)).await;
    let timeout_resp = responses.iter().find(|r| {
        r.get("id") == Some(&json!(1))
            && r.pointer("/error/code").and_then(|v| v.as_i64()) == Some(-32006)
    });
    assert!(
        timeout_resp.is_some(),
        "expected -32006 timeout error, got: {responses:?}"
    );

    drop(writer);
    let _ = handle.await;
}

#[tokio::test]
async fn test_timeout_includes_proxy_source() {
    let (mut writer, mut reader, handle) = spawn_proxy(1);

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "slow", "slow": true}}
    });
    send_newline(&mut writer, &req).await;

    let responses = read_all_responses(&mut reader, Duration::from_secs(10)).await;
    let timeout_resp = responses.iter().find(|r| {
        r.pointer("/error/code").and_then(|v| v.as_i64()) == Some(-32006)
    });
    assert!(timeout_resp.is_some(), "expected timeout error");
    assert_eq!(
        timeout_resp.unwrap().pointer("/error/data/error_source"),
        Some(&json!("proxy"))
    );

    drop(writer);
    let _ = handle.await;
}

// ─── Event forwarding tests ─────────────────────────────────────────────

#[tokio::test]
async fn test_codex_event_forwarded_to_upstream() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    // Codex call triggers 2 events before the response
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "hello"}}
    });
    send_newline(&mut writer, &req).await;

    let responses = read_all_responses(&mut reader, Duration::from_secs(5)).await;

    let events: Vec<&Value> = responses
        .iter()
        .filter(|r| r.get("method") == Some(&json!("codex/event")))
        .collect();
    assert!(
        events.len() >= 2,
        "expected at least 2 codex/event notifications, got {}",
        events.len()
    );

    drop(writer);
    let _ = handle.await;
}

#[tokio::test]
async fn test_codex_event_has_agent_id() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "hello"}}
    });
    send_newline(&mut writer, &req).await;

    let responses = read_all_responses(&mut reader, Duration::from_secs(5)).await;
    let events: Vec<&Value> = responses
        .iter()
        .filter(|r| r.get("method") == Some(&json!("codex/event")))
        .collect();

    for event in &events {
        let agent_id = event
            .pointer("/params/agent_id")
            .and_then(|v| v.as_str());
        // Events without a known threadId mapping fall back to "proxy:unknown"
        assert!(
            agent_id.is_some(),
            "event should have an agent_id field"
        );
    }

    drop(writer);
    let _ = handle.await;
}

#[tokio::test]
async fn test_event_content_unchanged() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "hello"}}
    });
    send_newline(&mut writer, &req).await;

    let responses = read_all_responses(&mut reader, Duration::from_secs(5)).await;
    let events: Vec<&Value> = responses
        .iter()
        .filter(|r| r.get("method") == Some(&json!("codex/event")))
        .collect();

    assert!(!events.is_empty(), "expected events");
    // Check that the msg content is preserved from the echo server
    let first_event = events[0];
    let msg_type = first_event
        .pointer("/params/msg/type")
        .and_then(|v| v.as_str());
    assert!(
        msg_type.is_some(),
        "event msg.type should be present"
    );
    // The echo server sends "session_configured" as the first event type
    assert_eq!(msg_type, Some("session_configured"));

    drop(writer);
    let _ = handle.await;
}

// ─── Proxy lifecycle tests ──────────────────────────────────────────────

#[tokio::test]
async fn test_proxy_shuts_down_on_stdin_eof() {
    let (writer, _reader, handle) = spawn_proxy(300);

    // Drop the writer immediately — proxy should exit
    drop(writer);

    let result = tokio::time::timeout(Duration::from_secs(5), handle).await;
    assert!(result.is_ok(), "proxy should exit on stdin EOF");
    assert!(result.unwrap().is_ok(), "proxy should exit without panic");
}

#[tokio::test]
async fn test_tools_list_schema_valid() {
    // Verify all synthetic tools have valid JSON Schema inputSchema
    let tools = atm_agent_mcp::tools::synthetic_tools();
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        let schema = tool
            .get("inputSchema")
            .unwrap_or_else(|| panic!("{name} missing inputSchema"));
        assert_eq!(
            schema["type"].as_str(),
            Some("object"),
            "{name} inputSchema type should be 'object'"
        );
        assert!(
            schema.get("properties").is_some(),
            "{name} inputSchema should have properties"
        );
    }
}

// ─── codex-reply pass-through ───────────────────────────────────────────

#[tokio::test]
async fn test_codex_reply_passes_through() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    // First spawn with codex
    let codex_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "codex", "arguments": {"prompt": "start session"}}
    });
    send_newline(&mut writer, &codex_req).await;
    let _ = read_all_responses(&mut reader, Duration::from_secs(5)).await;

    // Now send codex-reply
    let reply_req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "codex-reply",
            "arguments": {"prompt": "continue", "threadId": "test-thread-001"}
        }
    });
    send_newline(&mut writer, &reply_req).await;

    let responses = read_all_responses(&mut reader, Duration::from_secs(5)).await;
    let main_resp = responses.iter().find(|r| r.get("id") == Some(&json!(2)));
    assert!(main_resp.is_some(), "should get codex-reply response");

    let resp = main_resp.unwrap();
    let content = resp
        .pointer("/result/structuredContent/threadId")
        .and_then(|v| v.as_str());
    assert_eq!(content, Some("test-thread-001"));

    drop(writer);
    let _ = handle.await;
}

// ─── Synthetic ATM tool dispatch ─────────────────────────────────────────

/// ATM tools require an identity.  When no identity is configured on the proxy
/// and none is provided in arguments, the proxy must return ERR_IDENTITY_REQUIRED
/// (-32009) as a JSON-RPC error (not an `isError` result).
///
/// This test replaced the Sprint A.2/A.3 stub test which expected `isError:true`
/// with "not yet implemented" — ATM tools are real as of Sprint A.4.
#[tokio::test]
async fn test_synthetic_tool_returns_not_implemented() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    // atm_send requires identity; default proxy config has none → ERR_IDENTITY_REQUIRED
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "atm_send",
            "arguments": {"to": "agent1", "message": "hello"}
        }
    });
    send_newline(&mut writer, &req).await;

    let resp = read_response(&mut reader).await.expect("synthetic tool response");
    assert_eq!(resp["id"], 1);
    // Must be a JSON-RPC error (not a result)
    let code = resp
        .pointer("/error/code")
        .and_then(|v| v.as_i64())
        .expect("error code must be present");
    assert_eq!(
        code,
        atm_agent_mcp::proxy::ERR_IDENTITY_REQUIRED,
        "atm_send without identity should return ERR_IDENTITY_REQUIRED (-32009)"
    );

    drop(writer);
    let _ = handle.await;
}

// ─── Content-Length upstream framing ─────────────────────────────────────

#[tokio::test]
async fn test_content_length_upstream_framing() {
    let (mut writer, mut reader, handle) = spawn_proxy(300);

    // Send a synthetic tool call using Content-Length framing
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "agent_status",
            "arguments": {}
        }
    });
    send_content_length(&mut writer, &req).await;

    let resp = read_response(&mut reader).await.expect("response to CL-framed request");
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["isError"], true);

    drop(writer);
    let _ = handle.await;
}

// ─── Dropped events counter ─────────────────────────────────────────────

#[tokio::test]
async fn test_dropped_events_counter_accessible() {
    use atm_agent_mcp::config::AgentMcpConfig;
    use std::sync::atomic::Ordering;

    let config = AgentMcpConfig::default();
    let proxy = atm_agent_mcp::proxy::ProxyServer::new(config);
    assert_eq!(proxy.dropped_events.load(Ordering::Relaxed), 0);
}
