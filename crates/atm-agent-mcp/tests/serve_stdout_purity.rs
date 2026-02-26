//! Verify `atm-agent-mcp serve` keeps stdout clean for MCP JSON-RPC framing.
//!
//! Any non-JSON text on stdout corrupts the MCP stdio transport and breaks
//! clients such as MCP Inspector.

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use tempfile::TempDir;

fn write_content_length_request(stdin: &mut impl Write, body: &str) {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin
        .write_all(header.as_bytes())
        .expect("write content-length header");
    stdin.write_all(body.as_bytes()).expect("write json body");
    stdin.flush().expect("flush request");
}

fn atm_agent_mcp_bin_path() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_atm-agent-mcp") {
        return PathBuf::from(path);
    }

    let mut path = std::env::current_exe().expect("resolve current_exe");
    path.pop(); // test binary
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("atm-agent-mcp");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

fn write_team_config(home: &std::path::Path) {
    let team_dir = home.join(".claude").join("teams").join("atm-dev");
    std::fs::create_dir_all(&team_dir).expect("create team dir");
    let config = serde_json::json!({
        "name": "atm-dev",
        "createdAt": 1770765919076_u64,
        "leadAgentId": "team-lead@atm-dev",
        "leadSessionId": "6075f866-f103-4be1-b2e9-8dbf66009eb9",
        "members": [
            {
                "agentId": "team-lead@atm-dev",
                "name": "team-lead",
                "agentType": "general-purpose",
                "model": "claude-haiku-4-5-20251001",
                "joinedAt": 1770765919076_u64,
                "tmuxPaneId": "",
                "cwd": "/tmp",
                "subscriptions": []
            },
            {
                "agentId": "arch-ctm@atm-dev",
                "name": "arch-ctm",
                "agentType": "general-purpose",
                "model": "gpt-5.2",
                "joinedAt": 1770765919077_u64,
                "tmuxPaneId": "",
                "cwd": "/tmp",
                "subscriptions": []
            }
        ]
    });
    std::fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).expect("serialize team config"),
    )
    .expect("write team config");
}

fn write_atm_config(path: &std::path::Path) {
    let config = r#"[core]
default_team = "atm-dev"
identity = "arch-ctm"

[plugins.atm-agent-mcp]
identity = "arch-ctm"
auto_mail = false
"#;
    std::fs::write(path, config).expect("write atm config");
}

fn read_jsonl_line(reader: &mut BufReader<std::process::ChildStdout>) -> Value {
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read stdout line");
        assert!(!line.is_empty(), "unexpected EOF waiting for JSONL response");
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        assert!(
            !trimmed.starts_with("Content-Length:"),
            "stdout must not contain Content-Length framing; got: {trimmed:?}"
        );
        return serde_json::from_str(trimmed).expect("valid jsonl response line");
    }
}

#[test]
fn serve_stdout_emits_only_newline_jsonrpc() {
    let bin = atm_agent_mcp_bin_path();
    assert!(
        bin.exists(),
        "atm-agent-mcp test binary not found at {}",
        bin.display()
    );

    let home = TempDir::new().expect("temp ATM_HOME");
    write_team_config(home.path());

    let config_path = home.path().join("test.atm.toml");
    write_atm_config(&config_path);

    let mut child = Command::new(&bin)
        .arg("--config")
        .arg(&config_path)
        .arg("serve")
        .env("ATM_HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn atm-agent-mcp serve");

    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let mut reader = BufReader::new(stdout);

    // 1) initialize
    let init_req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{}}}"#;
    write_content_length_request(&mut stdin, init_req);
    let init_resp = read_jsonl_line(&mut reader);
    assert_eq!(init_resp.get("jsonrpc").and_then(Value::as_str), Some("2.0"));
    assert_eq!(init_resp.get("id").and_then(Value::as_i64), Some(1));
    assert!(
        init_resp.get("result").is_some(),
        "initialize response should include result"
    );

    // 2) tools/call (standalone ATM tool path)
    let tool_req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"atm_pending_count","arguments":{}}}"#;
    write_content_length_request(&mut stdin, tool_req);
    let tool_resp = read_jsonl_line(&mut reader);
    assert_eq!(tool_resp.get("jsonrpc").and_then(Value::as_str), Some("2.0"));
    assert_eq!(tool_resp.get("id").and_then(Value::as_i64), Some(2));
    assert!(
        tool_resp.get("result").is_some() || tool_resp.get("error").is_some(),
        "tools/call response should be valid JSON-RPC success or error"
    );

    drop(stdin);

    let status = child
        .wait_timeout(Duration::from_secs(2))
        .expect("wait for child")
        .expect("child should exit after stdin closes");
    assert!(status.success(), "child exited with {status}");
}

trait ChildWaitTimeout {
    fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>>;
}

impl ChildWaitTimeout for std::process::Child {
    fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let start = std::time::Instant::now();
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
