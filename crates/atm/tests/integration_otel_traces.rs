use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use serial_test::serial;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn setup_team(home: &Path, team: &str) {
    let team_dir = home.join(".claude/teams").join(team);
    fs::create_dir_all(team_dir.join("inboxes")).expect("create team dirs");
    fs::write(
        team_dir.join("config.json"),
        serde_json::json!({
            "name": team,
            "description": "test team",
            "createdAt": 1739284800000u64,
            "leadAgentId": format!("team-lead@{team}"),
            "leadSessionId": "sess-team-lead",
            "members": [
                {
                    "agentId": format!("team-lead@{team}"),
                    "name": "team-lead",
                    "agentType": "team-lead",
                    "model": "claude-sonnet-4-6",
                    "joinedAt": 1739284800000u64,
                    "cwd": ".",
                    "subscriptions": []
                }
            ]
        })
        .to_string(),
    )
    .expect("write team config");
}

fn setup_daemon_status(home: &Path) {
    let daemon_dir = home.join(".atm/daemon");
    fs::create_dir_all(&daemon_dir).expect("create daemon dir");
    fs::write(
        daemon_dir.join("status.json"),
        serde_json::json!({
            "timestamp": "2026-03-18T00:00:00Z",
            "pid": 4242,
            "version": "0.45.0",
            "uptime_secs": 1,
            "plugins": [],
            "teams": ["atm-dev"],
            "logging": {
                "state": "healthy",
                "dropped_counter": 0,
                "spool_path": home.join(".atm/log-spool").to_string_lossy(),
                "last_error": null,
                "canonical_log_path": home.join(".atm/atm.log.jsonl").to_string_lossy(),
                "spool_count": 0,
                "oldest_spool_age": null
            },
            "otel": {
                "schema_version": "v1",
                "enabled": true,
                "collector_endpoint": "http://collector:4318",
                "protocol": "otlp_http",
                "collector_state": "healthy",
                "local_mirror_state": "healthy",
                "local_mirror_path": home.join(".atm/atm.log.otel.jsonl").to_string_lossy(),
                "debug_local_export": false,
                "debug_local_state": "disabled",
                "last_error": {
                    "code": null,
                    "message": null,
                    "at": null
                }
            }
        })
        .to_string(),
    )
    .expect("write daemon status");
}

fn start_collector() -> (String, mpsc::Receiver<(String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind collector");
    listener
        .set_nonblocking(false)
        .expect("collector blocking mode");
    let addr = listener.local_addr().expect("collector addr");
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        for _ in 0..4 {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 1024];
            let mut header_end = None;
            while header_end.is_none() {
                let read = stream.read(&mut chunk).expect("read request");
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                header_end = buffer.windows(4).position(|window| window == b"\r\n\r\n");
            }
            let Some(header_end_idx) = header_end else {
                continue;
            };
            let body_start = header_end_idx + 4;
            let headers = String::from_utf8_lossy(&buffer[..header_end_idx]);
            let first_line = headers.lines().next().unwrap_or_default().to_string();
            let path = first_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    (name.eq_ignore_ascii_case("content-length"))
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);

            while buffer.len().saturating_sub(body_start) < content_length {
                let read = stream.read(&mut chunk).expect("read request body");
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
            }

            let body = String::from_utf8_lossy(&buffer[body_start..body_start + content_length])
                .to_string();
            tx.send((path, body)).expect("send captured request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .expect("write response");
        }
    });

    (format!("http://{}", addr), rx)
}

#[test]
#[serial]
fn cli_status_exports_trace_record_to_collector() {
    let temp = TempDir::new().expect("temp dir");
    setup_team(temp.path(), "atm-dev");
    setup_daemon_status(temp.path());
    let (endpoint, rx) = start_collector();

    let mut cmd = Command::new(cargo_bin("atm"));
    cmd.env("ATM_HOME", temp.path())
        .env("ATM_TEAM", "atm-dev")
        .env("ATM_IDENTITY", "arch-ctm")
        .env("ATM_RUNTIME", "codex")
        .env("CLAUDE_SESSION_ID", "sess-123")
        .env("ATM_LOG", "0")
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env("ATM_OTEL_ENABLED", "true")
        .env("ATM_OTEL_ENDPOINT", endpoint)
        .args(["status", "--json"]);

    let output = cmd.output().expect("run atm status");
    assert!(
        output.status.success(),
        "status command failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let (path, body) = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("collector request");
    assert_eq!(path, "/v1/traces");

    let payload: Value = serde_json::from_str(&body).expect("valid traces payload");
    let span = &payload["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
    assert_eq!(span["name"], "atm.command.status");
    assert_eq!(span["traceId"].as_str().is_some(), true);
    assert_eq!(span["spanId"].as_str().is_some(), true);

    let attrs = payload["resourceSpans"][0]["resource"]["attributes"]
        .as_array()
        .expect("resource attributes");
    assert!(
        attrs
            .iter()
            .any(|item| { item["key"] == "team" && item["value"]["stringValue"] == "atm-dev" })
    );
    assert!(
        attrs
            .iter()
            .any(|item| { item["key"] == "agent" && item["value"]["stringValue"] == "arch-ctm" })
    );
}

#[test]
#[serial]
fn cli_status_trace_export_is_fail_open_when_collector_unreachable() {
    let temp = TempDir::new().expect("temp dir");
    setup_team(temp.path(), "atm-dev");
    setup_daemon_status(temp.path());

    let mut cmd = Command::new(cargo_bin("atm"));
    cmd.env("ATM_HOME", temp.path())
        .env("ATM_TEAM", "atm-dev")
        .env("ATM_IDENTITY", "arch-ctm")
        .env("ATM_RUNTIME", "codex")
        .env("CLAUDE_SESSION_ID", "sess-123")
        .env("ATM_LOG", "0")
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env("ATM_OTEL_ENABLED", "true")
        .env("ATM_OTEL_ENDPOINT", "http://127.0.0.1:1")
        .args(["status", "--json"]);

    let output = cmd.output().expect("run atm status");
    assert!(
        output.status.success(),
        "trace export failure must not fail the command: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
