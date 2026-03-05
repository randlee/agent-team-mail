//! Integration tests for the `atm register` command.
//!
//! Each test uses ATM_HOME pointing to a temporary directory so tests are
//! fully isolated from any real team configuration on disk.

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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Set up the environment for a command: isolate ATM_HOME, strip ATM_IDENTITY,
/// and point the CWD to a neutral subdirectory so .atm.toml in the repo root
/// does not leak identity.
fn configure_cmd(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).ok();
    cmd.env("ATM_HOME", temp_dir.path())
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .env_remove("ATM_CONFIG")
        .env_remove("CLAUDE_SESSION_ID")
        .current_dir(&workdir);
}

/// Write a minimal `.atm.toml` into `workdir` so identity resolves to the
/// given string without hitting ATM_IDENTITY or the real `.atm.toml` on disk.
fn write_atm_toml(temp_dir: &TempDir, identity: &str) {
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).ok();
    fs::write(
        workdir.join(".atm.toml"),
        format!("[core]\nidentity = \"{identity}\"\ndefault_team = \"test-team\"\n"),
    )
    .unwrap();
}

/// Create a test team with the specified members.
///
/// Returns the path to the team directory.
fn create_test_team(temp_dir: &TempDir, team_name: &str, members: &[(&str, bool)]) {
    let team_dir = temp_dir
        .path()
        .join(".claude")
        .join("teams")
        .join(team_name);
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    let members_json: Vec<serde_json::Value> = members
        .iter()
        .map(|(name, active)| {
            serde_json::json!({
                "agentId": format!("{name}@{team_name}"),
                "name": name,
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1_770_000_000_000u64,
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": active,
            })
        })
        .collect();

    let config = serde_json::json!({
        "name": team_name,
        "description": format!("{team_name} test team"),
        "createdAt": 1_770_000_000_000u64,
        "leadAgentId": format!("team-lead@{team_name}"),
        "leadSessionId": "",
        "members": members_json,
    });

    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    // Create empty inbox files so send operations don't fail.
    for (name, _) in members {
        fs::write(inboxes_dir.join(format!("{name}.json")), "[]").unwrap();
    }
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
        else:
            resp = {
                "version": 1,
                "request_id": request_id,
                "status": "ok",
                "payload": {},
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
fn wait_for_fake_daemon_socket(home: &Path) {
    let socket = home.join(".claude/daemon/atm-daemon.sock");
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if socket.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "fake daemon socket not created in time: {}",
        socket.display()
    );
}

#[cfg(unix)]
fn start_fake_unknown_register_hint_daemon(home: &Path) -> Child {
    let script = write_fake_unknown_register_hint_daemon_script(home);
    let child = Command::new(&script).env("ATM_HOME", home).spawn().unwrap();
    wait_for_fake_daemon_socket(home);
    child
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_register_team_lead_with_session_id_env() {
    let temp_dir = TempDir::new().unwrap();
    create_test_team(
        &temp_dir,
        "my-team",
        &[("team-lead", true), ("alice", false)],
    );
    write_atm_toml(&temp_dir, "team-lead");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    cmd.env("CLAUDE_SESSION_ID", "test-session-lead-001")
        .env("ATM_TEAM", "my-team")
        // Use workdir so .atm.toml there is found.
        .current_dir(temp_dir.path().join("workdir"))
        .args(["register", "my-team"]);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Registered as team-lead"))
        .stdout(predicate::str::contains("my-team"))
        .stdout(predicate::str::contains("test-session-lead-001"))
        .stderr(predicate::str::contains("WARNING: hook file not found"));

    // Verify leadSessionId was updated in config.json.
    let config_path = temp_dir.path().join(".claude/teams/my-team/config.json");
    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(config["leadSessionId"], "test-session-lead-001");
}

#[test]
fn test_register_teammate_with_session_id_env() {
    let temp_dir = TempDir::new().unwrap();
    create_test_team(
        &temp_dir,
        "my-team",
        &[("team-lead", true), ("alice", false)],
    );

    // Provide an existing leadSessionId so no warning is printed.
    let config_path = temp_dir.path().join(".claude/teams/my-team/config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    config["leadSessionId"] = serde_json::json!("existing-lead-session");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    cmd.env("CLAUDE_SESSION_ID", "test-session-alice-001")
        .env("ATM_TEAM", "my-team")
        .args(["register", "my-team", "alice"]);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Registered as 'alice'"))
        .stdout(predicate::str::contains("my-team"))
        .stdout(predicate::str::contains("test-session-alice-001"))
        .stderr(predicate::str::contains("WARNING: hook file not found"));

    // Verify sessionId was written on the alice member.
    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    let alice = config["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "alice")
        .expect("alice member not found");
    assert_eq!(alice["sessionId"], "test-session-alice-001");
}

#[test]
fn test_register_unknown_name_fails() {
    let temp_dir = TempDir::new().unwrap();
    create_test_team(
        &temp_dir,
        "my-team",
        &[("team-lead", true), ("alice", false)],
    );

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    cmd.env("CLAUDE_SESSION_ID", "test-session-unknown")
        .env("ATM_TEAM", "my-team")
        .args(["register", "my-team", "unknown-agent"]);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("WARNING: hook file not found"))
        .stderr(predicate::str::contains("not found in team"));
}

#[test]
fn test_register_warns_when_lead_not_registered() {
    let temp_dir = TempDir::new().unwrap();
    // Team with no leadSessionId set (empty string as created by create_test_team).
    create_test_team(&temp_dir, "my-team", &[("team-lead", true), ("bob", false)]);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    cmd.env("CLAUDE_SESSION_ID", "test-session-bob-001")
        .env("ATM_TEAM", "my-team")
        .args(["register", "my-team", "bob"]);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("WARNING"))
        .stderr(predicate::str::contains("WARNING: hook file not found"));
}

#[test]
fn test_register_team_lead_wrong_identity_fails() {
    let temp_dir = TempDir::new().unwrap();
    create_test_team(
        &temp_dir,
        "my-team",
        &[("team-lead", true), ("charlie", false)],
    );
    // Set identity to "charlie" — not team-lead.
    write_atm_toml(&temp_dir, "charlie");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    // Override current_dir to workdir so the .atm.toml written there is picked up.
    cmd.env("CLAUDE_SESSION_ID", "test-session-charlie")
        .env("ATM_TEAM", "my-team")
        .current_dir(temp_dir.path().join("workdir"))
        .args(["register", "my-team"]);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("WARNING: hook file not found"))
        .stderr(predicate::str::contains("Only team-lead may call"));
}

#[test]
fn test_register_nonexistent_team_fails() {
    let temp_dir = TempDir::new().unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    cmd.env("CLAUDE_SESSION_ID", "test-session-x")
        .args(["register", "nonexistent-team"]);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn test_register_requires_session_id() {
    let temp_dir = TempDir::new().unwrap();
    create_test_team(
        &temp_dir,
        "my-team",
        &[("team-lead", true), ("dave", false)],
    );
    write_atm_toml(&temp_dir, "team-lead");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    // No hook file and no CLAUDE_SESSION_ID → should fail with helpful message.
    cmd.env("ATM_TEAM", "my-team")
        .current_dir(temp_dir.path().join("workdir"))
        .args(["register", "my-team"]);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("Cannot determine session_id"));
}

#[test]
fn test_register_invalid_hook_file_does_not_fallback_to_env() {
    let temp_dir = TempDir::new().unwrap();
    create_test_team(
        &temp_dir,
        "my-team",
        &[("team-lead", true), ("erin", false)],
    );
    write_atm_toml(&temp_dir, "team-lead");

    // Build a stale hook file at atm-hook-<ppid>.json where ppid will be this test process.
    let ppid = std::process::id();
    let hook_path = temp_dir.path().join(format!("atm-hook-{ppid}.json"));
    let stale = serde_json::json!({
        "pid": ppid,
        "session_id": "stale-session",
        "agent_name": "team-lead",
        "created_at": 0.0
    });
    fs::write(&hook_path, serde_json::to_string(&stale).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    cmd.env("TMPDIR", temp_dir.path())
        .env("TMP", temp_dir.path()) // Windows uses TMP/TEMP, not TMPDIR
        .env("TEMP", temp_dir.path())
        .env("CLAUDE_SESSION_ID", "env-session-should-not-be-used")
        .env("ATM_TEAM", "my-team")
        .current_dir(temp_dir.path().join("workdir"))
        .args(["register", "my-team"]);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("hook file validation failed"));
}

#[test]
fn test_register_conflicting_lead_session_blocks_without_force_when_daemon_unreachable() {
    let temp_dir = TempDir::new().unwrap();
    create_test_team(
        &temp_dir,
        "my-team",
        &[("team-lead", true), ("alice", false)],
    );
    write_atm_toml(&temp_dir, "team-lead");

    // Pre-populate leadSessionId to simulate an existing lead claim.
    let config_path = temp_dir.path().join(".claude/teams/my-team/config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    config["leadSessionId"] = serde_json::json!("existing-live-session-xyz");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    cmd.env("CLAUDE_SESSION_ID", "new-session-id-123")
        .env("ATM_TEAM", "my-team")
        .current_dir(temp_dir.path().join("workdir"))
        .args(["register", "my-team"]);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains(
            "cannot confirm liveness of existing team-lead session",
        ))
        .stderr(predicate::str::contains("--force"));
}

#[test]
fn test_register_conflicting_lead_session_allows_force_when_daemon_unreachable() {
    let temp_dir = TempDir::new().unwrap();
    create_test_team(
        &temp_dir,
        "my-team",
        &[("team-lead", true), ("alice", false)],
    );
    write_atm_toml(&temp_dir, "team-lead");

    let config_path = temp_dir.path().join(".claude/teams/my-team/config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    config["leadSessionId"] = serde_json::json!("existing-live-session-xyz");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    cmd.env("CLAUDE_SESSION_ID", "forced-new-session-id")
        .env("ATM_TEAM", "my-team")
        .current_dir(temp_dir.path().join("workdir"))
        .args(["register", "my-team", "--force"]);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Registered as team-lead"))
        .stdout(predicate::str::contains("forced-new-session-id"));
}

#[cfg(unix)]
#[test]
fn test_register_warns_and_continues_when_daemon_is_unsupported() {
    let temp_dir = TempDir::new().unwrap();
    create_test_team(
        &temp_dir,
        "my-team",
        &[("team-lead", true), ("alice", false)],
    );

    let ppid = std::process::id();
    let hook_path = temp_dir.path().join(format!("atm-hook-{ppid}.json"));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let hook = serde_json::json!({
        "pid": ppid,
        "session_id": "test-session-alice-unknown-daemon",
        "agent_name": "alice",
        "created_at": now,
    });
    fs::write(&hook_path, serde_json::to_string(&hook).unwrap()).unwrap();

    let mut daemon = start_fake_unknown_register_hint_daemon(temp_dir.path());

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    cmd.env("TMPDIR", temp_dir.path())
        .env("TMP", temp_dir.path())
        .env("TEMP", temp_dir.path())
        .env("ATM_TEAM", "my-team")
        .args(["register", "my-team", "alice"]);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Registered as 'alice'"))
        .stderr(predicate::str::contains(
            "Connected daemon does not support 'register-hint'",
        ))
        .stderr(predicate::str::contains(
            "continuing without daemon session sync",
        ));

    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
fn test_register_teammate_rejects_cross_identity_write() {
    let temp_dir = TempDir::new().unwrap();
    create_test_team(
        &temp_dir,
        "my-team",
        &[("team-lead", true), ("alice", false), ("bob", false)],
    );
    write_atm_toml(&temp_dir, "bob");

    let config_path = temp_dir.path().join(".claude/teams/my-team/config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    config["members"][1]["sessionId"] = serde_json::json!("alice-session-existing");
    config["leadSessionId"] = serde_json::json!("lead-session-existing");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    cmd.env("CLAUDE_SESSION_ID", "new-cross-identity-session")
        .env("ATM_TEAM", "my-team")
        .current_dir(temp_dir.path().join("workdir"))
        .args(["register", "my-team", "alice"]);

    cmd.assert().success().stderr(predicate::str::contains(
        "refusing to update sessionId for 'alice': caller identity is 'bob'",
    ));

    let config_after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    let alice_after = config_after["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "alice")
        .unwrap();
    assert_eq!(alice_after["sessionId"], "alice-session-existing");
}
