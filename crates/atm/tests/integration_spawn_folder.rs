#[cfg(unix)]
use agent_team_mail_core::consts::WAIT_FOR_DAEMON_SOCKET_SECS;
use assert_cmd::cargo;
use predicates::prelude::*;
use serial_test::serial;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
#[cfg(unix)]
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    let runtime_home = temp_dir.path().join("runtime-home");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&runtime_home).unwrap();
    cmd.env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", temp_dir.path())
        .envs([("ATM_HOME", temp_dir.path())])
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env_remove("ATM_CONFIG")
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .env_remove("CLAUDE_SESSION_ID")
        .current_dir(&workdir);
}

fn write_team_config(home: &TempDir, team: &str) {
    let team_dir = home.path().join(".claude/teams").join(team);
    fs::create_dir_all(&team_dir).unwrap();
    let config = serde_json::json!({
        "name": team,
        "createdAt": 1739284800000u64,
        "leadAgentId": format!("team-lead@{team}"),
        "leadSessionId": "lead-sess",
        "members": [
            {
                "agentId": format!("team-lead@{team}"),
                "name": "team-lead",
                "agentType": "general-purpose",
                "model": "unknown",
                "joinedAt": 1739284800000u64,
                "tmuxPaneId": "",
                "cwd": ".",
                "subscriptions": [],
                "sessionId": "lead-sess"
            },
            {
                "agentId": format!("arch-atm@{team}"),
                "name": "arch-atm",
                "agentType": "general-purpose",
                "model": "unknown",
                "joinedAt": 1739284800000u64,
                "tmuxPaneId": "",
                "cwd": ".",
                "subscriptions": [],
                "sessionId": "co-sess"
            },
            {
                "agentId": format!("dev-1@{team}"),
                "name": "dev-1",
                "agentType": "general-purpose",
                "model": "unknown",
                "joinedAt": 1739284800000u64,
                "tmuxPaneId": "",
                "cwd": ".",
                "subscriptions": [],
                "sessionId": "dev-sess"
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
fn write_fake_spawn_session_daemon_script(home: &Path) -> PathBuf {
    let canonical_daemon_dir = home.join(".atm/daemon");
    fs::create_dir_all(&canonical_daemon_dir).unwrap();
    let legacy_daemon_root = home.join(".claude");
    fs::create_dir_all(&legacy_daemon_root).unwrap();
    let legacy_daemon_dir = legacy_daemon_root.join("daemon");
    if !legacy_daemon_dir.exists() {
        symlink(&canonical_daemon_dir, &legacy_daemon_dir).unwrap();
    }

    let script = home.join("fake-spawn-session-daemon.py");
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
srv.listen(64)
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
        if command == "list-agents":
            payload = [
                {
                    "agent": "member-a",
                    "state": "active",
                    "activity": "busy",
                    "session_id": "abc11111-1111-4111-8111-111111111111",
                    "process_id": 1001,
                    "last_alive_at": "2026-03-10T12:00:00Z",
                    "reason": "ok",
                    "source": "daemon",
                    "in_config": True,
                },
                {
                    "agent": "member-b",
                    "state": "active",
                    "activity": "busy",
                    "session_id": "abc22222-2222-4222-8222-222222222222",
                    "process_id": 1002,
                    "last_alive_at": "2026-03-10T12:00:01Z",
                    "reason": "ok",
                    "source": "daemon",
                    "in_config": True,
                },
            ]
        else:
            payload = {}
        response = {
            "version": 1,
            "request_id": request_id,
            "status": "ok",
            "payload": payload,
        }
        try:
            conn.sendall((json.dumps(response) + "\n").encode())
        except BrokenPipeError:
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
        .env("ATM_IDENTITY", "team-lead")
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
    let error = parsed["error"].as_str().unwrap();
    assert!(
        error.contains("Daemon is not running"),
        "expected daemon-unavailable error, got: {error}"
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
        .env("ATM_IDENTITY", "team-lead")
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
        .env("ATM_IDENTITY", "team-lead")
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
        .env("ATM_IDENTITY", "team-lead")
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
    assert!(stdout.contains("Launch command:"));
    assert!(stdout.contains("cd "));
    assert!(stdout.contains("&& env CLAUDECODE=1"));
    assert!(stdout.contains("env CLAUDECODE=1 ATM_TEAM='atm-dev' ATM_IDENTITY='my-agent'"));
    assert!(stdout.contains("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 claude"));
    assert!(stdout.contains("--agent-id 'my-agent@atm-dev'"));
    assert!(stdout.contains("--agent-name 'my-agent'"));
    assert!(stdout.contains("--team-name 'atm-dev'"));
    assert!(stdout.contains("--dangerously-skip-permissions"));
    assert!(
        stderr.contains("Daemon is not running"),
        "expected daemon unavailable failure, got stderr: {stderr}"
    );
}

#[test]
fn test_spawn_system_prompt_template_renders_before_daemon_probe() {
    let temp_dir = TempDir::new().unwrap();
    let workdir = temp_dir.path().join("workdir");
    let folder = workdir.join("templated-spawn");
    fs::create_dir_all(&folder).unwrap();

    let template = folder.join("system.md.j2");
    fs::write(
        &template,
        r#"---
required_variables:
  - team
  - agent
  - runtime
  - cwd
  - custom
defaults:
  model: "unset"
---
team={{ team }}
agent={{ agent }}
runtime={{ runtime }}
cwd={{ cwd }}
model={{ model }}
custom={{ custom }}
"#,
    )
    .unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env("ATM_IDENTITY", "team-lead")
        .args([
            "teams",
            "spawn",
            "templated-agent",
            "--team",
            "atm-dev",
            "--runtime",
            "gemini",
            "--folder",
            folder.to_str().unwrap(),
            "--system-prompt",
            "system.md.j2",
            "--model",
            "gemini-2.5-pro",
            "--var",
            "custom=hello",
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

    let rendered_path = temp_dir
        .path()
        .join("runtime-home/.claude/runtime/compose/atm-dev/templated-agent/gemini-system.md");
    assert!(
        rendered_path.exists(),
        "templated system prompt should be rendered before daemon readiness check"
    );

    let rendered = fs::read_to_string(&rendered_path).unwrap();
    let canonical_folder = fs::canonicalize(&folder).unwrap();
    assert!(rendered.contains("team=atm-dev"));
    assert!(rendered.contains("agent=templated-agent"));
    assert!(rendered.contains("runtime=gemini"));
    assert!(rendered.contains(&format!("cwd={}", canonical_folder.to_string_lossy())));
    assert!(rendered.contains("model=gemini-2.5-pro"));
    assert!(rendered.contains("custom=hello"));
}

#[test]
fn test_spawn_system_prompt_plain_markdown_does_not_create_composed_prompt_file() {
    let temp_dir = TempDir::new().unwrap();
    let workdir = temp_dir.path().join("workdir");
    let folder = workdir.join("plain-spawn");
    fs::create_dir_all(&folder).unwrap();

    fs::write(folder.join("system.md"), "plain system prompt").unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_IDENTITY", "team-lead")
        .args([
            "teams",
            "spawn",
            "plain-agent",
            "--team",
            "atm-dev",
            "--runtime",
            "gemini",
            "--folder",
            folder.to_str().unwrap(),
            "--system-prompt",
            "system.md",
            "--json",
        ])
        .assert()
        .failure();

    let rendered_path = temp_dir
        .path()
        .join("runtime-home/.claude/runtime/compose/atm-dev/plain-agent/gemini-system.md");
    assert!(
        !rendered_path.exists(),
        "plain markdown system prompt should bypass compose rendering path"
    );
}

#[test]
fn test_spawn_var_requires_key_value_format() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("workdir").join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.args([
        "teams",
        "spawn",
        "agent-invalid-var",
        "--team",
        "atm-dev",
        "--runtime",
        "gemini",
        "--folder",
        folder.to_str().unwrap(),
        "--var",
        "NOT_A_PAIR",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("Invalid --var value"));
}

#[test]
fn test_spawn_resume_and_continue_are_mutually_exclusive() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("workdir").join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.args([
        "teams",
        "spawn",
        "agent-conflict",
        "--team",
        "atm-dev",
        "--runtime",
        "gemini",
        "--folder",
        folder.to_str().unwrap(),
        "--resume",
        "abc123",
        "--continue",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn test_spawn_continue_without_tracked_session_returns_stable_not_found_code() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("workdir").join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_IDENTITY", "team-lead")
        .args([
            "teams",
            "spawn",
            "agent-missing-session",
            "--team",
            "atm-dev",
            "--runtime",
            "gemini",
            "--folder",
            folder.to_str().unwrap(),
            "--continue",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("SESSION_ID_NOT_FOUND"));
}

#[cfg(unix)]
#[test]
#[serial]
fn test_spawn_resume_prefix_ambiguous_returns_stable_error_code() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("workdir").join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();

    let script = write_fake_spawn_session_daemon_script(temp_dir.path());
    let runtime_home = temp_dir.path().join("runtime-home");
    fs::create_dir_all(&runtime_home).unwrap();
    let mut daemon = Command::new(&script)
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", temp_dir.path())
        .envs([("ATM_HOME", temp_dir.path())])
        .spawn()
        .expect("failed to launch fake daemon");
    wait_for_daemon_socket(&runtime_home);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env("ATM_IDENTITY", "team-lead")
        .args([
            "teams",
            "spawn",
            "agent-ambiguous",
            "--team",
            "atm-dev",
            "--runtime",
            "codex",
            "--folder",
            folder.to_str().unwrap(),
            "--resume",
            "abc",
        ])
        .assert()
        .failure();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("SESSION_ID_AMBIGUOUS"),
        "expected SESSION_ID_AMBIGUOUS, got: {stderr}"
    );

    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
fn test_spawn_env_team_mismatch_requires_override_team() {
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
    assert!(stdout.contains("Launch command:"));
    assert!(stderr.contains("Warning: team mismatch detected"));
    assert!(stderr.contains("ATM_TEAM ('env-team')"));
    assert!(stderr.contains(".atm.toml default_team ('toml-team')"));
    assert!(stderr.contains("--override-team"));
    assert_eq!(fs::read_to_string(&atm_toml_path).unwrap(), toml_content);
}

#[test]
fn test_spawn_env_team_mismatch_override_team_uses_env_team_without_modifying_toml() {
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
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("Warning: team mismatch detected"));
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
#[test]
fn test_spawn_env_team_matching_toml_does_not_require_override() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(
        workdir.join(".atm.toml"),
        "[core]\ndefault_team = \"env-team\"\nidentity = \"team-lead\"\n",
    )
    .unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env("ATM_TEAM", "env-team")
        .args([
            "teams",
            "spawn",
            "agent-match",
            "--runtime",
            "codex",
            "--folder",
            folder.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["team"].as_str(), Some("env-team"));
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not running")
    );
    assert!(!stderr.contains("Warning: team mismatch detected"));
    assert!(!stderr.contains("--override-team"));
}

#[test]
fn test_spawn_help_without_atm_toml_includes_generated_launch_reference() {
    let temp_dir = TempDir::new().unwrap();
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd.args(["teams", "spawn", "--help"]).assert().success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Launch command output:"));
    assert!(stdout.contains("exact copy/paste launch command"));
    assert!(stdout.contains("atm teams spawn test-member-3 --runtime claude"));
    assert!(stdout.contains("--color cyan --model haiku"));
}

#[test]
fn test_spawn_policy_blocks_unauthorized_identity() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(
        workdir.join(".atm.toml"),
        r#"
[core]
default_team = "atm-dev"
identity = "team-lead"

[team."atm-dev"]
spawn_policy = "leaders-only"
co_leaders = ["arch-atm"]
"#
        .trim_start(),
    )
    .unwrap();
    write_team_config(&temp_dir, "atm-dev");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env("ATM_IDENTITY", "dev-1")
        .args([
            "teams",
            "spawn",
            "agent-policy",
            "--runtime",
            "codex",
            "--folder",
            folder.to_str().unwrap(),
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stdout.contains("Launch command:"));
    assert!(
        stderr.contains("SPAWN_UNAUTHORIZED"),
        "expected SPAWN_UNAUTHORIZED for unknown caller, got stderr: {stderr}"
    );
}

#[test]
fn test_spawn_policy_allows_co_leader() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(
        workdir.join(".atm.toml"),
        r#"
[core]
default_team = "atm-dev"
identity = "team-lead"

[team."atm-dev"]
spawn_policy = "leaders-only"
co_leaders = ["arch-atm"]
"#
        .trim_start(),
    )
    .unwrap();
    write_team_config(&temp_dir, "atm-dev");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env("ATM_IDENTITY", "arch-atm")
        .args([
            "teams",
            "spawn",
            "agent-policy",
            "--runtime",
            "codex",
            "--folder",
            folder.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not running")
    );
    assert!(!stderr.contains("SPAWN_UNAUTHORIZED"));
}

#[test]
fn test_spawn_policy_allows_team_lead_identity() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(
        workdir.join(".atm.toml"),
        r#"
[core]
default_team = "atm-dev"
identity = "team-lead"

[team."atm-dev"]
spawn_policy = "leaders-only"
co_leaders = ["arch-atm"]
"#
        .trim_start(),
    )
    .unwrap();
    write_team_config(&temp_dir, "atm-dev");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env("ATM_IDENTITY", "team-lead")
        .args([
            "teams",
            "spawn",
            "agent-policy",
            "--runtime",
            "codex",
            "--folder",
            folder.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not running")
    );
    assert!(!stderr.contains("SPAWN_UNAUTHORIZED"));
}

#[test]
fn test_spawn_policy_named_spawn_without_team_name_still_checked() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(
        workdir.join(".atm.toml"),
        r#"
[core]
default_team = "atm-dev"
identity = "team-lead"

[team."atm-dev"]
spawn_policy = "leaders-only"
co_leaders = []
"#
        .trim_start(),
    )
    .unwrap();
    write_team_config(&temp_dir, "atm-dev");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_IDENTITY", "dev-1")
        .args([
            "teams",
            "spawn",
            "named-worker",
            "--runtime",
            "codex",
            "--folder",
            folder.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("SPAWN_UNAUTHORIZED"));
}

#[test]
fn test_spawn_policy_blocks_unknown_caller_identity_with_preview() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(
        workdir.join(".atm.toml"),
        r#"
[core]
default_team = "atm-dev"
identity = "human"

[team."atm-dev"]
spawn_policy = "leaders-only"
co_leaders = []
"#
        .trim_start(),
    )
    .unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args([
            "teams",
            "spawn",
            "unknown-caller",
            "--runtime",
            "codex",
            "--folder",
            folder.to_str().unwrap(),
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stdout.contains("Launch command:"));
    assert!(
        stderr.contains("SPAWN_UNAUTHORIZED"),
        "expected SPAWN_UNAUTHORIZED for unknown caller, got stderr: {stderr}"
    );
    assert!(stderr.contains("Resolved caller: <unknown>"));
}

#[test]
fn test_spawn_policy_json_unauthorized_includes_launch_command() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(
        workdir.join(".atm.toml"),
        r#"
[core]
default_team = "atm-dev"
identity = "team-lead"

[team."atm-dev"]
spawn_policy = "leaders-only"
co_leaders = []
"#
        .trim_start(),
    )
    .unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env("ATM_IDENTITY", "dev-1")
        .args([
            "teams",
            "spawn",
            "agent-policy",
            "--runtime",
            "codex",
            "--folder",
            folder.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("SPAWN_UNAUTHORIZED")
    );
    assert!(
        parsed["launch_command"]
            .as_str()
            .expect("launch_command must be present in unauthorized JSON output")
            .contains("codex")
    );
    assert!(stderr.contains("Launch command:"));
}

#[test]
fn test_spawn_policy_allows_team_lead_explicitly() {
    // team-lead identity must pass the gate (get daemon-not-running, not SPAWN_UNAUTHORIZED)
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(
        workdir.join(".atm.toml"),
        r#"
[core]
default_team = "atm-dev"
identity = "team-lead"

[team."atm-dev"]
spawn_policy = "leaders-only"
co_leaders = []
"#
        .trim_start(),
    )
    .unwrap();
    write_team_config(&temp_dir, "atm-dev");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env("ATM_IDENTITY", "team-lead")
        .args([
            "teams",
            "spawn",
            "some-agent",
            "--runtime",
            "codex",
            "--folder",
            folder.to_str().unwrap(),
        ])
        .assert()
        .failure();

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        !stderr.contains("SPAWN_UNAUTHORIZED"),
        "team-lead should not get SPAWN_UNAUTHORIZED, got: {stderr}"
    );
    // Positive assertion: must fail for the expected reason (daemon unavailable), not silently
    assert!(
        stderr.contains("Daemon") || stdout.contains("Daemon") || stdout.contains("daemon"),
        "team-lead should fail with daemon-unavailable error, got stderr={stderr} stdout={stdout}"
    );
}

#[test]
fn test_spawn_policy_defaults_leaders_only_when_no_team_section() {
    // .atm.toml with [core] but no [team."atm-dev"] — must default to leaders-only
    // and block non-lead without a TOML parse error
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("spawn-folder");
    fs::create_dir_all(&folder).unwrap();
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(
        workdir.join(".atm.toml"),
        r#"
[core]
default_team = "atm-dev"
identity = "team-lead"
"#
        .trim_start(),
    )
    .unwrap();
    write_team_config(&temp_dir, "atm-dev");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_IDENTITY", "dev-1")
        .args([
            "teams",
            "spawn",
            "some-agent",
            "--runtime",
            "codex",
            "--folder",
            folder.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("SPAWN_UNAUTHORIZED"));
}
