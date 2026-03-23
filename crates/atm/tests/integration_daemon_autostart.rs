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
            custom_payload = os.environ.get("ATM_FAKE_LIST_AGENTS")
            if custom_payload:
                try:
                    response_payload = json.loads(custom_payload)
                except Exception:
                    response_payload = []
            else:
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
fn read_daemon_pid(home: &Path) -> Option<u32> {
    let raw = fs::read_to_string(daemon_pid_path(home)).ok()?;
    raw.trim().parse::<u32>().ok()
}

#[cfg(unix)]
fn runtime_home(temp: &TempDir) -> PathBuf {
    temp.path().to_path_buf()
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
fn fake_member_states_json(agent: &str, process_id: u32) -> String {
    serde_json::json!([
        {
            "agent": agent,
            "state": "active",
            "activity": "busy",
            "session_id": "fake-session",
            "process_id": process_id,
            "last_alive_at": "2026-03-20T22:00:00Z",
            "reason": "session active",
            "source": "session_registry",
            "in_config": true
        }
    ])
    .to_string()
}
#[test]
#[cfg(unix)]
fn test_status_autostarts_daemon_when_absent() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let runtime_home = runtime_home(&temp);
    let team = "team-a";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    let output = cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
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
    wait_for_daemon_socket(&runtime_home);
    let daemon_pid =
        read_daemon_pid(&runtime_home).expect("status autostart should write daemon pid");
    let _daemon_guard = daemon_process_guard::DaemonProcessGuard::adopt_registered_pid(
        daemon_pid,
        &script,
        &runtime_home,
    );
    assert_eq!(
        spawn_count(&runtime_home),
        1,
        "daemon should auto-start exactly once when absent"
    );
}

#[test]
#[cfg(unix)]
fn test_status_noops_when_daemon_already_healthy() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let runtime_home = runtime_home(&temp);
    let team = "team-b";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);
    let daemon = Command::new(&script)
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .spawn()
        .unwrap();
    let _daemon_guard = daemon_process_guard::DaemonProcessGuard::from_child(
        daemon,
        std::path::Path::new(&script),
        &runtime_home,
    );
    wait_for_daemon_socket(&runtime_home);
    assert_eq!(spawn_count(&runtime_home), 1);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    let output = cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
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
        spawn_count(&runtime_home),
        1,
        "healthy daemon should not be re-spawned"
    );
}

#[test]
#[cfg(unix)]
fn test_concurrent_multi_team_status_uses_single_daemon_instance() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let runtime_home = runtime_home(&temp);
    let teams = ["team-c1", "team-c2", "team-c3", "team-c4", "team-c5"];
    for team in teams {
        write_team_config(home, team);
    }
    let script = write_fake_daemon_script(home);
    let daemon = Command::new(&script)
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .spawn()
        .unwrap();
    let _daemon_guard = daemon_process_guard::DaemonProcessGuard::from_child(
        daemon,
        std::path::Path::new(&script),
        &runtime_home,
    );
    wait_for_daemon_socket(&runtime_home);
    assert_eq!(spawn_count(&runtime_home), 1);

    let mut threads = Vec::new();
    for team in teams {
        let home = home.to_path_buf();
        let runtime_home = runtime_home.clone();
        let script = script.clone();
        threads.push(std::thread::spawn(move || {
            let mut cmd = cargo::cargo_bin_cmd!("atm");
            let output = cmd
                .env("ATM_HOME", &runtime_home)
                .env("ATM_CONFIG_HOME", &home)
                .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
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
        spawn_count(&runtime_home),
        1,
        "concurrent daemon-backed commands across teams must share one daemon"
    );
}

#[test]
#[cfg(unix)]
fn test_status_reports_actionable_error_when_autostart_binary_missing() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let runtime_home = runtime_home(&temp);
    let team = "team-missing-bin";
    write_team_config(home, team);
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    let output = cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
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
    let runtime_home = runtime_home(&temp);
    let team = "team-kill";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    let output = cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
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
    wait_for_daemon_socket(&runtime_home);
    let daemon_pid =
        read_daemon_pid(&runtime_home).expect("daemon --kill autostart should write daemon pid");
    let _daemon_guard = daemon_process_guard::DaemonProcessGuard::adopt_registered_pid(
        daemon_pid,
        &script,
        &runtime_home,
    );
    assert_eq!(
        spawn_count(&runtime_home),
        1,
        "daemon should autostart for --kill"
    );
}

#[test]
#[cfg(unix)]
fn test_cleanup_agent_autostarts_daemon_when_absent() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let runtime_home = runtime_home(&temp);
    let team = "team-cleanup";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    let output = cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
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
    wait_for_daemon_socket(&runtime_home);
    let daemon_pid =
        read_daemon_pid(&runtime_home).expect("cleanup --agent autostart should write daemon pid");
    let _daemon_guard = daemon_process_guard::DaemonProcessGuard::adopt_registered_pid(
        daemon_pid,
        &script,
        &runtime_home,
    );
    assert_eq!(
        spawn_count(&runtime_home),
        1,
        "daemon should autostart for cleanup"
    );
}

#[test]
#[cfg(unix)]
fn test_doctor_no_daemon_not_running_after_status_autostart() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let runtime_home = runtime_home(&temp);
    let team = "team-doctor";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);

    let mut status_cmd = cargo::cargo_bin_cmd!("atm");
    status_cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", &script)
        .env("ATM_FAKE_SESSION_ALIVE", "true")
        .arg("status")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .assert()
        .success();
    wait_for_daemon_socket(&runtime_home);
    let daemon_pid = read_daemon_pid(&runtime_home)
        .expect("status autostart should write daemon pid for doctor");
    let _daemon_guard = daemon_process_guard::DaemonProcessGuard::adopt_registered_pid(
        daemon_pid,
        &script,
        &runtime_home,
    );
    assert_eq!(
        spawn_count(&runtime_home),
        1,
        "status should auto-start exactly one daemon"
    );

    let mut doctor_cmd = cargo::cargo_bin_cmd!("atm");
    let output = doctor_cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
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
#[cfg(unix)]
fn test_doctor_distinguishes_absent_daemon_from_pid_verification_failure() {
    let absent_home = TempDir::new().unwrap();
    let absent_runtime_home = runtime_home(&absent_home);
    write_team_config(absent_home.path(), "team-absent");

    let mut absent_cmd = cargo::cargo_bin_cmd!("atm");
    let absent_output = absent_cmd
        .env("ATM_HOME", &absent_runtime_home)
        .env("ATM_CONFIG_HOME", absent_home.path())
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", "team-absent")
        .env("ATM_DAEMON_AUTOSTART", "0")
        .arg("doctor")
        .arg("--team")
        .arg("team-absent")
        .output()
        .unwrap();
    assert_eq!(absent_output.status.code(), Some(2));
    let absent_stdout = String::from_utf8_lossy(&absent_output.stdout);
    assert!(
        absent_stdout
            .contains("Daemon is not running: no live daemon PID file or socket was found"),
        "unexpected absent-daemon output: {absent_stdout}"
    );
    let mut absent_json_cmd = cargo::cargo_bin_cmd!("atm");
    let absent_json_output = absent_json_cmd
        .env("ATM_HOME", &absent_runtime_home)
        .env("ATM_CONFIG_HOME", absent_home.path())
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", "team-absent")
        .env("ATM_DAEMON_AUTOSTART", "0")
        .arg("doctor")
        .arg("--team")
        .arg("team-absent")
        .arg("--json")
        .output()
        .unwrap();
    let absent_value: serde_json::Value =
        serde_json::from_slice(&absent_json_output.stdout).unwrap();
    let absent_codes: Vec<_> = absent_value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|finding| finding["code"].as_str())
        .collect();
    assert!(absent_codes.contains(&"DAEMON_NOT_RUNNING"));
    assert!(!absent_codes.contains(&"DAEMON_PID_UNVERIFIABLE"));

    let stale_home = TempDir::new().unwrap();
    let stale_runtime_home = runtime_home(&stale_home);
    write_team_config(stale_home.path(), "team-stale");
    let daemon_dir = stale_runtime_home.join(".atm/daemon");
    fs::create_dir_all(&daemon_dir).unwrap();
    fs::write(
        daemon_dir.join("status.json"),
        serde_json::json!({
            "pid": 999_991_u32,
            "version": env!("CARGO_PKG_VERSION"),
        })
        .to_string(),
    )
    .unwrap();

    let mut stale_cmd = cargo::cargo_bin_cmd!("atm");
    let stale_output = stale_cmd
        .env("ATM_HOME", &stale_runtime_home)
        .env("ATM_CONFIG_HOME", stale_home.path())
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", "team-stale")
        .env("ATM_DAEMON_AUTOSTART", "0")
        .arg("doctor")
        .arg("--team")
        .arg("team-stale")
        .output()
        .unwrap();
    assert_eq!(stale_output.status.code(), Some(2));
    let stale_stdout = String::from_utf8_lossy(&stale_output.stdout);
    assert!(
        stale_stdout.contains("PID cannot be verified"),
        "unexpected stale-daemon output: {stale_stdout}"
    );
    let mut stale_json_cmd = cargo::cargo_bin_cmd!("atm");
    let stale_json_output = stale_json_cmd
        .env("ATM_HOME", &stale_runtime_home)
        .env("ATM_CONFIG_HOME", stale_home.path())
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", "team-stale")
        .env("ATM_DAEMON_AUTOSTART", "0")
        .arg("doctor")
        .arg("--team")
        .arg("team-stale")
        .arg("--json")
        .output()
        .unwrap();
    let stale_value: serde_json::Value = serde_json::from_slice(&stale_json_output.stdout).unwrap();
    let stale_codes: Vec<_> = stale_value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|finding| finding["code"].as_str())
        .collect();
    assert!(stale_codes.contains(&"DAEMON_PID_UNVERIFIABLE"));
    assert!(!stale_codes.contains(&"DAEMON_NOT_RUNNING"));
}

#[test]
#[cfg(unix)]
fn test_members_reports_status_session_and_pid_after_daemon_autostart() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let runtime_home = runtime_home(&temp);
    let team = "team-members";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);

    let mut members_cmd = cargo::cargo_bin_cmd!("atm");
    let output = members_cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", &script)
        .env(
            "ATM_FAKE_LIST_AGENTS",
            fake_member_states_json("alice", 4242),
        )
        .arg("members")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "members should succeed after daemon autostart: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    wait_for_daemon_socket(&runtime_home);
    let daemon_pid =
        read_daemon_pid(&runtime_home).expect("members autostart should write daemon pid");
    let _daemon_guard = daemon_process_guard::DaemonProcessGuard::adopt_registered_pid(
        daemon_pid,
        &script,
        &runtime_home,
    );

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let member = value["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"].as_str() == Some("alice"))
        .expect("alice row");
    assert_eq!(member["status"].as_str(), Some("Active"));
    assert_eq!(member["activity"].as_str(), Some("Busy"));
    assert_eq!(member["sessionId"].as_str(), Some("fake-session"));
    assert_eq!(member["processId"].as_u64(), Some(4242));
    assert_eq!(member["lastAliveAt"].as_str(), Some("2026-03-20T22:00:00Z"));
}

#[test]
#[cfg(unix)]
fn test_state_surfaces_show_master_record_then_explicit_unavailable_after_shutdown() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let runtime_home = runtime_home(&temp);
    let team = "team-state-truth";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);

    let mut status_cmd = cargo::cargo_bin_cmd!("atm");
    let status_output = status_cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", &script)
        .env(
            "ATM_FAKE_LIST_AGENTS",
            fake_member_states_json("alice", 4242),
        )
        .arg("status")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .output()
        .unwrap();
    assert!(status_output.status.success(), "status should succeed");
    let status_json: serde_json::Value = serde_json::from_slice(&status_output.stdout).unwrap();
    assert_eq!(
        status_json["daemon_state"]["availability"].as_str(),
        Some("available")
    );
    assert_eq!(status_json["members"][0]["liveness"].as_bool(), Some(true));

    wait_for_daemon_socket(&runtime_home);
    let daemon_pid = read_daemon_pid(&runtime_home).expect("state-truth daemon pid");
    let daemon_guard = daemon_process_guard::DaemonProcessGuard::adopt_registered_pid(
        daemon_pid,
        &script,
        &runtime_home,
    );

    let mut members_cmd = cargo::cargo_bin_cmd!("atm");
    let members_output = members_cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", &script)
        .env(
            "ATM_FAKE_LIST_AGENTS",
            fake_member_states_json("alice", 4242),
        )
        .arg("members")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .output()
        .unwrap();
    assert!(members_output.status.success(), "members should succeed");
    let members_json: serde_json::Value = serde_json::from_slice(&members_output.stdout).unwrap();
    assert_eq!(
        members_json["daemonState"]["availability"].as_str(),
        Some("available")
    );
    assert_eq!(
        members_json["members"][0]["sessionId"].as_str(),
        Some("fake-session")
    );
    assert_eq!(members_json["members"][0]["processId"].as_u64(), Some(4242));

    let mut doctor_cmd = cargo::cargo_bin_cmd!("atm");
    let doctor_output = doctor_cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", &script)
        .env(
            "ATM_FAKE_LIST_AGENTS",
            fake_member_states_json("alice", 4242),
        )
        .arg("doctor")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        doctor_output.status.success() || doctor_output.status.code() == Some(2),
        "doctor should emit a report when daemon state is healthy"
    );
    let doctor_json: serde_json::Value = serde_json::from_slice(&doctor_output.stdout).unwrap();
    assert_eq!(
        doctor_json["daemon_state"]["availability"].as_str(),
        Some("available")
    );
    assert_eq!(
        doctor_json["members"][0]["session_id"].as_str(),
        Some("fake-session")
    );
    assert_eq!(doctor_json["members"][0]["status"].as_str(), Some("Online"));

    drop(daemon_guard);

    let mut status_down_cmd = cargo::cargo_bin_cmd!("atm");
    let status_down = status_down_cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_AUTOSTART", "0")
        .arg("status")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        status_down.status.success(),
        "status should stay best-effort"
    );
    let status_down_json: serde_json::Value = serde_json::from_slice(&status_down.stdout).unwrap();
    assert_eq!(
        status_down_json["daemon_state"]["availability"].as_str(),
        Some("unavailable")
    );

    let mut members_down_cmd = cargo::cargo_bin_cmd!("atm");
    let members_down = members_down_cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_AUTOSTART", "0")
        .arg("members")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        members_down.status.success(),
        "members should stay best-effort"
    );
    let members_down_json: serde_json::Value =
        serde_json::from_slice(&members_down.stdout).unwrap();
    assert_eq!(
        members_down_json["daemonState"]["availability"].as_str(),
        Some("unavailable")
    );

    let mut doctor_down_cmd = cargo::cargo_bin_cmd!("atm");
    let doctor_down = doctor_down_cmd
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_AUTOSTART", "0")
        .arg("doctor")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(doctor_down.status.code(), Some(2));
    let doctor_down_json: serde_json::Value = serde_json::from_slice(&doctor_down.stdout).unwrap();
    assert_eq!(
        doctor_down_json["daemon_state"]["availability"].as_str(),
        Some("unavailable")
    );
    let finding_codes = doctor_down_json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|finding| finding["code"].as_str())
        .collect::<Vec<_>>();
    assert!(finding_codes.contains(&"DAEMON_UNREACHABLE"));
}

#[test]
#[cfg(unix)]
fn test_status_autostart_recovers_after_stale_restart_cycle() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let runtime_home = runtime_home(&temp);
    let team = "team-restart";
    write_team_config(home, team);
    let script = write_fake_daemon_script(home);

    let mut first_status = cargo::cargo_bin_cmd!("atm");
    first_status
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", &script)
        .arg("status")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .assert()
        .success();
    wait_for_daemon_socket(&runtime_home);
    let first_pid = read_daemon_pid(&runtime_home).expect("initial daemon pid");
    Command::new("kill")
        .args(["-9", &first_pid.to_string()])
        .status()
        .expect("kill stale daemon");
    daemon_process_guard::wait_for_pid_exit(first_pid as i32, Duration::from_secs(5));

    let mut second_status = cargo::cargo_bin_cmd!("atm");
    second_status
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", home)
        .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
        .env("ATM_TEAM", team)
        .env("ATM_DAEMON_BIN", &script)
        .arg("status")
        .arg("--team")
        .arg(team)
        .arg("--json")
        .assert()
        .success();
    wait_for_daemon_socket(&runtime_home);
    let second_pid = read_daemon_pid(&runtime_home).expect("restarted daemon pid");
    assert_ne!(
        second_pid, first_pid,
        "autostart should replace the stale daemon pid"
    );
    let _daemon_guard = daemon_process_guard::DaemonProcessGuard::adopt_registered_pid(
        second_pid,
        &script,
        &runtime_home,
    );
}

#[test]
#[cfg(windows)]
fn windows_compile_check() {
    // Compile-check placeholder for Windows targets: unix-only tests/helpers are
    // gated per-function to keep this integration test file cross-platform.
    let _ = cargo::cargo_bin("atm");
}
