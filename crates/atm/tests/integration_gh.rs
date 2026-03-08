//! Integration tests for `atm gh ...` daemon-routed commands.

use assert_cmd::cargo;
use predicates::prelude::PredicateBooleanExt;
use std::fs;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Child, Command};
#[cfg(unix)]
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn write_repo_gh_monitor_config(workdir: &Path, team: &str) {
    let content = format!(
        r#"[core]
default_team = "{team}"
identity = "team-lead"

[plugins.gh_monitor]
enabled = true
provider = "github"
team = "{team}"
agent = "gh-monitor"
repo = "agent-team-mail"
poll_interval_secs = 60
"#
    );
    fs::write(workdir.join(".atm.toml"), content).unwrap();
}

fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir, team: &str, with_plugin: bool) {
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).ok();
    if with_plugin {
        write_repo_gh_monitor_config(&workdir, team);
    }
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
health_path = daemon_dir / "gh-health.json"
configured = os.environ.get("ATM_FAKE_GH_CONFIGURED", "1") == "1"
enabled = os.environ.get("ATM_FAKE_GH_ENABLED", "1") == "1"
availability_state = "healthy" if configured and enabled else "disabled_config_error"
availability_message = None
if not configured:
    availability_message = "gh_monitor plugin is not configured"
elif not enabled:
    availability_message = "gh_monitor plugin is disabled in configuration"

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
                "configured": True,
                "enabled": True,
                "config_source": "repo",
                "config_path": str(home / "workdir" / ".atm.toml"),
                "target_kind": payload.get("target_kind", "workflow"),
                "target": payload.get("target", "ci"),
                "state": "tracking",
                "configured": configured,
                "enabled": enabled,
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
                    "configured": True,
                    "enabled": True,
                    "config_source": "repo",
                    "config_path": str(home / "workdir" / ".atm.toml"),
                    "target_kind": payload.get("target_kind", "workflow"),
                    "target": payload.get("target", "ci"),
                    "state": "tracking",
                    "configured": configured,
                    "enabled": enabled,
                    "run_id": 987654,
                    "reference": "develop",
                    "updated_at": "2026-03-06T03:00:00Z",
                    "message": None,
                }
            resp = {"version": 1, "request_id": request_id, "status": "ok", "payload": status_payload}
        elif command == "gh-monitor-control":
            action = payload.get("action", "start")
            if action == "stop":
                health_payload = {
                    "team": payload.get("team", "test-team"),
                    "configured": True,
                    "enabled": True,
                    "config_source": "repo",
                    "config_path": str(home / "workdir" / ".atm.toml"),
                    "lifecycle_state": "stopped",
                    "availability_state": availability_state,
                    "configured": configured,
                    "enabled": enabled,
                    "in_flight": 0,
                    "updated_at": "2026-03-06T03:00:00Z",
                    "message": availability_message or "stopped",
                }
            elif action == "restart":
                health_payload = {
                    "team": payload.get("team", "test-team"),
                    "configured": True,
                    "enabled": True,
                    "config_source": "repo",
                    "config_path": str(home / "workdir" / ".atm.toml"),
                    "lifecycle_state": "running",
                    "availability_state": availability_state,
                    "configured": configured,
                    "enabled": enabled,
                    "in_flight": 0,
                    "updated_at": "2026-03-06T03:00:00Z",
                    "message": availability_message or "restarted",
                }
            else:
                health_payload = {
                    "team": payload.get("team", "test-team"),
                    "configured": True,
                    "enabled": True,
                    "config_source": "repo",
                    "config_path": str(home / "workdir" / ".atm.toml"),
                    "lifecycle_state": "running",
                    "availability_state": availability_state,
                    "configured": configured,
                    "enabled": enabled,
                    "in_flight": 0,
                    "updated_at": "2026-03-06T03:00:00Z",
                    "message": availability_message or "started",
                }
            health_path.write_text(json.dumps(health_payload))
            resp = {"version": 1, "request_id": request_id, "status": "ok", "payload": health_payload}
        elif command == "gh-monitor-health":
            if health_path.exists():
                health_payload = json.loads(health_path.read_text())
            else:
                health_payload = {
                    "team": payload.get("team", "test-team"),
                    "configured": True,
                    "enabled": True,
                    "config_source": "repo",
                    "config_path": str(home / "workdir" / ".atm.toml"),
                    "lifecycle_state": "running",
                    "availability_state": availability_state,
                    "configured": configured,
                    "enabled": enabled,
                    "in_flight": 0,
                    "updated_at": "2026-03-06T03:00:00Z",
                    "message": availability_message,
                }
            resp = {"version": 1, "request_id": request_id, "status": "ok", "payload": health_payload}
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
    {
        let mut file = fs::File::create(&script).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        file.sync_all().unwrap();
    }
    let mut perms = fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();
    // Guard against file-write races on CI where the script can be executed
    // before write visibility/metadata updates fully settle.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if fs::read_to_string(&script)
            .map(|content| content == body)
            .unwrap_or(false)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "fake gh daemon script did not become readable in time"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
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
    start_fake_gh_daemon_with_mode(home, true, true)
}

#[cfg(unix)]
fn start_fake_gh_daemon_with_mode(home: &Path, configured: bool, enabled: bool) -> Child {
    let script = write_fake_gh_daemon_script(home);
    // Retry on ETXTBUSY (code 26): Linux can transiently block execution of a
    // newly-written file on CI while kernel mappings settle.
    let child = {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match Command::new(&script)
                .env("ATM_HOME", home)
                .env("ATM_FAKE_GH_CONFIGURED", if configured { "1" } else { "0" })
                .env("ATM_FAKE_GH_ENABLED", if enabled { "1" } else { "0" })
                .spawn()
            {
                Ok(child) => break child,
                Err(e) if e.raw_os_error() == Some(26) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("failed to spawn fake gh daemon: {e}"),
            }
        }
    };
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
    set_home_env(&mut monitor, &temp_dir, "test-team", true);
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
    assert_eq!(monitor_json["configured"].as_bool(), Some(true));
    assert_eq!(monitor_json["enabled"].as_bool(), Some(true));

    let mut status = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut status, &temp_dir, "test-team", true);
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
    assert_eq!(status_json["configured"].as_bool(), Some(true));
    assert_eq!(status_json["enabled"].as_bool(), Some(true));

    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
#[cfg(windows)]
fn test_gh_command_surface_compiles_on_windows() {
    let _ = agent_team_mail_core::daemon_client::gh_monitor;
    let _ = agent_team_mail_core::daemon_client::gh_status;
    let _ = agent_team_mail_core::daemon_client::gh_monitor_control;
    let _ = agent_team_mail_core::daemon_client::gh_monitor_health;
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_lifecycle_status_roundtrip_json() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let mut daemon = start_fake_gh_daemon(temp_dir.path());

    let mut start = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut start, &temp_dir, "test-team", true);
    let start_output = start
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("monitor")
        .arg("start")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let start_json: serde_json::Value = serde_json::from_slice(&start_output).unwrap();
    assert_eq!(start_json["team"].as_str(), Some("test-team"));
    assert_eq!(start_json["lifecycle_state"].as_str(), Some("running"));

    let mut health = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut health, &temp_dir, "test-team", true);
    let health_output = health
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("monitor")
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let health_json: serde_json::Value = serde_json::from_slice(&health_output).unwrap();
    assert_eq!(health_json["team"].as_str(), Some("test-team"));
    assert_eq!(health_json["availability_state"].as_str(), Some("healthy"));
    assert_eq!(health_json["configured"].as_bool(), Some(true));
    assert_eq!(health_json["enabled"].as_bool(), Some(true));

    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
#[cfg(unix)]
fn test_gh_status_preflight_disabled_config_shows_atm_gh_init_remediation() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let mut daemon = start_fake_gh_daemon_with_mode(temp_dir.path(), false, false);

    let mut status = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut status, &temp_dir, "test-team", false);
    let output = status
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("status")
        .arg("workflow")
        .arg("ci")
        .output()
        .expect("run atm gh status");

    assert!(
        !output.status.success(),
        "status should fail when gh_monitor is not configured"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("missing [plugins.gh_monitor] configuration"));
    assert!(
        stderr.contains("Run `atm gh init` to configure and enable GitHub monitor for this team.")
    );

    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_status_accepts_json_flag_after_subcommand() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let mut daemon = start_fake_gh_daemon(temp_dir.path());

    let mut health = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut health, &temp_dir, "test-team", true);
    let health_output = health
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("monitor")
        .arg("status")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let health_json: serde_json::Value = serde_json::from_slice(&health_output).unwrap();
    assert_eq!(health_json["team"].as_str(), Some("test-team"));
    assert_eq!(health_json["lifecycle_state"].as_str(), Some("running"));

    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_status_human_output_is_single_block() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let mut daemon = start_fake_gh_daemon(temp_dir.path());

    let mut health = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut health, &temp_dir, "test-team", true);
    let health_output = health
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("monitor")
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(health_output).unwrap();
    assert_eq!(stdout.matches("Team:").count(), 1);
    assert_eq!(stdout.matches("Lifecycle:").count(), 1);
    assert_eq!(stdout.matches("Availability:").count(), 1);

    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[cfg(unix)]
fn install_fake_gh_cli(temp_dir: &TempDir) -> PathBuf {
    let script = temp_dir.path().join("gh");
    let body = r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "gh version 2.0.0"
  exit 0
fi
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  echo "Logged in"
  exit 0
fi
echo "unsupported gh args: $*" >&2
exit 1
"#;
    fs::write(&script, body).unwrap();
    let mut perms = fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();
    script
}

#[test]
#[cfg(unix)]
fn test_gh_namespace_status_no_subcommand_returns_json_status() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let mut daemon = start_fake_gh_daemon(temp_dir.path());

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", true);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["team"].as_str(), Some("test-team"));
    assert!(json["configured"].is_boolean());
    assert!(json["enabled"].is_boolean());
    assert!(json["availability_state"].is_string());
    assert!(json["actions"].is_array());

    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_fails_with_actionable_guidance_when_plugin_unconfigured() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    cmd.env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("monitor")
        .arg("run")
        .arg("123")
        .assert()
        .failure()
        .stderr(predicates::str::contains("Run `atm gh init`"));
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_json_unavailable_emits_structured_error() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    cmd.env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("monitor")
        .arg("run")
        .arg("123")
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "\"error_code\": \"PLUGIN_UNAVAILABLE\"",
        ))
        .stderr(predicates::str::contains("\"hint\":"))
        .stderr(predicates::str::contains("atm gh init"))
        .stderr(predicates::str::contains("Error:").not());
}

#[test]
#[cfg(unix)]
fn test_gh_init_dry_run_does_not_write_config() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let gh_path = install_fake_gh_cli(&temp_dir);
    let path_env = format!(
        "{}:{}",
        gh_path.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    let output = cmd
        .env("PATH", path_env)
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("init")
        .arg("--dry-run")
        .arg("--repo")
        .arg("acme/agent-team-mail")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["dry_run"].as_bool(), Some(true));
    assert_eq!(json["notify_target"].as_str(), Some("team-lead"));

    let workdir = temp_dir.path().join("workdir");
    assert!(
        !workdir.join(".atm.toml").exists(),
        "dry-run must not write .atm.toml"
    );
}

#[test]
#[cfg(unix)]
fn test_gh_init_writes_plugin_config() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let gh_path = install_fake_gh_cli(&temp_dir);
    let path_env = format!(
        "{}:{}",
        gh_path.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    cmd.env("PATH", path_env)
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("init")
        .arg("--repo")
        .arg("acme/agent-team-mail")
        .assert()
        .success();

    let workdir = temp_dir.path().join("workdir");
    let cfg = fs::read_to_string(workdir.join(".atm.toml")).unwrap();
    assert!(cfg.contains("[plugins.gh_monitor]"));
    assert!(cfg.contains("enabled = true"));
    assert!(cfg.contains("team = \"test-team\""));
    assert!(cfg.contains("repo = \"agent-team-mail\""));
    assert!(cfg.contains("notify_target = \"team-lead\""));
}

#[test]
#[cfg(unix)]
fn test_gh_status_surfaces_consistent_when_daemon_unreachable() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");

    let mut ns = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut ns, &temp_dir, "test-team", true);
    let ns_out = ns
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ns_json: serde_json::Value = serde_json::from_slice(&ns_out).unwrap();

    let mut status = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut status, &temp_dir, "test-team", true);
    let status_out = status
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status_json: serde_json::Value = serde_json::from_slice(&status_out).unwrap();

    let mut monitor_status = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut monitor_status, &temp_dir, "test-team", true);
    let monitor_out = monitor_status
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("monitor")
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let monitor_json: serde_json::Value = serde_json::from_slice(&monitor_out).unwrap();

    assert_eq!(
        ns_json["availability_state"].as_str(),
        status_json["availability_state"].as_str()
    );
    assert_eq!(
        status_json["availability_state"].as_str(),
        monitor_json["availability_state"].as_str()
    );
    assert_eq!(ns_json["message"].as_str(), status_json["message"].as_str());
    assert_eq!(
        status_json["message"].as_str(),
        monitor_json["message"].as_str()
    );
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_status_json_has_stable_schema() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let mut daemon = start_fake_gh_daemon(temp_dir.path());

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", true);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("monitor")
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    for key in [
        "team",
        "configured",
        "enabled",
        "lifecycle_state",
        "availability_state",
        "in_flight",
        "updated_at",
        "actions",
    ] {
        assert!(json.get(key).is_some(), "missing key: {key}");
    }
    assert!(json["actions"].is_array());

    let _ = daemon.kill();
    let _ = daemon.wait();
}
