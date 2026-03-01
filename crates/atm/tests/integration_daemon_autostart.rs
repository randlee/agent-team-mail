use assert_cmd::cargo;
#[cfg(unix)]
use serial_test::serial;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
#[cfg(unix)]
use std::time::{Duration, Instant};
#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
fn write_team_config(home: &Path, team: &str) {
    let team_dir = home.join(".claude/teams").join(team);
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();
    let config = serde_json::json!({
        "name": team,
        "createdAt": 1770765919076u64,
        "leadAgentId": format!("team-lead@{team}"),
        "leadSessionId": "6075f866-f103-4be1-b2e9-8dbf66009eb9",
        "members": [
            {
                "agentId": format!("alice@{team}"),
                "name": "alice",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1770765919076u64,
                "cwd": "/test",
                "subscriptions": [],
                "isActive": true
            }
        ]
    });
    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
}

#[cfg(unix)]
fn write_fake_daemon_script(home: &Path) -> PathBuf {
    let script = home.join("fake-daemon.py");
    let body = r#"#!/usr/bin/env python3
import json
import os
import signal
import socket
from pathlib import Path

home = Path(os.environ["ATM_HOME"])
daemon_dir = home / ".claude" / "daemon"
daemon_dir.mkdir(parents=True, exist_ok=True)
marker_dir = home / "spawn-markers"
marker_dir.mkdir(parents=True, exist_ok=True)
(marker_dir / f"spawn-{os.getpid()}").write_text("1")

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
srv.listen(128)
srv.settimeout(0.2)

while running:
    try:
        conn, _ = srv.accept()
    except TimeoutError:
        continue
    except OSError:
        break
    with conn:
        buf = b""
        while b"\n" not in buf:
            chunk = conn.recv(4096)
            if not chunk:
                break
            buf += chunk
        try:
            req = json.loads(buf.decode().strip() or "{}")
        except Exception:
            req = {}
        request_id = req.get("request_id", "req")
        command = req.get("command", "")
        payload = req.get("payload", {}) or {}
        if command == "list-agents":
            response_payload = []
        elif command == "session-for-team":
            response_payload = {
                "team": payload.get("team", "unknown"),
                "agent": payload.get("agent", "unknown"),
                "session_id": "fake-session",
                "process_id": os.getpid(),
                "alive": True,
            }
        elif command == "agent-state":
            response_payload = {"state": "idle", "last_transition": None}
        else:
            response_payload = {}
        response = {
            "version": 1,
            "request_id": request_id,
            "status": "ok",
            "payload": response_payload,
        }
        conn.sendall((json.dumps(response) + "\n").encode())

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
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "fake daemon socket was not created in time: {}",
        socket.display()
    );
}

#[cfg(unix)]
fn spawn_count(home: &Path) -> usize {
    fs::read_dir(home.join("spawn-markers"))
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .count()
}

#[cfg(unix)]
fn kill_pid_from_file(home: &Path) {
    let pid_path = home.join(".claude/daemon/atm-daemon.pid");
    if let Ok(content) = fs::read_to_string(pid_path)
        && let Ok(pid) = content.trim().parse::<i32>()
    {
        // SAFETY: test teardown sends SIGTERM to a process that this test launched.
        let _ = unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

#[test]
#[cfg(unix)]
#[serial]
fn test_status_autostarts_daemon_when_absent() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let team = "team-a";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    let output = cmd
        .env("ATM_HOME", home)
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", &script)
        .arg("status")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "status command should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        spawn_count(home),
        1,
        "daemon should auto-start exactly once when absent"
    );

    kill_pid_from_file(home);
}

#[test]
#[cfg(unix)]
#[serial]
fn test_status_noops_when_daemon_already_healthy() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let team = "team-b";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);
    let mut daemon = Command::new(&script).env("ATM_HOME", home).spawn().unwrap();
    wait_for_daemon_socket(home);
    assert_eq!(spawn_count(home), 1);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    let output = cmd
        .env("ATM_HOME", home)
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", &script)
        .arg("status")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "status command should succeed when daemon already healthy: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        spawn_count(home),
        1,
        "healthy daemon should not be re-spawned"
    );

    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
#[cfg(unix)]
#[serial]
fn test_concurrent_multi_team_status_uses_single_daemon_instance() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let teams = ["team-c1", "team-c2", "team-c3", "team-c4", "team-c5"];
    for team in teams {
        write_team_config(home, team);
    }
    let script = write_fake_daemon_script(home);
    let mut threads = Vec::new();
    for team in teams {
        let home = home.to_path_buf();
        let script = script.clone();
        threads.push(std::thread::spawn(move || {
            let mut cmd = cargo::cargo_bin_cmd!("atm");
            let output = cmd
                .env("ATM_HOME", &home)
                .env("ATM_TEAM", team)
                .env("ATM_DAEMON_BIN", &script)
                .arg("status")
                .arg("--team")
                .arg(team)
                .arg("--json")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "status failed for {team}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }));
    }
    for t in threads {
        t.join().unwrap();
    }

    assert_eq!(
        spawn_count(home),
        1,
        "concurrent daemon-backed commands across teams must share one daemon"
    );
    kill_pid_from_file(home);
}

#[test]
#[cfg(unix)]
#[serial]
fn test_status_reports_actionable_error_when_autostart_binary_missing() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let team = "team-missing-bin";
    write_team_config(home, team);
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    let output = cmd
        .env("ATM_HOME", home)
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", "/definitely-missing-atm-daemon-binary")
        .arg("status")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "status should fail when auto-start binary is missing"
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("failed to auto-start daemon")
            || combined.contains("not found in PATH"),
        "expected actionable auto-start failure message, got: {combined}"
    );
}

#[test]
#[cfg(windows)]
fn windows_compile_check() {
    // Compile-check placeholder for Windows targets: unix-only tests/helpers are
    // gated per-function to keep this integration test file cross-platform.
    let _ = cargo::cargo_bin("atm");
}
