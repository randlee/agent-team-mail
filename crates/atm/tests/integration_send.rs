//! Integration tests for the send command

#[cfg(unix)]
use agent_team_mail_core::consts::WAIT_FOR_DAEMON_SOCKET_SECS;
use assert_cmd::cargo;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Child, Command};
#[cfg(unix)]
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

/// Helper to set home directory for cross-platform test compatibility.
/// Uses `ATM_HOME` which is checked first by `get_home_dir()`, avoiding
/// platform-specific differences in how `dirs::home_dir()` resolves
/// (HOME on Unix, Windows API on Windows).
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    // Use a subdirectory as CWD to avoid:
    // 1. .atm.toml config leak from the repo root
    // 2. auto-identity CWD matching against team member CWD (temp_dir root)
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).ok();
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env_remove("ATM_TEAM")
        .env_remove("ATM_CONFIG")
        .env_remove("CLAUDE_SESSION_ID")
        .env("ATM_IDENTITY", "team-lead") // default identity; individual tests can override
        .current_dir(&workdir);
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
srv.listen(32)
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
        if command in ("session-query-team", "session-for-team"):
            response_payload = {
                "session_id": "fake-session",
                "process_id": os.getpid(),
                "alive": False,
                "runtime": "codex",
                "agent": payload.get("name", "unknown"),
                "team": payload.get("team", "unknown"),
            }
        elif command == "list-agents":
            response_payload = []
        else:
            response_payload = {}

        resp = {
            "version": 1,
            "request_id": request_id,
            "status": "ok",
            "payload": response_payload,
        }
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
    let socket = home.join(".atm/daemon/atm-daemon.sock");
    let deadline = Instant::now() + Duration::from_secs(WAIT_FOR_DAEMON_SOCKET_SECS);
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
fn spawn_python_script(script: &Path, home: &Path) -> Child {
    let python = std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_string());
    Command::new(python)
        .arg(script)
        .env("ATM_HOME", home)
        .spawn()
        .unwrap()
}

#[cfg(unix)]
fn start_fake_dead_session_daemon(home: &Path) -> Child {
    let script = write_fake_daemon_script(home);
    let child = spawn_python_script(&script, home);
    wait_for_daemon_socket(home);
    child
}

#[cfg(unix)]
fn write_fake_unknown_register_hint_daemon_script(home: &Path) -> PathBuf {
    let script = home.join("fake-register-unknown-daemon.py");
    let body = r#"#!/usr/bin/env python3
import json
import os
import signal
import socket
from pathlib import Path

home = Path(os.environ["ATM_HOME"])
daemon_dir = home / ".atm" / "daemon"
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
        payload = req.get("payload", {}) or {}
        if command == "register-hint":
            resp = {
                "version": 1,
                "request_id": request_id,
                "status": "error",
                "error": {
                    "code": "UNKNOWN_COMMAND",
                    "message": "Unknown command: 'register-hint'",
                },
            }
        elif command in ("session-query-team", "session-for-team"):
            resp = {
                "version": 1,
                "request_id": request_id,
                "status": "ok",
                "payload": {
                    "session_id": "recipient-live-session",
                    "process_id": os.getpid(),
                    "alive": True,
                    "runtime": "claude",
                    "agent": payload.get("name", "unknown"),
                    "team": payload.get("team", "unknown"),
                },
            }
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
fn start_fake_unknown_register_hint_daemon(home: &Path) -> Child {
    let script = write_fake_unknown_register_hint_daemon_script(home);
    let child = spawn_python_script(&script, home);
    wait_for_daemon_socket(home);
    child
}

#[cfg(unix)]
fn write_fake_request_logging_daemon_script(home: &Path) -> PathBuf {
    let script = home.join("fake-request-logging-daemon.py");
    let body = r#"#!/usr/bin/env python3
import json
import os
import signal
import socket
from pathlib import Path

home = Path(os.environ["ATM_HOME"])
daemon_dir = home / ".atm" / "daemon"
daemon_dir.mkdir(parents=True, exist_ok=True)

sock_path = daemon_dir / "atm-daemon.sock"
pid_path = daemon_dir / "atm-daemon.pid"
log_path = daemon_dir / "requests.jsonl"
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
        with log_path.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(req) + "\n")

        request_id = req.get("request_id", "req")
        command = req.get("command", "")
        payload = req.get("payload", {}) or {}
        if command in ("session-query-team", "session-for-team"):
            response_payload = {
                "session_id": "live-session",
                "process_id": os.getpid(),
                "alive": True,
                "runtime": "codex",
                "agent": payload.get("name", "unknown"),
                "team": payload.get("team", "unknown"),
            }
        elif command == "agent-state":
            response_payload = {
                "state": "idle",
                "last_transition": "2026-02-11T10:00:00Z",
            }
        else:
            response_payload = {}

        resp = {
            "version": 1,
            "request_id": request_id,
            "status": "ok",
            "payload": response_payload,
        }
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
fn start_fake_request_logging_daemon(home: &Path) -> (Child, PathBuf) {
    let script = write_fake_request_logging_daemon_script(home);
    let child = spawn_python_script(&script, home);
    wait_for_daemon_socket(home);
    (child, home.join(".atm/daemon/requests.jsonl"))
}

#[cfg(unix)]
fn start_fake_claude_process(home: &Path) -> Child {
    let sleep_bin = Path::new("/bin/sleep");
    assert!(
        sleep_bin.exists(),
        "expected /bin/sleep for fake claude process"
    );

    let claude_link = home.join("claude");
    if !claude_link.exists() {
        symlink(sleep_bin, &claude_link).unwrap();
    }
    Command::new(&claude_link).arg("60").spawn().unwrap()
}

/// Create a test team structure
fn setup_test_team(temp_dir: &TempDir, team_name: &str) -> PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");

    fs::create_dir_all(&inboxes_dir).unwrap();

    // Create team config.json
    let config = serde_json::json!({
        "name": team_name,
        "description": "Test team",
        "createdAt": 1739284800000i64,
        "leadAgentId": format!("team-lead@{}", team_name),
        "leadSessionId": "test-session-id",
        "members": [
            {
                "agentId": format!("team-lead@{}", team_name),
                "name": "team-lead",
                "agentType": "general-purpose",
                "model": "claude-haiku-4-5-20251001",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": []
            },
            {
                "agentId": format!("test-agent@{}", team_name),
                "name": "test-agent",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "prompt": "Test agent",
                "color": "blue",
                "planModeRequired": false,
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%1",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "backendType": "tmux",
                "isActive": true
            }
        ]
    });

    let config_path = team_dir.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    team_dir
}

#[test]
fn test_send_basic_message() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("Hello, test agent!")
        .assert()
        .success();

    // Verify inbox file was created
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/test-agent.json");

    assert!(inbox_path.exists());

    // Verify message content
    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["from"], "team-lead");
    assert_eq!(messages[0]["text"], "Hello, test agent!");
    assert_eq!(messages[0]["read"], false);
    assert!(messages[0]["message_id"].is_string());
}

#[test]
fn test_send_cross_team_addressing() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir1 = setup_test_team(&temp_dir, "team-a");
    let _team_dir2 = setup_test_team(&temp_dir, "team-b");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "team-a")
        .arg("send")
        .arg("test-agent@team-b")
        .arg("Cross-team message")
        .assert()
        .success();

    // Verify inbox file was created in team-b
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/team-b/inboxes/test-agent.json");

    assert!(inbox_path.exists());

    // Verify message content
    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["text"], "Cross-team message");
}

#[test]
fn test_send_alias_with_team_suffix_resolves_end_to_end() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Configure alias in global ATM config under ATM_HOME so send command
    // resolves arch-atm -> team-lead while preserving explicit @team suffix.
    let global_cfg_dir = temp_dir.path().join(".config/atm");
    fs::create_dir_all(&global_cfg_dir).unwrap();
    fs::write(
        global_cfg_dir.join("config.toml"),
        "[core]\ndefault_team = \"test-team\"\nidentity = \"human\"\n\n[aliases]\narch-atm = \"team-lead\"\n",
    )
    .unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "test-agent") // override default; sender != alias recipient (team-lead)
        .arg("send")
        .arg("arch-atm@test-team")
        .arg("Alias routed message")
        .assert()
        .success();

    // Full send path assertion: parse_address + alias resolution + team lookup + inbox write.
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/team-lead.json");
    assert!(inbox_path.exists());

    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["text"], "Alias routed message");
}

#[test]
fn test_send_role_with_team_suffix_resolves_end_to_end() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = temp_dir.path().join(".claude/teams").join("test-team");
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    // Team member is arch-atm (role target), not literal team-lead.
    let config = serde_json::json!({
        "name": "test-team",
        "description": "Test team",
        "createdAt": 1739284800000i64,
        "leadAgentId": "arch-atm@test-team",
        "leadSessionId": "test-session-id",
        "members": [
            {
                "agentId": "sender@test-team",
                "name": "sender",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": []
            },
            {
                "agentId": "arch-atm@test-team",
                "name": "arch-atm",
                "agentType": "general-purpose",
                "model": "claude-haiku-4-5-20251001",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": []
            }
        ]
    });
    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    // Configure role in global ATM config under ATM_HOME so send command
    // resolves team-lead -> arch-atm while preserving explicit @team suffix.
    let global_cfg_dir = temp_dir.path().join(".config/atm");
    fs::create_dir_all(&global_cfg_dir).unwrap();
    fs::write(
        global_cfg_dir.join("config.toml"),
        "[core]\ndefault_team = \"test-team\"\nidentity = \"sender\"\n\n[roles]\nteam-lead = \"arch-atm\"\n",
    )
    .unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("team-lead@test-team")
        .arg("Role routed message")
        .assert()
        .success();

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/arch-atm.json");
    assert!(inbox_path.exists());

    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["text"], "Role routed message");
}

#[test]
fn test_send_with_team_flag() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "override-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "default-team")
        .arg("send")
        .arg("--team")
        .arg("override-team")
        .arg("test-agent")
        .arg("Message with team flag")
        .assert()
        .success();

    // Verify inbox file was created in override-team
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/override-team/inboxes/test-agent.json");

    assert!(inbox_path.exists());
}

#[test]
fn test_send_with_summary() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("--summary")
        .arg("Custom summary")
        .arg("Long message content that would normally be truncated")
        .assert()
        .success();

    // Verify message has custom summary
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/test-agent.json");

    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages[0]["summary"], "Custom summary");
}

#[test]
fn test_send_json_output() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("--json")
        .arg("Test message")
        .assert()
        .success();

    // Verify output is valid JSON (assert_cmd captures stdout)
    // We can't easily verify the exact JSON output without more complex assertion
    // but the command succeeding with --json is a good smoke test
}

#[test]
fn test_send_dry_run() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("--dry-run")
        .arg("Dry run message")
        .assert()
        .success();

    // Verify inbox file was NOT created
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/test-agent.json");

    assert!(!inbox_path.exists());
}

#[test]
fn test_send_with_stdin() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("--stdin")
        .write_stdin("Message from stdin")
        .assert()
        .success();

    // Verify message content
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/test-agent.json");

    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages[0]["text"], "Message from stdin");
}

#[test]
fn test_send_with_file_reference() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Create .git directory so file policy recognizes a repo root
    fs::create_dir_all(temp_dir.path().join(".git")).unwrap();

    // Create a test file in the temp directory
    let test_file = temp_dir.path().join("test-file.txt");
    fs::write(&test_file, "File content").unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .current_dir(temp_dir.path())
        .arg("send")
        .arg("test-agent")
        .arg("--file")
        .arg(&test_file)
        .assert()
        .success();

    // Verify message includes file reference
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/test-agent.json");

    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    let text = messages[0]["text"].as_str().unwrap();
    assert!(text.contains("File reference:"));
    assert!(text.contains("test-file.txt"));
}

#[test]
fn test_send_agent_not_found() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("nonexistent-agent")
        .arg("Test message")
        .assert()
        .failure();
}

#[test]
fn test_send_team_not_found() {
    let temp_dir = TempDir::new().unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "nonexistent-team")
        .arg("send")
        .arg("test-agent")
        .arg("Test message")
        .assert()
        .failure();
}

#[test]
fn test_send_file_not_found() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("--file")
        .arg("/nonexistent/file.txt")
        .assert()
        .failure();
}

#[test]
fn test_send_multiple_messages_append() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Send first message
    let mut cmd1 = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd1, &temp_dir);
    cmd1.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("First message")
        .assert()
        .success();

    // Send second message
    let mut cmd2 = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd2, &temp_dir);
    cmd2.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("Second message")
        .assert()
        .success();

    // Verify both messages are in inbox
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/test-agent.json");

    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["text"], "First message");
    assert_eq!(messages[1]["text"], "Second message");
}

// ============================================================================
// Offline Recipient Detection Tests
// ============================================================================

/// Create a test team with mixed online/offline agents
fn setup_team_with_offline_agents(temp_dir: &TempDir, team_name: &str) -> PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");

    fs::create_dir_all(&inboxes_dir).unwrap();

    let config = serde_json::json!({
        "name": team_name,
        "description": "Test team with offline agents",
        "createdAt": 1739284800000i64,
        "leadAgentId": format!("team-lead@{}", team_name),
        "leadSessionId": "test-session-id",
        "members": [
            {
                "agentId": format!("team-lead@{}", team_name),
                "name": "team-lead",
                "agentType": "general-purpose",
                "model": "claude-haiku-4-5-20251001",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": true
            },
            {
                "agentId": format!("online-agent@{}", team_name),
                "name": "online-agent",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%1",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": true
            },
            {
                "agentId": format!("offline-agent@{}", team_name),
                "name": "offline-agent",
                "agentType": "general-purpose",
                "model": "claude-sonnet-4-5-20250929",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%2",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": false
            },
            {
                "agentId": format!("no-status-agent@{}", team_name),
                "name": "no-status-agent",
                "agentType": "general-purpose",
                "model": "claude-sonnet-4-5-20250929",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%3",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": []
            }
        ]
    });

    let config_path = team_dir.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    team_dir
}

#[test]
fn test_offline_recipient_detection_auto_tag() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_team_with_offline_agents(&temp_dir, "test-team");

    // Send to offline-agent (isActive: false)
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("offline-agent")
        .arg("Please review this")
        .assert()
        .success();

    // Without explicit action text, default behavior is no prepend.
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/offline-agent.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();

    assert_eq!(messages.len(), 1);
    let text = messages[0]["text"].as_str().unwrap();
    assert_eq!(text, "Please review this");
}

#[test]
#[cfg(unix)]
fn test_offline_recipient_custom_flag() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_team_with_offline_agents(&temp_dir, "test-team");
    let mut daemon = start_fake_dead_session_daemon(temp_dir.path());

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("offline-agent")
        .arg("--offline-action")
        .arg("DO THIS LATER")
        .arg("Review when ready")
        .assert()
        .success();

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/offline-agent.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();

    let text = messages[0]["text"].as_str().unwrap();
    assert_eq!(text, "[DO THIS LATER] Review when ready");
    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
#[cfg(unix)]
fn test_offline_recipient_config_override() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_team_with_offline_agents(&temp_dir, "test-team");
    let mut daemon = start_fake_dead_session_daemon(temp_dir.path());

    // Create .atm.toml config with messaging.offline_action
    let config_dir = temp_dir.path().join("workdir");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join(".atm.toml"),
        "[core]\ndefault_team = \"test-team\"\nidentity = \"human\"\n\n[messaging]\noffline_action = \"QUEUED\"\n",
    )
    .unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .current_dir(&config_dir)
        .arg("send")
        .arg("offline-agent")
        .arg("Queued message")
        .assert()
        .success();

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/offline-agent.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();

    let text = messages[0]["text"].as_str().unwrap();
    assert_eq!(text, "[QUEUED] Queued message");
    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
fn test_offline_recipient_empty_string_opt_out() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_team_with_offline_agents(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("offline-agent")
        .arg("--offline-action")
        .arg("")
        .arg("No prefix please")
        .assert()
        .success();

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/offline-agent.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();

    let text = messages[0]["text"].as_str().unwrap();
    assert_eq!(
        text, "No prefix please",
        "Empty opt-out should skip prepend"
    );
}

#[test]
fn test_online_recipient_no_tag() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_team_with_offline_agents(&temp_dir, "test-team");

    // Send to online-agent (isActive: true)
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("online-agent")
        .arg("Hello online agent")
        .assert()
        .success();

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/online-agent.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();

    let text = messages[0]["text"].as_str().unwrap();
    assert_eq!(
        text, "Hello online agent",
        "Online agent should NOT get action prefix"
    );
}

#[test]
fn test_unknown_session_state_never_prefixes_even_with_offline_action_override() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_team_with_offline_agents(&temp_dir, "test-team");

    // Daemon absent => session state is unknown (Ok(None)); no prefix should be added
    // even when caller provides an explicit offline-action override.
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("no-status-agent")
        .arg("--offline-action")
        .arg("DO THIS LATER")
        .arg("Check status")
        .assert()
        .success();

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/no-status-agent.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();

    let text = messages[0]["text"].as_str().unwrap();
    assert!(
        !text.starts_with("[DO THIS LATER]"),
        "Unknown daemon session state must not trigger offline prefix, got: {text}"
    );
    assert_eq!(text, "Check status");
}

#[cfg(unix)]
#[test]
fn test_send_warns_and_continues_when_register_hint_is_unsupported() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut fake_claude = start_fake_claude_process(temp_dir.path());
    let mut daemon = start_fake_unknown_register_hint_daemon(temp_dir.path());

    let ppid = std::process::id();
    let hook_path = temp_dir.path().join(format!("atm-hook-{ppid}.json"));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let hook = serde_json::json!({
        "pid": fake_claude.id(),
        "session_id": "test-session-send-unknown-daemon",
        "agent_name": "team-lead",
        "created_at": now,
    });
    fs::write(&hook_path, serde_json::to_string(&hook).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env("TMPDIR", temp_dir.path())
        .env("TMP", temp_dir.path())
        .env("TEMP", temp_dir.path())
        .env("ATM_TEAM", "test-team")
        .args([
            "send",
            "test-agent",
            "message survives unknown register-hint",
        ])
        .assert()
        .success();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("Connected daemon does not support 'register-hint'"));
    assert!(stderr.contains("continuing without daemon session sync"));

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/test-agent.json");
    let messages: Vec<serde_json::Value> =
        serde_json::from_str(&fs::read_to_string(&inbox_path).unwrap()).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0]["text"],
        "message survives unknown register-hint"
    );

    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = fake_claude.kill();
    let _ = fake_claude.wait();
}

#[cfg(unix)]
#[test]
fn test_send_emits_post_send_idle_without_subscribe_side_effect() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");
    let (mut daemon, request_log) = start_fake_request_logging_daemon(temp_dir.path());

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_RUNTIME", "codex")
        .env("ATM_SESSION_ID", "codex-session-123")
        .arg("send")
        .arg("test-agent")
        .arg("codex should return to idle")
        .assert()
        .success();

    let requests: Vec<serde_json::Value> = fs::read_to_string(&request_log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    let commands: Vec<&str> = requests
        .iter()
        .filter_map(|request| request["command"].as_str())
        .collect();
    assert!(
        !commands.contains(&"subscribe"),
        "send must not auto-subscribe sender to idle events; commands={commands:?}"
    );
    let hook_event = requests
        .iter()
        .find(|request| request["command"] == "hook-event")
        .expect("post-send idle hook-event should be emitted");
    assert_eq!(hook_event["payload"]["event"], "teammate_idle");
    assert_eq!(hook_event["payload"]["agent"], "team-lead");
    assert_eq!(hook_event["payload"]["team"], "test-team");
    assert_eq!(hook_event["payload"]["session_id"], "codex-session-123");
    assert_eq!(hook_event["payload"]["source"]["kind"], "agent_hook");

    let _ = daemon.kill();
    let _ = daemon.wait();
}
