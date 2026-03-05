use assert_cmd::cargo;
use predicates::prelude::*;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Child, Command};
#[cfg(unix)]
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env_remove("ATM_CONFIG")
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .current_dir(&workdir);
}

#[cfg(unix)]
fn write_fake_live_session_daemon_script(home: &Path) -> PathBuf {
    let script = home.join("fake-live-session-daemon.py");
    let body = r#"#!/usr/bin/env python3
import json
import os
import signal
import socket
from pathlib import Path

home = Path(os.environ["ATM_HOME"])
daemon_dir = home / ".claude" / "daemon"
daemon_dir.mkdir(parents=True, exist_ok=True)

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
        if command in ("session-query-team", "session-for-team"):
            payload = {
                "session_id": "live-sess",
                "process_id": 9999,
                "alive": True,
                "runtime": "codex",
                "agent": req.get("payload", {}).get("name", "unknown"),
                "team": req.get("payload", {}).get("team", "unknown"),
            }
            resp = {"version": 1, "request_id": request_id, "status": "ok", "payload": payload}
        elif command == "list-agents":
            resp = {"version": 1, "request_id": request_id, "status": "ok", "payload": []}
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
fn start_fake_live_session_daemon(home: &Path) -> Child {
    let script = write_fake_live_session_daemon_script(home);
    let child = Command::new(&script).env("ATM_HOME", home).spawn().unwrap();
    wait_for_daemon_socket(home);
    child
}

#[test]
fn test_spawn_folder_rejects_nonexistent_directory() {
    let temp_dir = TempDir::new().unwrap();
    let missing = temp_dir.path().join("missing");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.args([
        "teams",
        "spawn",
        "agent-a",
        "--team",
        "atm-dev",
        "--runtime",
        "codex",
        "--folder",
        missing.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("does not exist"));
}

#[test]
fn test_spawn_folder_rejects_existing_file_path() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("plain-file.txt");
    fs::write(&file_path, "x").unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.args([
        "teams",
        "spawn",
        "agent-file",
        "--team",
        "atm-dev",
        "--runtime",
        "codex",
        "--folder",
        file_path.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("is not a directory"));
}

#[test]
fn test_spawn_folder_and_cwd_mismatch_rejected() {
    let temp_dir = TempDir::new().unwrap();
    let folder_a = temp_dir.path().join("a");
    let folder_b = temp_dir.path().join("b");
    fs::create_dir_all(&folder_a).unwrap();
    fs::create_dir_all(&folder_b).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.args([
        "teams",
        "spawn",
        "agent-b",
        "--team",
        "atm-dev",
        "--runtime",
        "gemini",
        "--folder",
        folder_a.to_str().unwrap(),
        "--cwd",
        folder_b.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("resolve to different directories"));
}

#[test]
fn test_spawn_cwd_only_reaches_daemon_with_json_folder_field() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("cwd-only");
    fs::create_dir_all(&folder).unwrap();
    let canonical = fs::canonicalize(&folder).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args([
            "teams",
            "spawn",
            "agent-c",
            "--team",
            "atm-dev",
            "--runtime",
            "codex",
            "--cwd",
            folder.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not running")
    );
    assert_eq!(parsed["folder"], canonical.to_string_lossy().to_string());
}

#[test]
fn test_spawn_dual_flag_match_reaches_daemon_and_keeps_folder_json() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("dual");
    fs::create_dir_all(&folder).unwrap();
    let canonical = fs::canonicalize(&folder).unwrap();
    let alt = temp_dir.path().join("dual/.");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args([
            "teams",
            "spawn",
            "agent-d",
            "--team",
            "atm-dev",
            "--runtime",
            "gemini",
            "--folder",
            folder.to_str().unwrap(),
            "--cwd",
            alt.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not running")
    );
    assert_eq!(parsed["folder"], canonical.to_string_lossy().to_string());
}

#[test]
fn test_spawn_relative_folder_normalizes_to_absolute_in_json_output() {
    let temp_dir = TempDir::new().unwrap();
    let subdir = temp_dir.path().join("workdir").join("subdir");
    fs::create_dir_all(&subdir).unwrap();
    let canonical = fs::canonicalize(&subdir).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args([
            "teams",
            "spawn",
            "agent-rel",
            "--team",
            "atm-dev",
            "--runtime",
            "codex",
            "--folder",
            "./subdir",
            "--json",
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not running")
    );
    assert_eq!(parsed["folder"], canonical.to_string_lossy().to_string());
}

#[test]
fn test_spawn_claude_echoes_full_launch_command_on_failure() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("claude-folder");
    fs::create_dir_all(&folder).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args([
            "teams",
            "spawn",
            "my-agent",
            "--team",
            "atm-dev",
            "--runtime",
            "claude",
            "--folder",
            folder.to_str().unwrap(),
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stdout.contains("# Spawn command:"));
    assert!(stdout.contains("env ATM_TEAM='atm-dev' ATM_IDENTITY='my-agent'"));
    assert!(stdout.contains("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 claude"));
    assert!(stdout.contains("--agent-id my-agent@atm-dev"));
    assert!(stdout.contains("--agent-name my-agent"));
    assert!(stdout.contains("--team-name atm-dev"));
    assert!(stdout.contains("--dangerously-skip-permissions"));
    assert!(
        stderr.contains("Daemon is not running"),
        "expected daemon unavailable failure, got stderr: {stderr}"
    );
}

#[test]
fn test_spawn_env_team_mismatch_requires_override_team() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(
        workdir.join(".atm.toml"),
        "[core]\ndefault_team = \"toml-team\"\nidentity = \"team-lead\"\n",
    )
    .unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env("ATM_TEAM", "env-team")
        .args([
            "teams",
            "spawn",
            "agent-mismatch",
            "--runtime",
            "codex",
            "--folder",
            folder.to_str().unwrap(),
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stdout.contains("# Spawn command:"));
    assert!(stderr.contains("Warning: team mismatch detected"));
    assert!(stderr.contains("ATM_TEAM ('env-team')"));
    assert!(stderr.contains(".atm.toml default_team ('toml-team')"));
    assert!(stderr.contains("--override-team"));
}

#[test]
fn test_spawn_env_team_mismatch_override_team_uses_env_team() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    let atm_toml_path = workdir.join(".atm.toml");
    let toml_content = "[core]\ndefault_team = \"toml-team\"\nidentity = \"team-lead\"\n";
    fs::write(&atm_toml_path, toml_content).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env("ATM_TEAM", "env-team")
        .args([
            "teams",
            "spawn",
            "agent-override",
            "--runtime",
            "codex",
            "--override-team",
            "--folder",
            folder.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["team"].as_str(), Some("env-team"));
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not running")
    );
    assert_eq!(fs::read_to_string(&atm_toml_path).unwrap(), toml_content);
}

#[cfg(unix)]
#[test]
fn test_spawn_rejects_live_session_ownership_conflict() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();
    let mut daemon = start_fake_live_session_daemon(temp_dir.path());

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args([
            "teams",
            "spawn",
            "agent-live",
            "--team",
            "atm-dev",
            "--runtime",
            "codex",
            "--folder",
            folder.to_str().unwrap(),
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stdout.contains("# Spawn command:"));
    assert!(stderr.contains("live session is already registered"));
    assert!(stderr.contains("session_id='live-sess'"));
    assert!(stderr.contains("Stop the existing process"));

    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
fn test_spawn_help_includes_env_folder_and_runtime_examples() {
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.args(["teams", "spawn", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ATM_TEAM"))
        .stdout(predicate::str::contains("ATM_IDENTITY"))
        .stdout(predicate::str::contains("--folder"))
        .stdout(predicate::str::contains("--runtime codex"))
        .stdout(predicate::str::contains("--runtime gemini"))
        .stdout(predicate::str::contains("--runtime claude"));
}
