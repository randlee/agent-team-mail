//! Integration tests for `atm gh ...` daemon-routed commands.

use assert_cmd::cargo;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Child, Command};
#[cfg(unix)]
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).ok();
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env_remove("ATM_TEAM")
        .env_remove("ATM_CONFIG")
        .env_remove("CLAUDE_SESSION_ID")
        .env("ATM_IDENTITY", "team-lead")
        .current_dir(&workdir);
}

fn setup_test_team(temp_dir: &TempDir, team_name: &str) -> PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    let config = serde_json::json!({
        "name": team_name,
        "description": "Test team",
        "createdAt": 1739284800000i64,
        "leadAgentId": format!("team-lead@{}", team_name),
        "leadSessionId": "test-session-id",
        "members": [{
            "agentId": format!("team-lead@{}", team_name),
            "name": "team-lead",
            "agentType": "general-purpose",
            "model": "claude-sonnet-4-6",
            "joinedAt": 1739284800000i64,
            "cwd": temp_dir.path().to_str().unwrap(),
            "subscriptions": []
        }]
    });
    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    team_dir
}

#[cfg(unix)]
fn write_fake_gh_daemon_script(home: &Path) -> PathBuf {
    let script = home.join("fake-gh-daemon.py");
    let body = r#"#!/usr/bin/env python3
import json
import os
import signal
import socket
from pathlib import Path

home = Path(os.environ["ATM_HOME"])
daemon_dir = home / ".claude" / "daemon"
daemon_dir.mkdir(parents=True, exist_ok=True)
state_path = daemon_dir / "gh-state.json"

sock_path = daemon_dir / "atm-daemon.sock"
pid_path = daemon_dir / "atm-daemon.pid"
if sock_path.exists():
    sock_path.unlink()
pid_path.write_text(str(os.getpid()))

running = True
def _stop(_signum, _frame):
    global running
    running = False

signal.signal(signal.SIGTERM, _stop)
signal.signal(signal.SIGINT, _stop)

srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(str(sock_path))
srv.listen(16)
srv.settimeout(0.2)

while running:
    try:
        conn, _ = srv.accept()
    except TimeoutError:
        continue
    except OSError:
        break
    with conn:
        data = b""
        while b"\n" not in data:
            chunk = conn.recv(4096)
            if not chunk:
                break
            data += chunk
        try:
            req = json.loads(data.decode().strip() or "{}")
        except Exception:
            req = {}

        request_id = req.get("request_id", "req")
        command = req.get("command", "")
        payload = req.get("payload", {}) or {}

        if command == "gh-monitor":
            status_payload = {
                "team": payload.get("team", "test-team"),
                "target_kind": payload.get("target_kind", "workflow"),
                "target": payload.get("target", "ci"),
                "state": "tracking",
                "run_id": 987654,
                "reference": payload.get("reference"),
                "updated_at": "2026-03-06T03:00:00Z",
                "message": None,
            }
            state_path.write_text(json.dumps(status_payload))
            resp = {"version": 1, "request_id": request_id, "status": "ok", "payload": status_payload}
        elif command == "gh-status":
            if state_path.exists():
                status_payload = json.loads(state_path.read_text())
            else:
                status_payload = {
                    "team": payload.get("team", "test-team"),
                    "target_kind": payload.get("target_kind", "workflow"),
                    "target": payload.get("target", "ci"),
                    "state": "tracking",
                    "run_id": 987654,
                    "reference": "develop",
                    "updated_at": "2026-03-06T03:00:00Z",
                    "message": None,
                }
            resp = {"version": 1, "request_id": request_id, "status": "ok", "payload": status_payload}
        elif command == "status":
            resp = {"version": 1, "request_id": request_id, "status": "ok", "payload": {"state":"running"}}
        else:
            resp = {"version": 1, "request_id": request_id, "status": "ok", "payload": {}}

        conn.sendall((json.dumps(resp) + "\n").encode())

try:
    srv.close()
finally:
    try:
        sock_path.unlink()
    except FileNotFoundError:
        pass
    try:
        pid_path.unlink()
    except FileNotFoundError:
        pass
"#;
    fs::write(&script, body).unwrap();
    let mut perms = fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();
    script
}

#[cfg(unix)]
fn wait_for_daemon_socket(home: &Path) {
    let socket = home.join(".claude/daemon/atm-daemon.sock");
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if socket.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "fake daemon socket was not created in time: {}",
        socket.display()
    );
}

#[cfg(unix)]
fn start_fake_gh_daemon(home: &Path) -> Child {
    let script = write_fake_gh_daemon_script(home);
    let child = Command::new(&script).env("ATM_HOME", home).spawn().unwrap();
    wait_for_daemon_socket(home);
    child
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_workflow_roundtrip_json() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let mut daemon = start_fake_gh_daemon(temp_dir.path());

    let mut monitor = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut monitor, &temp_dir);
    let monitor_output = monitor
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("monitor")
        .arg("workflow")
        .arg("ci")
        .arg("--ref")
        .arg("develop")
        .arg("--start-timeout")
        .arg("30")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let monitor_json: serde_json::Value = serde_json::from_slice(&monitor_output).unwrap();
    assert_eq!(monitor_json["team"].as_str(), Some("test-team"));
    assert_eq!(monitor_json["target_kind"].as_str(), Some("workflow"));
    assert_eq!(monitor_json["target"].as_str(), Some("ci"));
    assert_eq!(monitor_json["reference"].as_str(), Some("develop"));
    assert_eq!(monitor_json["run_id"].as_u64(), Some(987654));
    assert_eq!(monitor_json["state"].as_str(), Some("tracking"));

    let mut status = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut status, &temp_dir);
    let status_output = status
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("status")
        .arg("workflow")
        .arg("ci")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status_json: serde_json::Value = serde_json::from_slice(&status_output).unwrap();
    assert_eq!(status_json["target_kind"].as_str(), Some("workflow"));
    assert_eq!(status_json["target"].as_str(), Some("ci"));
    assert_eq!(status_json["run_id"].as_u64(), Some(987654));
    assert_eq!(status_json["state"].as_str(), Some("tracking"));

    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
#[cfg(windows)]
fn test_gh_command_surface_compiles_on_windows() {
    let _ = agent_team_mail_core::daemon_client::gh_monitor;
    let _ = agent_team_mail_core::daemon_client::gh_status;
}
