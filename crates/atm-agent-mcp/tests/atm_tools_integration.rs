//! Integration tests for ATM tool handlers via the proxy dispatch path.
//!
//! These tests exercise `handle_synthetic_tool` in `ProxyServer` by sending
//! JSON-RPC `tools/call` messages through a full proxy round-trip using in-memory
//! duplex streams.  This verifies that the proxy correctly routes ATM tool names
//! to the real handlers implemented in `atm_tools.rs`.
//!
//! All tests use `ATM_HOME` to redirect inbox I/O to a temporary directory and
//! are serialized with `#[serial]` to prevent env-var races.

use atm_agent_mcp::config::AgentMcpConfig;
use atm_agent_mcp::proxy::{ERR_IDENTITY_REQUIRED, ProxyServer};
use serde_json::{Value, json};
use serial_test::serial;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Create a `ProxyServer` with an explicit identity and team for ATM tests.
fn make_proxy(identity: Option<&str>, team: &str) -> ProxyServer {
    let config = AgentMcpConfig {
        identity: identity.map(|s| s.to_string()),
        ..Default::default()
    };
    ProxyServer::new_with_team(config, team)
}

/// Send a `tools/call` JSON-RPC message to the proxy via duplex I/O and return
/// the first Content-Length-framed response.
///
/// Strategy: send the message, wait for the first complete Content-Length response
/// from the proxy, then drop the client writer to allow the proxy to shut down.
async fn roundtrip_tools_call(proxy: &mut ProxyServer, msg: Value) -> Value {
    use atm_agent_mcp::framing::encode_content_length;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    let (mut client_write, proxy_read) = duplex(65536);
    let (proxy_write, mut client_read) = duplex(65536);

    // Frame and write the message
    let serialized = serde_json::to_string(&msg).unwrap();
    let frame = encode_content_length(&serialized);
    client_write.write_all(&frame).await.unwrap();
    // Do NOT drop client_write yet — dropping would trigger upstream EOF before proxy responds.

    // Run the proxy in a background task
    let mut proxy_moved = std::mem::replace(proxy, make_proxy(None, "__placeholder__"));
    let handle = tokio::spawn(async move {
        let _ = proxy_moved.run(proxy_read, proxy_write).await;
        proxy_moved
    });

    // Read until we can parse a complete response, then close the writer.
    // We read with a timeout to avoid hanging if the proxy never responds.
    let mut buf = Vec::new();
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let mut tmp = [0u8; 4096];
            match client_read.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    // Try to parse a complete response
                    let candidate = parse_first_content_length_response(&buf);
                    if candidate != json!(null) {
                        return candidate;
                    }
                }
            }
        }
        parse_first_content_length_response(&buf)
    })
    .await
    .unwrap_or(json!(null));

    // Signal EOF to allow proxy shutdown
    drop(client_write);
    let _ = handle.await;

    response
}

/// Extract the first JSON body from a Content-Length framed byte stream.
fn parse_first_content_length_response(data: &[u8]) -> Value {
    let text = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => return json!(null),
    };
    let header_end = match text.find("\r\n\r\n") {
        Some(i) => i,
        None => return json!(null),
    };
    let header = &text[..header_end];
    let body_start = header_end + 4;
    let len: usize = header
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.splitn(2, ':').nth(1))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let body_end = body_start + len;
    if body_end > text.len() {
        return json!(null);
    }
    serde_json::from_str(&text[body_start..body_end]).unwrap_or(json!(null))
}

/// Write a minimal team config JSON at the expected path.
fn write_team_config(home: &std::path::Path, team: &str, member_names: &[&str]) {
    let team_dir = home.join(".claude").join("teams").join(team);
    std::fs::create_dir_all(&team_dir).unwrap();

    let members: Vec<Value> = member_names
        .iter()
        .map(|name| {
            json!({
                "agentId": format!("{name}@{team}"),
                "name": name,
                "agentType": "general-purpose",
                "model": "claude-sonnet-4-6",
                "joinedAt": 1_000_000_u64,
                "cwd": "/tmp"
            })
        })
        .collect();

    let config = json!({
        "name": team,
        "createdAt": 1_000_000_u64,
        "leadAgentId": format!("{}@{}", member_names[0], team),
        "leadSessionId": "test-session-id",
        "members": members
    });

    std::fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Integration tests
// ---------------------------------------------------------------------------

/// `atm_send` with identity in config sends a message and returns success.
#[tokio::test]
#[serial]
async fn integration_atm_send_with_config_identity() {
    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("ATM_HOME", dir.path()) };

    let mut proxy = make_proxy(Some("team-lead"), "atm-dev");

    let msg = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "atm_send",
            "arguments": {
                "to": "arch-ctm",
                "message": "Integration test message"
            }
        }
    });

    let resp = roundtrip_tools_call(&mut proxy, msg).await;
    unsafe { std::env::remove_var("ATM_HOME") };

    assert!(resp.get("error").is_none(), "should not be a protocol error; got: {resp}");
    assert_ne!(resp["result"]["isError"], json!(true), "should not be isError; got: {resp}");

    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("arch-ctm"), "response should mention recipient; got: {text}");

    // Verify inbox file was written
    let inbox_path = dir
        .path()
        .join(".claude")
        .join("teams")
        .join("atm-dev")
        .join("inboxes")
        .join("arch-ctm.json");
    assert!(inbox_path.exists(), "inbox file should have been created");

    let content = std::fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["from"], "team-lead");
    assert_eq!(messages[0]["text"], "Integration test message");
}

/// `atm_send` with `identity` override in arguments wins over config identity.
#[tokio::test]
#[serial]
async fn integration_atm_send_explicit_identity_override() {
    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("ATM_HOME", dir.path()) };

    let mut proxy = make_proxy(Some("config-agent"), "team");

    let msg = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "atm_send",
            "arguments": {
                "to": "recip",
                "message": "hello",
                "identity": "override-agent"
            }
        }
    });

    let resp = roundtrip_tools_call(&mut proxy, msg).await;
    unsafe { std::env::remove_var("ATM_HOME") };

    assert_ne!(resp["result"]["isError"], json!(true), "should succeed; got: {resp}");

    let inbox_path = dir
        .path()
        .join(".claude")
        .join("teams")
        .join("team")
        .join("inboxes")
        .join("recip.json");
    let content = std::fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages[0]["from"], "override-agent", "should use explicit identity");
}

/// `atm_send` without any identity returns ERR_IDENTITY_REQUIRED (-32009).
#[tokio::test]
#[serial]
async fn integration_atm_send_no_identity_returns_error() {
    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("ATM_HOME", dir.path()) };

    let mut proxy = make_proxy(None, "team");

    let msg = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "atm_send",
            "arguments": {
                "to": "someone",
                "message": "hello"
            }
        }
    });

    let resp = roundtrip_tools_call(&mut proxy, msg).await;
    unsafe { std::env::remove_var("ATM_HOME") };

    let code = resp.pointer("/error/code").and_then(|v| v.as_i64());
    assert_eq!(
        code,
        Some(ERR_IDENTITY_REQUIRED),
        "missing identity should produce ERR_IDENTITY_REQUIRED (-32009); got: {resp}"
    );
}

/// `atm_read` returns empty array when inbox does not exist.
#[tokio::test]
#[serial]
async fn integration_atm_read_empty_inbox() {
    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("ATM_HOME", dir.path()) };

    let mut proxy = make_proxy(Some("my-agent"), "team");

    let msg = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "atm_read",
            "arguments": {}
        }
    });

    let resp = roundtrip_tools_call(&mut proxy, msg).await;
    unsafe { std::env::remove_var("ATM_HOME") };

    assert!(resp.get("error").is_none(), "should not be protocol error; got: {resp}");
    assert_ne!(resp["result"]["isError"], json!(true), "should not be isError; got: {resp}");

    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("null");
    let messages: Vec<Value> = serde_json::from_str(text).unwrap();
    assert!(messages.is_empty(), "empty inbox should return empty array");
}

/// `atm_pending_count` returns `{{"unread":0}}` for a nonexistent inbox.
#[tokio::test]
#[serial]
async fn integration_atm_pending_count_no_inbox() {
    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("ATM_HOME", dir.path()) };

    let mut proxy = make_proxy(Some("nobody"), "team");

    let msg = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "atm_pending_count",
            "arguments": {}
        }
    });

    let resp = roundtrip_tools_call(&mut proxy, msg).await;
    unsafe { std::env::remove_var("ATM_HOME") };

    assert!(resp.get("error").is_none(), "should not be protocol error; got: {resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("{}");
    let v: Value = serde_json::from_str(text).unwrap();
    assert_eq!(v["unread"], json!(0), "nonexistent inbox should have 0 unread");
}

/// `agent_sessions` returns stub error (Sprint A.6).
#[tokio::test]
#[serial]
async fn integration_agent_sessions_is_stub() {
    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("ATM_HOME", dir.path()) };

    let mut proxy = make_proxy(Some("team-lead"), "team");

    let msg = json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/call",
        "params": {
            "name": "agent_sessions",
            "arguments": {}
        }
    });

    let resp = roundtrip_tools_call(&mut proxy, msg).await;
    unsafe { std::env::remove_var("ATM_HOME") };

    // Should return isError: true with "not yet implemented" message
    assert_eq!(resp["result"]["isError"], json!(true), "stub should set isError; got: {resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("not yet implemented"),
        "stub message should mention not yet implemented; got: {text}"
    );
}

/// `atm_read` without identity returns ERR_IDENTITY_REQUIRED.
#[tokio::test]
#[serial]
async fn integration_atm_read_no_identity_returns_error() {
    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("ATM_HOME", dir.path()) };

    let mut proxy = make_proxy(None, "team");

    let msg = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": {
            "name": "atm_read",
            "arguments": {}
        }
    });

    let resp = roundtrip_tools_call(&mut proxy, msg).await;
    unsafe { std::env::remove_var("ATM_HOME") };

    let code = resp.pointer("/error/code").and_then(|v| v.as_i64());
    assert_eq!(
        code,
        Some(ERR_IDENTITY_REQUIRED),
        "atm_read without identity should return -32009; got: {resp}"
    );
}

/// `atm_broadcast` without identity returns ERR_IDENTITY_REQUIRED.
#[tokio::test]
#[serial]
async fn integration_atm_broadcast_no_identity_returns_error() {
    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("ATM_HOME", dir.path()) };

    let mut proxy = make_proxy(None, "team");

    let msg = json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "tools/call",
        "params": {
            "name": "atm_broadcast",
            "arguments": {"message": "hello"}
        }
    });

    let resp = roundtrip_tools_call(&mut proxy, msg).await;
    unsafe { std::env::remove_var("ATM_HOME") };

    let code = resp.pointer("/error/code").and_then(|v| v.as_i64());
    assert_eq!(
        code,
        Some(ERR_IDENTITY_REQUIRED),
        "atm_broadcast without identity should return -32009; got: {resp}"
    );
}

/// `atm_broadcast` with a valid team config sends to all non-caller members.
#[tokio::test]
#[serial]
async fn integration_atm_broadcast_delivers_to_members() {
    let dir = TempDir::new().unwrap();
    unsafe { std::env::set_var("ATM_HOME", dir.path()) };

    write_team_config(
        dir.path(),
        "broadcast-team",
        &["team-lead", "agent-a", "agent-b"],
    );

    let mut proxy = make_proxy(Some("team-lead"), "broadcast-team");

    let msg = json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "tools/call",
        "params": {
            "name": "atm_broadcast",
            "arguments": {"message": "broadcast hello"}
        }
    });

    let resp = roundtrip_tools_call(&mut proxy, msg).await;
    unsafe { std::env::remove_var("ATM_HOME") };

    assert!(resp.get("error").is_none(), "should not be protocol error; got: {resp}");
    assert_ne!(resp["result"]["isError"], json!(true), "should not be isError; got: {resp}");

    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("2 members"),
        "should report 2 recipients (not counting self); got: {text}"
    );

    // Verify both recipients got the message
    for agent in &["agent-a", "agent-b"] {
        let path = dir
            .path()
            .join(".claude")
            .join("teams")
            .join("broadcast-team")
            .join("inboxes")
            .join(format!("{agent}.json"));
        assert!(path.exists(), "inbox for {agent} should exist");
        let content = std::fs::read_to_string(&path).unwrap();
        let messages: Vec<Value> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages.len(), 1, "{agent} should have 1 message");
        assert_eq!(messages[0]["from"], "team-lead");
    }

    // Sender should NOT have received their own broadcast
    let sender_path = dir
        .path()
        .join(".claude")
        .join("teams")
        .join("broadcast-team")
        .join("inboxes")
        .join("team-lead.json");
    assert!(
        !sender_path.exists(),
        "sender should not receive their own broadcast"
    );
}
