//! Integration tests for `atm gh ...` daemon-routed commands.

#[cfg(unix)]
use agent_team_mail_core::consts::WAIT_FOR_DAEMON_SOCKET_SECS;
use assert_cmd::cargo;
use predicates::prelude::PredicateBooleanExt;
#[cfg(unix)]
use serial_test::serial;
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

fn write_repo_gh_monitor_config_with_owner(workdir: &Path, team: &str, owner: &str, repo: &str) {
    let content = format!(
        r#"[core]
default_team = "{team}"
identity = "team-lead"

[plugins.gh_monitor]
enabled = true
provider = "github"
team = "{team}"
agent = "gh-monitor"
owner = "{owner}"
repo = "{repo}"
poll_interval_secs = 60
"#
    );
    fs::write(workdir.join(".atm.toml"), content).unwrap();
}

fn write_repo_gh_monitor_config_missing_repo(workdir: &Path, team: &str) {
    std::fs::create_dir_all(workdir).unwrap();
    let content = format!(
        r#"[core]
default_team = "{team}"
identity = "team-lead"

[plugins.gh_monitor]
enabled = true
provider = "github"
team = "{team}"
agent = "gh-monitor"
poll_interval_secs = 60
"#
    );
    fs::write(workdir.join(".atm.toml"), content).unwrap();
}

fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir, team: &str, with_plugin: bool) {
    let workdir = temp_dir.path().join("workdir");
    let fake_daemon_bin = temp_dir.path().join("fake-gh-daemon.py");
    std::fs::create_dir_all(&workdir).ok();
    if with_plugin {
        write_repo_gh_monitor_config(&workdir, team);
    }
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env("ATM_DAEMON_BIN", &fake_daemon_bin)
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
import time
from pathlib import Path

home = Path(os.environ["ATM_HOME"])
daemon_dir = home / ".atm" / "daemon"
daemon_dir.mkdir(parents=True, exist_ok=True)
state_path = daemon_dir / "gh-state.json"
health_path = daemon_dir / "gh-health.json"
request_log_path = os.environ.get("ATM_FAKE_GH_REQUEST_LOG")
configured = os.environ.get("ATM_FAKE_GH_CONFIGURED", "1") == "1"
enabled = os.environ.get("ATM_FAKE_GH_ENABLED", "1") == "1"
monitor_delay_ms = int(os.environ.get("ATM_FAKE_GH_MONITOR_DELAY_MS", "0") or "0")
control_delay_ms = int(os.environ.get("ATM_FAKE_GH_CONTROL_DELAY_MS", "0") or "0")
daemon_version = os.environ.get("ATM_FAKE_DAEMON_VERSION", "0.0.0")
availability_state = "healthy" if configured and enabled else "disabled_config_error"
availability_message = None
if not configured:
    availability_message = "gh_monitor plugin is not configured"
elif not enabled:
    availability_message = "gh_monitor plugin is disabled in configuration"

sock_path = daemon_dir / "atm-daemon.sock"
pid_path = daemon_dir / "atm-daemon.pid"
metadata_path = home / ".config" / "atm" / "daemon.lock.meta.json"
if sock_path.exists():
    sock_path.unlink()
pid_path.write_text(str(os.getpid()))
metadata_path.parent.mkdir(parents=True, exist_ok=True)
metadata_path.write_text(json.dumps({
    "pid": os.getpid(),
    "executable_path": str(Path(__file__).resolve()),
    "home_scope": str(home.resolve()),
    "version": daemon_version,
    "written_at": "2026-03-09T00:00:00Z"
}))

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
        if request_log_path:
            request_log_file = Path(request_log_path)
            request_log_file.parent.mkdir(parents=True, exist_ok=True)
            request_log_file.write_text(json.dumps({
                "command": command,
                "payload": payload,
            }))

        if command == "gh-monitor":
            if monitor_delay_ms > 0:
                time.sleep(monitor_delay_ms / 1000.0)
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
            if control_delay_ms > 0:
                time.sleep(control_delay_ms / 1000.0)
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

        try:
            conn.sendall((json.dumps(resp) + "\n").encode())
        except BrokenPipeError:
            pass

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
    let socket = home.join(".atm/daemon/atm-daemon.sock");
    let deadline = Instant::now() + Duration::from_secs(WAIT_FOR_DAEMON_SOCKET_SECS);
    while Instant::now() < deadline {
        if socket.exists() && std::os::unix::net::UnixStream::connect(&socket).is_ok() {
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
    start_fake_gh_daemon_with_mode_and_delays(home, true, true, 0, 0, None)
}

#[cfg(unix)]
fn start_fake_gh_daemon_with_mode(home: &Path, configured: bool, enabled: bool) -> Child {
    start_fake_gh_daemon_with_mode_and_delays(home, configured, enabled, 0, 0, None)
}

/// Start the fake daemon with an explicit request log path.
///
/// Use instead of `start_fake_gh_daemon` when the test needs to inspect the
/// last socket request the daemon received. Passing the path here (rather than
/// setting `ATM_FAKE_GH_REQUEST_LOG` in the test-process environment) prevents
/// concurrently-started daemons from other tests from inheriting the env var
/// and accidentally overwriting the log.
#[cfg(unix)]
fn start_fake_gh_daemon_with_request_log(home: &Path, request_log: &std::path::Path) -> Child {
    start_fake_gh_daemon_with_mode_and_delays(home, true, true, 0, 0, Some(request_log))
}

#[cfg(unix)]
fn start_fake_gh_daemon_with_mode_and_delays(
    home: &Path,
    configured: bool,
    enabled: bool,
    monitor_delay_ms: u64,
    control_delay_ms: u64,
    request_log: Option<&std::path::Path>,
) -> Child {
    let script = write_fake_gh_daemon_script(home);
    // Retry on ETXTBUSY (code 26): Linux can transiently block execution of a
    // newly-written file on CI while kernel mappings settle.
    let child = {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let mut cmd = Command::new(&script);
            cmd.env("ATM_HOME", home)
                .env("ATM_FAKE_GH_CONFIGURED", if configured { "1" } else { "0" })
                .env("ATM_FAKE_GH_ENABLED", if enabled { "1" } else { "0" })
                .env("ATM_FAKE_GH_MONITOR_DELAY_MS", monitor_delay_ms.to_string())
                .env("ATM_FAKE_GH_CONTROL_DELAY_MS", control_delay_ms.to_string())
                .env("ATM_FAKE_DAEMON_VERSION", env!("CARGO_PKG_VERSION"));
            // Explicitly control ATM_FAKE_GH_REQUEST_LOG: always override the
            // inherited process-env value so that parallel serial/non-serial tests
            // cannot write to each other's log files.
            match request_log {
                Some(path) => cmd.env("ATM_FAKE_GH_REQUEST_LOG", path),
                None => cmd.env_remove("ATM_FAKE_GH_REQUEST_LOG"),
            };
            match cmd.spawn() {
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

#[cfg(unix)]
fn write_fake_gh_cli_script(home: &Path, expected_repo: &str) -> PathBuf {
    let bin_dir = home.join("fake-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let script = bin_dir.join("gh");
    let body = format!(
        r#"#!/usr/bin/env python3
import json
import sys

args = sys.argv[1:]

if "--version" in args:
    print("gh version 2.0.0")
    sys.exit(0)

if len(args) >= 2 and args[0] == "auth" and args[1] == "status":
    print("Logged in to github.com")
    sys.exit(0)

if "pr" in args and "list" in args:
    if "-R" not in args:
        print("missing -R", file=sys.stderr)
        sys.exit(2)
    repo = args[args.index("-R") + 1]
    if repo != "{expected_repo}":
        print(f"unexpected repo scope: {{repo}}", file=sys.stderr)
        sys.exit(2)

    payload = [
        {{
            "number": 101,
            "title": "Add monitor dashboard",
            "url": "https://github.com/{expected_repo}/pull/101",
            "isDraft": False,
            "reviewDecision": "APPROVED",
            "mergeStateStatus": "CLEAN",
            "statusCheckRollup": [
                {{"conclusion": "SUCCESS"}},
                {{"status": "IN_PROGRESS"}}
            ]
        }},
        {{
            "number": 102,
            "title": "Fix flaky monitor test",
            "url": "https://github.com/{expected_repo}/pull/102",
            "isDraft": True,
            "reviewDecision": "CHANGES_REQUESTED",
            "mergeStateStatus": "DIRTY",
            "statusCheckRollup": [
                {{"conclusion": "FAILURE"}}
            ]
        }}
    ]
    print(json.dumps(payload))
    sys.exit(0)

if "pr" in args and "view" in args:
    if "-R" not in args:
        print("missing -R", file=sys.stderr)
        sys.exit(2)
    repo = args[args.index("-R") + 1]
    if repo != "{expected_repo}":
        print(f"unexpected repo scope: {{repo}}", file=sys.stderr)
        sys.exit(2)

    pr_number = None
    if "view" in args:
        view_idx = args.index("view")
        if view_idx + 1 < len(args):
            pr_number = args[view_idx + 1]
    if pr_number not in ("101", "103"):
        print(f"unexpected PR: {{pr_number}}", file=sys.stderr)
        sys.exit(2)

    if pr_number == "101":
        payload = {{
            "number": 101,
            "title": "Add monitor dashboard",
            "url": "https://github.com/{expected_repo}/pull/101",
            "isDraft": False,
            "reviewDecision": "APPROVED",
            "mergeStateStatus": "CLEAN",
            "mergeable": "UNKNOWN",
            "statusCheckRollup": [
                {{
                    "name": "clippy",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "startedAt": "2026-03-09T01:00:00Z",
                    "completedAt": "2026-03-09T01:02:00Z",
                    "detailsUrl": "https://github.com/{expected_repo}/actions/runs/1"
                }},
                {{
                    "context": "required-review",
                    "state": "PENDING",
                    "targetUrl": "https://github.com/{expected_repo}/checks/2"
                }}
            ],
            "reviews": [
                {{
                    "author": {{"login": "alice"}},
                    "state": "APPROVED",
                    "submittedAt": "2026-03-09T02:00:00Z"
                }},
                {{
                    "author": {{"login": "bob"}},
                    "state": "COMMENTED",
                    "submittedAt": "2026-03-09T02:30:00Z"
                }}
            ]
        }}
    else:
        payload = {{
            "number": 103,
            "title": "Skipped checks should still pass",
            "url": "https://github.com/{expected_repo}/pull/103",
            "isDraft": False,
            "reviewDecision": "",
            "mergeStateStatus": "UNKNOWN",
            "mergeable": "UNKNOWN",
            "statusCheckRollup": [
                {{
                    "name": "fmt",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "startedAt": "2026-03-09T03:00:00Z",
                    "completedAt": "2026-03-09T03:01:00Z",
                    "detailsUrl": "https://github.com/{expected_repo}/actions/runs/3"
                }},
                {{
                    "name": "optional-check",
                    "status": "COMPLETED",
                    "conclusion": "SKIPPED",
                    "startedAt": "2026-03-09T03:00:00Z",
                    "completedAt": "2026-03-09T03:01:00Z",
                    "detailsUrl": "https://github.com/{expected_repo}/actions/runs/4"
                }}
            ],
            "reviews": []
        }}
    print(json.dumps(payload))
    sys.exit(0)

print("unsupported gh invocation: " + " ".join(args), file=sys.stderr)
sys.exit(1)
"#
    );

    {
        let mut file = fs::File::create(&script).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        file.sync_all().unwrap();
    }
    let mut perms = fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();
    bin_dir
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_and_control_allow_daemon_responses_over_500ms() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).unwrap();
    init_git_repo_with_origin(&workdir, "https://github.com/acme/agent-team-mail.git");
    let mut daemon =
        start_fake_gh_daemon_with_mode_and_delays(temp_dir.path(), true, true, 750, 750, None);

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
    assert_eq!(monitor_json["state"].as_str(), Some("tracking"));
    assert_eq!(monitor_json["target_kind"].as_str(), Some("workflow"));

    let mut stop = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut stop, &temp_dir, "test-team", true);
    let stop_output = stop
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("monitor")
        .arg("stop")
        .arg("--drain-timeout")
        .arg("1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stop_json: serde_json::Value = serde_json::from_slice(&stop_output).unwrap();
    assert_eq!(stop_json["lifecycle_state"].as_str(), Some("stopped"));

    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
#[cfg(unix)]
#[serial]
fn test_gh_monitor_workflow_roundtrip_json() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).unwrap();
    init_git_repo_with_origin(&workdir, "https://github.com/acme/agent-team-mail.git");
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
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).unwrap();
    init_git_repo_with_origin(&workdir, "https://github.com/acme/agent-team-mail.git");
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
#[serial]
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
    assert!(stderr.contains("gh_monitor plugin is not configured"));
    assert!(stderr.contains("Remediation: run `atm gh init` and retry."));

    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
#[cfg(unix)]
#[serial]
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
#[serial]
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

#[cfg(unix)]
fn init_git_repo_with_origin(workdir: &Path, origin_url: &str) {
    let init_status = Command::new("git")
        .args(["init"])
        .current_dir(workdir)
        .status()
        .expect("git init should run");
    assert!(init_status.success(), "git init failed");

    let remote_status = Command::new("git")
        .args(["remote", "add", "origin", origin_url])
        .current_dir(workdir)
        .status()
        .expect("git remote add origin should run");
    assert!(remote_status.success(), "git remote add origin failed");
}

#[test]
#[cfg(unix)]
#[serial]
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
        .stderr(predicates::str::contains("atm gh init"));
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
    assert!(cfg.contains("repo = \"acme/agent-team-mail\""));
    assert!(cfg.contains("notify_target = \"team-lead\""));
}

#[test]
#[cfg(unix)]
fn test_gh_init_auto_populates_repo_from_git_remote() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let gh_path = install_fake_gh_cli(&temp_dir);
    let path_env = format!(
        "{}:{}",
        gh_path.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).unwrap();
    init_git_repo_with_origin(&workdir, "https://github.com/acme/agent-team-mail.git");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    cmd.env("PATH", path_env)
        .env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("init")
        .assert()
        .success();

    let cfg = fs::read_to_string(workdir.join(".atm.toml")).unwrap();
    assert!(cfg.contains("repo = \"acme/agent-team-mail\""));
    assert!(cfg.contains("owner = \"acme\""));
}

#[test]
#[cfg(unix)]
#[serial]
fn test_gh_monitor_infers_repo_scope_from_git_remote() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let request_log = temp_dir.path().join("request-log.json");
    // Pass the log path directly to the daemon rather than via process env so
    // that concurrently-running non-serial tests cannot inherit ATM_FAKE_GH_REQUEST_LOG
    // and accidentally overwrite this test's log with their own daemon requests.
    let mut daemon = start_fake_gh_daemon_with_request_log(temp_dir.path(), &request_log);
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).unwrap();
    init_git_repo_with_origin(&workdir, "https://github.com/acme/agent-team-mail.git");
    write_repo_gh_monitor_config_with_owner(&workdir, "test-team", "config-owner", "config-repo");
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "arch-ctm")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("monitor")
        .arg("run")
        .arg("42")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let _json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let request: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&request_log).unwrap()).unwrap();
    assert_eq!(request["command"].as_str(), Some("gh-monitor"));
    assert_eq!(
        request["payload"]["repo"].as_str(),
        Some("acme/agent-team-mail")
    );
    assert_eq!(
        request["payload"]["caller_agent"].as_str(),
        Some("arch-ctm")
    );
    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
#[cfg(unix)]
#[serial]
fn test_gh_monitor_repo_override_accepts_github_url_and_cc() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let request_log = temp_dir.path().join("request-log.json");
    // Pass the log path directly to the daemon rather than via process env so
    // that concurrently-running non-serial tests cannot inherit ATM_FAKE_GH_REQUEST_LOG
    // and accidentally overwrite this test's log with their own daemon requests.
    let mut daemon = start_fake_gh_daemon_with_request_log(temp_dir.path(), &request_log);
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).unwrap();
    write_repo_gh_monitor_config_with_owner(&workdir, "test-team", "config-owner", "config-repo");
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "arch-ctm")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--repo")
        .arg("https://github.com/example/other-repo.git")
        .arg("--cc")
        .arg("qa-bot")
        .arg("--cc")
        .arg("obs@ops")
        .arg("--json")
        .arg("monitor")
        .arg("run")
        .arg("42")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let _json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let request: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&request_log).unwrap()).unwrap();
    assert_eq!(
        request["payload"]["repo"].as_str(),
        Some("example/other-repo")
    );
    assert_eq!(
        request["payload"]["caller_agent"].as_str(),
        Some("arch-ctm")
    );
    assert_eq!(
        request["payload"]["cc"],
        serde_json::json!(["qa-bot", "obs@ops"])
    );
    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
#[cfg(unix)]
#[serial]
fn test_gh_monitor_requires_repo_context_when_not_in_git_repo_and_no_override() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let mut daemon = start_fake_gh_daemon(temp_dir.path());

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", true);
    cmd.env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("monitor")
        .arg("run")
        .arg("42")
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "Could not determine GitHub repository from current directory",
        ))
        .stderr(predicates::str::contains("--repo <owner/repo>"));
    let _ = daemon.kill();
    let _ = daemon.wait();
}

#[test]
#[cfg(unix)]
#[serial]
fn test_gh_namespace_status_missing_repo_is_actionable() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    write_repo_gh_monitor_config_missing_repo(&temp_dir.path().join("workdir"), "test-team");
    let mut daemon = start_fake_gh_daemon_with_mode(temp_dir.path(), true, true);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
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
    let status: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        status["availability_state"].as_str(),
        Some("disabled_config_error")
    );
    let message = status["message"].as_str().unwrap_or_default();
    assert!(message.contains("missing required field: repo"));
    assert!(message.contains("atm gh init"));

    let _ = daemon.kill();
    let _ = daemon.wait();
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
#[serial]
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

#[test]
#[cfg(unix)]
fn test_gh_monitor_namespace_rejects_removed_one_shot_commands() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");

    for removed in ["list", "report", "init-report"] {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir, "test-team", true);
        cmd.env("ATM_TEAM", "test-team")
            .arg("gh")
            .arg("monitor")
            .arg(removed)
            .assert()
            .failure()
            .stderr(
                predicates::str::contains("unrecognized subcommand")
                    .and(predicates::str::contains(removed)),
            );
    }
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_list_json_reports_rollups_without_daemon() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    write_repo_gh_monitor_config_with_owner(&workdir, "test-team", "acme", "agent-team-mail");
    let gh_bin = write_fake_gh_cli_script(temp_dir.path(), "acme/agent-team-mail");
    let path = format!(
        "{}:{}",
        gh_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .env("PATH", path)
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("pr")
        .arg("list")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["team"].as_str(), Some("test-team"));
    assert_eq!(json["repo"].as_str(), Some("acme/agent-team-mail"));
    assert_eq!(json["total_open_prs"].as_u64(), Some(2));
    let items = json["items"].as_array().unwrap();
    assert_eq!(items[0]["number"].as_u64(), Some(101));
    assert_eq!(items[0]["ci"]["state"].as_str(), Some("pending"));
    assert_eq!(items[0]["merge"].as_str(), Some("clean"));
    assert_eq!(items[0]["review"].as_str(), Some("approved"));
    assert_eq!(items[1]["number"].as_u64(), Some(102));
    assert_eq!(items[1]["ci"]["state"].as_str(), Some("fail"));
    assert_eq!(items[1]["draft"].as_bool(), Some(true));
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_list_human_output_has_one_line_rollups() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    write_repo_gh_monitor_config_with_owner(&workdir, "test-team", "acme", "agent-team-mail");
    let gh_bin = write_fake_gh_cli_script(temp_dir.path(), "acme/agent-team-mail");
    let path = format!(
        "{}:{}",
        gh_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .env("PATH", path)
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("pr")
        .arg("list")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("GitHub PR List: atm gh pr list"));
    assert!(text.contains("#101 [ready] [ci:PENDING 1/2] [merge:clean] [review:approved]"));
    assert!(text.contains(
        "#102 [draft] [ci:BLOCKED — merge conflict] [merge:CONFLICT ⚠] [review:changes_requested]"
    ));
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_report_json_includes_checks_reviews_and_merge_fields() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    write_repo_gh_monitor_config_with_owner(&workdir, "test-team", "acme", "agent-team-mail");
    let gh_bin = write_fake_gh_cli_script(temp_dir.path(), "acme/agent-team-mail");
    let path = format!(
        "{}:{}",
        gh_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .env("PATH", path)
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("pr")
        .arg("report")
        .arg("101")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["schema_version"].as_str(), Some("1.0.0"));
    assert_eq!(json["team"].as_str(), Some("test-team"));
    assert_eq!(json["repo"].as_str(), Some("acme/agent-team-mail"));
    assert_eq!(json["pr"]["number"].as_u64(), Some(101));
    assert_eq!(json["pr"]["ci"]["state"].as_str(), Some("pending"));
    assert_eq!(json["pr"]["merge"]["mergeable"].as_str(), Some("unknown"));
    assert_eq!(json["pr"]["merge"]["status"].as_str(), Some("blocked"));
    assert!(
        json["pr"]["merge"]["blocking_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str() == Some("CI checks still pending"))
    );
    assert!(
        json["pr"]["merge"]["advisory_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str() == Some("mergeability is UNKNOWN (transient)"))
    );
    assert!(json["pr"]["checks"].is_array());
    assert_eq!(json["pr"]["checks"].as_array().unwrap().len(), 2);
    assert!(json["pr"]["reviews"].is_array());
    assert_eq!(json["pr"]["reviews"].as_array().unwrap().len(), 2);
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_report_human_output_is_detailed() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    write_repo_gh_monitor_config_with_owner(&workdir, "test-team", "acme", "agent-team-mail");
    let gh_bin = write_fake_gh_cli_script(temp_dir.path(), "acme/agent-team-mail");
    let path = format!(
        "{}:{}",
        gh_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .env("PATH", path)
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("pr")
        .arg("report")
        .arg("101")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("GitHub PR Report: atm gh pr report"));
    assert!(text.contains("Schema Version:    1.0.0"));
    assert!(text.contains("PR:                #101"));
    assert!(text.contains("Merge:             status=blocked"));
    assert!(text.contains("Blocking Reasons:"));
    assert!(text.contains("CI checks still pending"));
    assert!(text.contains("Advisory Reasons:"));
    assert!(text.contains("mergeability is UNKNOWN (transient)"));
    assert!(text.contains("Reviews (2):"));
    assert!(text.contains("Checks (2):"));
    assert!(text.contains("clippy | status=completed"));
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_report_json_no_reviews_and_skips_are_non_blocking() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    write_repo_gh_monitor_config_with_owner(&workdir, "test-team", "acme", "agent-team-mail");
    let gh_bin = write_fake_gh_cli_script(temp_dir.path(), "acme/agent-team-mail");
    let path = format!(
        "{}:{}",
        gh_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .env("PATH", path)
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .arg("pr")
        .arg("report")
        .arg("103")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["pr"]["number"].as_u64(), Some(103));
    assert_eq!(json["pr"]["ci"]["state"].as_str(), Some("pass"));
    assert_eq!(json["pr"]["ci"]["pass"].as_u64(), Some(1));
    assert_eq!(json["pr"]["ci"]["skip"].as_u64(), Some(1));
    assert_eq!(json["pr"]["review_decision"].as_str(), Some("none"));
    assert_eq!(
        json["pr"]["merge"]["status"].as_str(),
        Some("indeterminate")
    );
    assert!(
        json["pr"]["merge"]["blocking_reasons"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        json["pr"]["merge"]["advisory_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str() == Some("no explicit review decision"))
    );
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_report_template_renders_custom_output() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    write_repo_gh_monitor_config_with_owner(&workdir, "test-team", "acme", "agent-team-mail");
    let gh_bin = write_fake_gh_cli_script(temp_dir.path(), "acme/agent-team-mail");
    let path = format!(
        "{}:{}",
        gh_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let template_path = workdir.join("custom-report.j2");
    fs::write(
        &template_path,
        "schema={{ schema_version }} pr={{ pr.number }} title={{ pr.title }}",
    )
    .unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .env("PATH", path)
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("pr")
        .arg("report")
        .arg("101")
        .arg("--template")
        .arg(&template_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("schema=1.0.0"));
    assert!(text.contains("pr=101"));
    assert!(text.contains("title=Add monitor dashboard"));
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_report_template_missing_file_is_actionable() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    write_repo_gh_monitor_config_with_owner(&workdir, "test-team", "acme", "agent-team-mail");
    let gh_bin = write_fake_gh_cli_script(temp_dir.path(), "acme/agent-team-mail");
    let path = format!(
        "{}:{}",
        gh_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let missing = workdir.join("missing-template.j2");
    let missing_display = missing.display().to_string();
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    cmd.env("ATM_TEAM", "test-team")
        .env("PATH", path)
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("pr")
        .arg("report")
        .arg("101")
        .arg("--template")
        .arg(&missing)
        .assert()
        .failure()
        .stderr(
            predicates::str::contains("failed to read template file")
                .and(predicates::str::contains(missing_display)),
        );
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_init_report_writes_starter_template() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    write_repo_gh_monitor_config_with_owner(&workdir, "test-team", "acme", "agent-team-mail");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    cmd.env("ATM_TEAM", "test-team")
        .arg("gh")
        .arg("pr")
        .arg("init-report")
        .assert()
        .success()
        .stdout(predicates::str::contains("atm gh pr init-report complete"));

    let template_path = workdir.join("gh-monitor-report-template.j2");
    let template = fs::read_to_string(&template_path).unwrap();
    assert!(template.contains("schema {{ schema_version }}"));
    assert!(template.contains("{{ pr.number }}"));
}

#[test]
#[cfg(unix)]
fn test_gh_monitor_report_template_render_failure_is_actionable() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    write_repo_gh_monitor_config_with_owner(&workdir, "test-team", "acme", "agent-team-mail");
    let gh_bin = write_fake_gh_cli_script(temp_dir.path(), "acme/agent-team-mail");
    let path = format!(
        "{}:{}",
        gh_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let template_path = workdir.join("invalid-template.j2");
    fs::write(&template_path, "{{ unclosed").unwrap();
    let template_display = template_path.display().to_string();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir, "test-team", false);
    cmd.env("ATM_TEAM", "test-team")
        .env("PATH", path)
        .arg("gh")
        .arg("--team")
        .arg("test-team")
        .arg("pr")
        .arg("report")
        .arg("101")
        .arg("--template")
        .arg(&template_path)
        .assert()
        .failure()
        .stderr(
            predicates::str::contains("failed to render template")
                .and(predicates::str::contains(template_display)),
        );
}
