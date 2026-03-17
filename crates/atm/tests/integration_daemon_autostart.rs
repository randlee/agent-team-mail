#[cfg(unix)]
use agent_team_mail_core::consts::WAIT_FOR_DAEMON_SOCKET_SECS;
use assert_cmd::cargo;
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
#[path = "support/daemon_process_guard.rs"]
#[allow(dead_code)]
mod daemon_process_guard;
#[cfg(unix)]
#[path = "support/daemon_test_registry.rs"]
#[allow(dead_code)]
mod daemon_test_registry;

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
daemon_dir = home / ".atm" / "daemon"
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
        elif command == "session-query-team" or command == "session-for-team":
            alive = os.environ.get("ATM_FAKE_SESSION_ALIVE", "false").lower() == "true"
            runtime = os.environ.get("ATM_FAKE_SESSION_RUNTIME", "codex")
            response_payload = {
                "team": payload.get("team", "unknown"),
                "agent": payload.get("name", payload.get("agent", "unknown")),
                "session_id": "fake-session",
                "process_id": os.getpid(),
                "alive": alive,
                "runtime": runtime,
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
        try:
            conn.sendall((json.dumps(response) + "\n").encode())
        except BrokenPipeError:
            # Client closed early; keep daemon running for subsequent requests.
            continue

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
    let socket = home.join(".atm/daemon/atm-daemon.sock");
    let deadline = Instant::now() + Duration::from_secs(WAIT_FOR_DAEMON_SOCKET_SECS);
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
fn daemon_pid_path(home: &Path) -> PathBuf {
    home.join(".atm/daemon/atm-daemon.pid")
}

#[cfg(unix)]
fn read_daemon_pid(temp_dir: &TempDir) -> Option<u32> {
    let raw = fs::read_to_string(daemon_pid_path(temp_dir.path())).ok()?;
    raw.trim().parse::<u32>().ok()
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

#[test]
#[cfg(unix)]
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
    wait_for_daemon_socket(home);
    let daemon_pid = read_daemon_pid(&temp).expect("status autostart should write daemon pid");
    let _daemon_guard =
        daemon_process_guard::DaemonProcessGuard::adopt_registered_pid(daemon_pid, &script, home);
    assert_eq!(
        spawn_count(home),
        1,
        "daemon should auto-start exactly once when absent"
    );
}

#[test]
#[cfg(unix)]
fn test_status_noops_when_daemon_already_healthy() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let team = "team-b";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);
    let daemon = Command::new(&script).env("ATM_HOME", home).spawn().unwrap();
    let _daemon_guard = daemon_process_guard::DaemonProcessGuard::from_child(
        daemon,
        std::path::Path::new(&script),
        home,
    );
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
}

#[test]
#[cfg(unix)]
fn test_concurrent_multi_team_status_uses_single_daemon_instance() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let teams = ["team-c1", "team-c2", "team-c3", "team-c4", "team-c5"];
    for team in teams {
        write_team_config(home, team);
    }
    let script = write_fake_daemon_script(home);
    let daemon = Command::new(&script).env("ATM_HOME", home).spawn().unwrap();
    let _daemon_guard = daemon_process_guard::DaemonProcessGuard::from_child(
        daemon,
        std::path::Path::new(&script),
        home,
    );
    wait_for_daemon_socket(home);
    assert_eq!(spawn_count(home), 1);

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
                .env("ATM_DAEMON_AUTOSTART", "0")
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
}

#[test]
#[cfg(unix)]
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
        output.status.success(),
        "status should remain best-effort when auto-start binary is missing"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"liveness\": null"),
        "status should report unknown liveness when daemon auto-start fails: {stdout}"
    );
}

#[test]
#[cfg(unix)]
fn test_daemon_kill_autostarts_daemon_when_absent() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let team = "team-kill";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    let output = cmd
        .env("ATM_HOME", home)
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", &script)
        .env("ATM_FAKE_SESSION_ALIVE", "false")
        .arg("daemon")
        .arg("--kill")
        .arg("alice")
        .arg("--team")
        .arg(team)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "daemon --kill should succeed with autostart: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    wait_for_daemon_socket(home);
    let daemon_pid =
        read_daemon_pid(&temp).expect("daemon --kill autostart should write daemon pid");
    let _daemon_guard =
        daemon_process_guard::DaemonProcessGuard::adopt_registered_pid(daemon_pid, &script, home);
    assert_eq!(spawn_count(home), 1, "daemon should autostart for --kill");
}

#[test]
#[cfg(unix)]
fn test_cleanup_agent_autostarts_daemon_when_absent() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let team = "team-cleanup";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    let output = cmd
        .env("ATM_HOME", home)
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", &script)
        .arg("cleanup")
        .arg("--agent")
        .arg("alice")
        .arg("--team")
        .arg(team)
        .arg("--force")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "cleanup --agent should succeed with autostart: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    wait_for_daemon_socket(home);
    let daemon_pid =
        read_daemon_pid(&temp).expect("cleanup --agent autostart should write daemon pid");
    let _daemon_guard =
        daemon_process_guard::DaemonProcessGuard::adopt_registered_pid(daemon_pid, &script, home);
    assert_eq!(spawn_count(home), 1, "daemon should autostart for cleanup");
}

#[test]
#[cfg(unix)]
fn test_doctor_no_daemon_not_running_after_status_autostart() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let team = "team-doctor";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);

    let mut status_cmd = cargo::cargo_bin_cmd!("atm");
    status_cmd
        .env("ATM_HOME", home)
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", &script)
        .env("ATM_FAKE_SESSION_ALIVE", "true")
        .arg("status")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .assert()
        .success();
    wait_for_daemon_socket(home);
    let daemon_pid =
        read_daemon_pid(&temp).expect("status autostart should write daemon pid for doctor");
    let _daemon_guard =
        daemon_process_guard::DaemonProcessGuard::adopt_registered_pid(daemon_pid, &script, home);
    assert_eq!(
        spawn_count(home),
        1,
        "status should auto-start exactly one daemon"
    );

    let mut doctor_cmd = cargo::cargo_bin_cmd!("atm");
    let output = doctor_cmd
        .env("ATM_HOME", home)
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", &script)
        .env("ATM_FAKE_SESSION_ALIVE", "true")
        .arg("doctor")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "doctor should succeed after daemon autostart: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let has_daemon_not_running = report["findings"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .any(|f| f["code"].as_str() == Some("DAEMON_NOT_RUNNING"))
        })
        .unwrap_or(false);
    assert!(
        !has_daemon_not_running,
        "doctor must not report DAEMON_NOT_RUNNING after status autostart"
    );
}

#[test]
#[cfg(windows)]
fn windows_compile_check() {
    // Compile-check placeholder for Windows targets: unix-only tests/helpers are
    // gated per-function to keep this integration test file cross-platform.
    let _ = cargo::cargo_bin("atm");
}
