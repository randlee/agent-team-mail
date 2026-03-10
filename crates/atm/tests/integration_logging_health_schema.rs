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

fn assert_canonical_logging_health(logging_health: &Value) {
    assert_eq!(logging_health["schema_version"], "v1");
    assert!(logging_health["state"].is_string());
    assert!(logging_health["log_root"].is_string());
    assert!(logging_health["canonical_log_path"].is_string());
    assert!(logging_health["spool_path"].is_string());
    assert!(logging_health["dropped_events_total"].is_u64());
    assert!(logging_health["spool_file_count"].is_u64());
    assert!(
        logging_health["oldest_spool_age_seconds"].is_u64()
            || logging_health["oldest_spool_age_seconds"].is_null()
    );
    assert!(logging_health["last_error"].is_object());
    assert!(
        logging_health["last_error"]["code"].is_string()
            || logging_health["last_error"]["code"].is_null()
    );
    assert!(
        logging_health["last_error"]["message"].is_string()
            || logging_health["last_error"]["message"].is_null()
    );
    assert!(
        logging_health["last_error"]["at"].is_string()
            || logging_health["last_error"]["at"].is_null()
    );
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
    let logging_health = &value["logging_health"];
    assert_canonical_logging_health(logging_health);
    assert_eq!(logging_health["state"], "degraded_spooling");
    assert_eq!(logging_health["spool_file_count"], 3);
    assert_eq!(logging_health["dropped_events_total"], 2);
    assert_eq!(logging_health["last_error"]["code"], "DEGRADED_SPOOLING");
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
    let logging_health = &value["logging_health"];
    assert_canonical_logging_health(logging_health);
}

#[test]
fn doctor_and_status_logging_health_schema_parity() {
    let temp_dir = TempDir::new().expect("temp dir");
    setup_team(temp_dir.path(), "atm-dev");
    setup_daemon_status(temp_dir.path());

    let status_output = Command::new(cargo::cargo_bin!("atm"))
        .env("ATM_HOME", temp_dir.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .arg("status")
        .arg("--team")
        .arg("atm-dev")
        .arg("--json")
        .output()
        .expect("run status");
    assert!(status_output.status.success(), "status must succeed");
    let status_value: Value =
        serde_json::from_slice(&status_output.stdout).expect("status output JSON");

    let doctor_output = Command::new(cargo::cargo_bin!("atm"))
        .env("ATM_HOME", temp_dir.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .arg("doctor")
        .arg("--team")
        .arg("atm-dev")
        .arg("--json")
        .output()
        .expect("run doctor");
    assert!(
        doctor_output.status.success() || doctor_output.status.code() == Some(2),
        "doctor failed with unexpected code: {:?}",
        doctor_output.status.code()
    );
    let doctor_value: Value =
        serde_json::from_slice(&doctor_output.stdout).expect("doctor output JSON");

    let status_health = &status_value["logging_health"];
    let doctor_health = &doctor_value["logging_health"];
    assert_canonical_logging_health(status_health);
    assert_canonical_logging_health(doctor_health);

    let status_keys = status_health
        .as_object()
        .expect("status logging_health object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let doctor_keys = doctor_health
        .as_object()
        .expect("doctor logging_health object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        status_keys, doctor_keys,
        "doctor/status logging_health keys must be identical"
    );

    let status_last_error_keys = status_health["last_error"]
        .as_object()
        .expect("status last_error object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let doctor_last_error_keys = doctor_health["last_error"]
        .as_object()
        .expect("doctor last_error object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        status_last_error_keys, doctor_last_error_keys,
        "doctor/status logging_health.last_error keys must be identical"
    );
}
