use assert_cmd::cargo;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn setup_team(home: &Path, team: &str) {
    let team_dir = home.join(".claude/teams").join(team);
    fs::create_dir_all(team_dir.join("inboxes")).expect("create team dirs");
    fs::write(
        team_dir.join("config.json"),
        serde_json::json!({
            "name": team,
            "description": "test team",
            "createdAt": 1739284800000u64,
            "leadAgentId": format!("team-lead@{team}"),
            "leadSessionId": "sess-team-lead",
            "members": [
                {
                    "agentId": format!("team-lead@{team}"),
                    "name": "team-lead",
                    "agentType": "team-lead",
                    "model": "claude-sonnet-4-6",
                    "joinedAt": 1739284800000u64,
                    "cwd": ".",
                    "subscriptions": []
                }
            ]
        })
        .to_string(),
    )
    .expect("write team config");
}

fn setup_daemon_status(home: &Path) {
    let daemon_dir = home.join(".claude/daemon");
    fs::create_dir_all(&daemon_dir).expect("create daemon dir");
    let tmp = std::env::temp_dir();
    let spool_path = tmp.join("log-spool").to_string_lossy().into_owned();
    let log_path = tmp.join("atm.log.jsonl").to_string_lossy().into_owned();
    fs::write(
        daemon_dir.join("status.json"),
        serde_json::json!({
            "timestamp": "2026-03-09T00:00:00Z",
            "pid": 4242,
            "version": "0.42.1",
            "uptime_secs": 1,
            "plugins": [],
            "teams": ["atm-dev"],
            "logging": {
                "state": "degraded_spooling",
                "dropped_counter": 2,
                "spool_path": spool_path,
                "last_error": "spool backlog",
                "canonical_log_path": log_path,
                "spool_count": 3,
                "oldest_spool_age": 15
            }
        })
        .to_string(),
    )
    .expect("write daemon status");
}

#[test]
fn status_json_includes_extended_logging_fields() {
    let temp_dir = TempDir::new().expect("temp dir");
    setup_team(temp_dir.path(), "atm-dev");
    setup_daemon_status(temp_dir.path());

    let output = Command::new(cargo::cargo_bin!("atm"))
        .env("ATM_HOME", temp_dir.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .arg("status")
        .arg("--team")
        .arg("atm-dev")
        .arg("--json")
        .output()
        .expect("run status");

    assert!(
        output.status.success(),
        "status failed: {:?}",
        output.status
    );
    let body = String::from_utf8(output.stdout).expect("utf8 stdout");
    let value: Value = serde_json::from_str(&body).expect("status json");
    let logging = &value["logging"];
    assert_eq!(logging["state"], "degraded_spooling");
    assert!(logging["canonical_log_path"].is_string());
    assert!(logging["spool_count"].is_u64());
    assert!(logging["oldest_spool_age"].is_u64());
}

#[test]
fn doctor_json_includes_extended_logging_fields() {
    let temp_dir = TempDir::new().expect("temp dir");
    setup_team(temp_dir.path(), "atm-dev");
    setup_daemon_status(temp_dir.path());

    let output = Command::new(cargo::cargo_bin!("atm"))
        .env("ATM_HOME", temp_dir.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .arg("doctor")
        .arg("--team")
        .arg("atm-dev")
        .arg("--json")
        .output()
        .expect("run doctor");

    assert!(
        output.status.success() || output.status.code() == Some(2),
        "doctor failed with unexpected code: {:?}",
        output.status.code()
    );

    let body = String::from_utf8(output.stdout).expect("utf8 stdout");
    let value: Value = serde_json::from_str(&body).expect("doctor json");
    let logging = &value["logging"];
    assert!(logging["state"].is_string());
    assert!(logging["canonical_log_path"].is_string());
    assert!(logging["spool_count"].is_u64());
    assert!(logging["oldest_spool_age"].is_number() || logging["oldest_spool_age"].is_null());
}
