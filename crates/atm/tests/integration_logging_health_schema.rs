use assert_cmd::cargo;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn runtime_home(temp_dir: &TempDir) -> std::path::PathBuf {
    let runtime_home = temp_dir.path().join("runtime-home");
    fs::create_dir_all(&runtime_home).expect("create runtime home");
    runtime_home
}

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
    let daemon_dir = home.join(".atm/daemon");
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
            },
            "otel": {
                "schema_version": "v1",
                "enabled": true,
                "collector_endpoint": "http://collector:4318",
                "protocol": "otlp_http",
                "collector_state": "degraded",
                "local_mirror_state": "healthy",
                "local_mirror_path": tmp.join("atm.log.otel.jsonl").to_string_lossy(),
                "debug_local_export": false,
                "debug_local_state": "disabled",
                "last_error": {
                    "code": "COLLECTOR_EXPORT_FAILED",
                    "message": "collector timeout",
                    "at": "2026-03-18T00:00:00Z"
                }
            }
        })
        .to_string(),
    )
    .expect("write daemon status");
}

fn assert_canonical_otel_health(otel_health: &Value) {
    assert_eq!(otel_health["schema_version"], "v1");
    assert!(otel_health["enabled"].is_boolean());
    assert!(
        otel_health["collector_endpoint"].is_string()
            || otel_health["collector_endpoint"].is_null()
    );
    assert!(otel_health["protocol"].is_string());
    assert!(otel_health["collector_state"].is_string());
    assert!(otel_health["local_mirror_state"].is_string());
    assert!(otel_health["local_mirror_path"].is_string());
    assert!(otel_health["debug_local_export"].is_boolean());
    assert!(otel_health["debug_local_state"].is_string());
    assert!(otel_health["last_error"].is_object());
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
    let runtime_home = runtime_home(&temp_dir);
    setup_team(temp_dir.path(), "atm-dev");
    setup_daemon_status(&runtime_home);

    let output = Command::new(cargo::cargo_bin!("atm"))
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", temp_dir.path())
        .envs([("HOME", temp_dir.path())])
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
    assert!(
        value.get("logging").is_none(),
        "legacy logging key must not be emitted"
    );
    let logging_health = &value["logging_health"];
    assert_canonical_logging_health(logging_health);
    let otel_health = &value["otel_health"];
    assert_canonical_otel_health(otel_health);
    assert_eq!(logging_health["state"], "degraded_spooling");
    assert_eq!(logging_health["spool_file_count"], 3);
    assert_eq!(logging_health["dropped_events_total"], 2);
    assert_eq!(logging_health["last_error"]["code"], "DEGRADED_SPOOLING");
    assert_eq!(otel_health["collector_state"], "degraded");
    assert_eq!(otel_health["last_error"]["code"], "COLLECTOR_EXPORT_FAILED");
}

#[test]
fn doctor_json_includes_extended_logging_fields() {
    let temp_dir = TempDir::new().expect("temp dir");
    let runtime_home = runtime_home(&temp_dir);
    setup_team(temp_dir.path(), "atm-dev");
    setup_daemon_status(&runtime_home);

    let output = Command::new(cargo::cargo_bin!("atm"))
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", temp_dir.path())
        .envs([("HOME", temp_dir.path())])
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
    assert!(
        value.get("logging").is_none(),
        "legacy logging key must not be emitted"
    );
    let logging_health = &value["logging_health"];
    assert_canonical_logging_health(logging_health);
    assert_canonical_otel_health(&value["otel_health"]);
}

#[test]
fn daemon_status_json_includes_extended_logging_fields() {
    let temp_dir = TempDir::new().expect("temp dir");
    let runtime_home = runtime_home(&temp_dir);
    setup_daemon_status(&runtime_home);

    let output = Command::new(cargo::cargo_bin!("atm"))
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", temp_dir.path())
        .envs([("HOME", temp_dir.path())])
        .env("ATM_DAEMON_AUTOSTART", "0")
        .arg("daemon")
        .arg("status")
        .arg("--json")
        .output()
        .expect("run daemon status");

    assert!(
        output.status.success() || output.status.code() == Some(1),
        "daemon status failed with unexpected code: {:?}",
        output.status.code()
    );

    let body = String::from_utf8(output.stdout).expect("utf8 stdout");
    let value: Value = serde_json::from_str(&body).expect("daemon status json");
    assert!(
        value.get("logging").is_none(),
        "legacy logging key must not be emitted"
    );
    let logging_health = &value["logging_health"];
    assert_canonical_logging_health(logging_health);
    let otel_health = &value["otel_health"];
    assert_canonical_otel_health(otel_health);
    assert_eq!(logging_health["state"], "degraded_spooling");
    assert_eq!(logging_health["spool_file_count"], 3);
    assert_eq!(logging_health["dropped_events_total"], 2);
    assert_eq!(logging_health["last_error"]["code"], "DEGRADED_SPOOLING");
    assert_eq!(otel_health["collector_state"], "degraded");
}

#[test]
fn doctor_and_status_logging_health_schema_parity() {
    let temp_dir = TempDir::new().expect("temp dir");
    let runtime_home = runtime_home(&temp_dir);
    setup_team(temp_dir.path(), "atm-dev");
    setup_daemon_status(&runtime_home);

    let status_output = Command::new(cargo::cargo_bin!("atm"))
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", temp_dir.path())
        .envs([("HOME", temp_dir.path())])
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
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", temp_dir.path())
        .envs([("HOME", temp_dir.path())])
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

    let daemon_output = Command::new(cargo::cargo_bin!("atm"))
        .env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", temp_dir.path())
        .envs([("HOME", temp_dir.path())])
        .env("ATM_DAEMON_AUTOSTART", "0")
        .arg("daemon")
        .arg("status")
        .arg("--json")
        .output()
        .expect("run daemon status");
    assert!(
        daemon_output.status.success() || daemon_output.status.code() == Some(1),
        "daemon status failed with unexpected code: {:?}",
        daemon_output.status.code()
    );
    let daemon_value: Value =
        serde_json::from_slice(&daemon_output.stdout).expect("daemon status output JSON");

    let status_health = &status_value["logging_health"];
    let doctor_health = &doctor_value["logging_health"];
    let daemon_health = &daemon_value["logging_health"];
    assert_canonical_logging_health(status_health);
    assert_canonical_logging_health(doctor_health);
    assert_canonical_logging_health(daemon_health);
    let status_otel = &status_value["otel_health"];
    let doctor_otel = &doctor_value["otel_health"];
    let daemon_otel = &daemon_value["otel_health"];
    assert_canonical_otel_health(status_otel);
    assert_canonical_otel_health(doctor_otel);
    assert_canonical_otel_health(daemon_otel);

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
    let daemon_keys = daemon_health
        .as_object()
        .expect("daemon logging_health object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        status_keys, doctor_keys,
        "doctor/status logging_health keys must be identical"
    );
    assert_eq!(
        status_keys, daemon_keys,
        "daemon/status logging_health keys must be identical"
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
    let daemon_last_error_keys = daemon_health["last_error"]
        .as_object()
        .expect("daemon last_error object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        status_last_error_keys, doctor_last_error_keys,
        "doctor/status logging_health.last_error keys must be identical"
    );
    assert_eq!(
        status_last_error_keys, daemon_last_error_keys,
        "daemon/status logging_health.last_error keys must be identical"
    );

    let status_otel_keys = status_otel
        .as_object()
        .expect("status otel_health object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let doctor_otel_keys = doctor_otel
        .as_object()
        .expect("doctor otel_health object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let daemon_otel_keys = daemon_otel
        .as_object()
        .expect("daemon otel_health object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(status_otel_keys, doctor_otel_keys);
    assert_eq!(status_otel_keys, daemon_otel_keys);
}
