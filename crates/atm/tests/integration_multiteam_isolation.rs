//! Multi-team CLI isolation and daemon restart state preservation tests.

#![cfg(unix)]

use assert_cmd::cargo;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;
#[path = "support/daemon_process_guard.rs"]
mod daemon_process_guard;
#[path = "support/daemon_test_registry.rs"]
mod daemon_test_registry;
#[path = "support/env_guard.rs"]
mod env_guard;
use daemon_process_guard::DaemonProcessGuard;
use env_guard::EnvGuard;

/// Helper to set home directory for cross-platform test compatibility.
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).ok();
    cmd.env(
        "ATM_HOME",
        daemon_process_guard::DaemonProcessGuard::runtime_home_path(temp_dir),
    )
    .env("ATM_CONFIG_HOME", temp_dir.path())
    .envs([("ATM_HOME", temp_dir.path())])
    .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
    .env("ATM_DAEMON_AUTOSTART", "0")
    .env_remove("ATM_TEAM")
    .env_remove("ATM_IDENTITY")
    .env_remove("ATM_CONFIG")
    .env_remove("CLAUDE_SESSION_ID")
    .current_dir(&workdir);
}

fn setup_test_team(
    temp_dir: &TempDir,
    team_name: &str,
    lead_name: &str,
    member_name: &str,
) -> PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    // Mark external backend so register-hint can use the test process PID
    // without backend mismatch enforcement.
    let config = serde_json::json!({
        "name": team_name,
        "description": "Integration team",
        "createdAt": 1739284800000i64,
        "leadAgentId": format!("{lead_name}@{team_name}"),
        "leadSessionId": format!("{team_name}-lead-session"),
        "members": [
            {
                "agentId": format!("{lead_name}@{team_name}"),
                "name": lead_name,
                "agentType": "general-purpose",
                "model": "claude-haiku-4-5-20251001",
                "joinedAt": 1739284800000i64,
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": false,
                "externalBackendType": "external"
            },
            {
                "agentId": format!("{member_name}@{team_name}"),
                "name": member_name,
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1739284800000i64,
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": true,
                "externalBackendType": "external"
            }
        ]
    });

    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    fs::write(inboxes_dir.join(format!("{lead_name}.json")), "[]").unwrap();
    fs::write(inboxes_dir.join(format!("{member_name}.json")), "[]").unwrap();
    team_dir
}

fn assert_member_presence_and_isolation(
    json: &Value,
    present_member: &str,
    absent_member: &str,
    field: &str,
) {
    let members = json
        .get("members")
        .and_then(|v| v.as_array())
        .expect("members should be an array");

    let names: Vec<String> = members
        .iter()
        .filter_map(|m| m.get("name").and_then(|v| v.as_str()))
        .map(ToString::to_string)
        .collect();

    assert!(
        names.iter().any(|name| name == present_member),
        "{field}: expected member '{present_member}' in {:?}",
        names
    );
    assert!(
        !names.iter().any(|name| name == absent_member),
        "{field}: foreign-team member '{absent_member}' leaked into {:?}",
        names
    );
}

#[test]
#[serial_test::serial]
fn test_cli_team_scoped_commands_do_not_bleed_members_across_teams() {
    let temp_dir = TempDir::new().unwrap();
    let team_a = "team-alpha";
    let team_b = "team-beta";
    let alpha_member = "alpha-only-member";
    let beta_member = "beta-only-member";
    setup_test_team(&temp_dir, team_a, "alpha-lead", alpha_member);
    setup_test_team(&temp_dir, team_b, "beta-lead", beta_member);

    let _config_home = EnvGuard::set("ATM_CONFIG_HOME", temp_dir.path());
    let _home = EnvGuard::set("HOME", temp_dir.path());
    let mut daemon = DaemonProcessGuard::spawn(&temp_dir, team_a);
    daemon.wait_ready(&temp_dir);

    let mut members_cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut members_cmd, &temp_dir);
    let members_json: Value = serde_json::from_slice(
        &members_cmd
            .arg("members")
            .arg("--team")
            .arg(team_a)
            .arg("--json")
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .unwrap();
    assert_member_presence_and_isolation(&members_json, alpha_member, beta_member, "members");

    let mut status_cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut status_cmd, &temp_dir);
    let status_json: Value = serde_json::from_slice(
        &status_cmd
            .arg("status")
            .arg("--team")
            .arg(team_a)
            .arg("--json")
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .unwrap();
    assert_member_presence_and_isolation(&status_json, alpha_member, beta_member, "status");

    let mut doctor_cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut doctor_cmd, &temp_dir);
    let doctor_output = doctor_cmd
        .arg("doctor")
        .arg("--team")
        .arg(team_a)
        .output()
        .expect("run doctor");
    let doctor_stdout = String::from_utf8(doctor_output.stdout).unwrap();
    assert!(
        doctor_stdout.contains(alpha_member),
        "doctor output should include team member '{}'",
        alpha_member
    );
    assert!(
        !doctor_stdout.contains(beta_member),
        "doctor output should not include foreign-team member '{}'",
        beta_member
    );
}

#[test]
#[serial_test::serial]
fn test_status_and_members_preserve_registered_member_state_after_daemon_restart() {
    let temp_dir = TempDir::new().unwrap();
    let team = "team-restart";
    let member = "persisted-member";
    setup_test_team(&temp_dir, team, "restart-lead", member);

    let _config_home = EnvGuard::set("ATM_CONFIG_HOME", temp_dir.path());
    let _home = EnvGuard::set("HOME", temp_dir.path());
    let mut daemon = DaemonProcessGuard::spawn(&temp_dir, team);
    daemon.wait_ready(&temp_dir);

    let liveness_before = read_member_visibility(&temp_dir, team, member);
    assert_eq!(
        liveness_before.get("members_present"),
        Some(&Some(true)),
        "members should continue to surface the configured member before restart"
    );
    assert_eq!(
        liveness_before.get("status_present"),
        Some(&Some(true)),
        "status should continue to surface the configured member before restart"
    );

    drop(daemon);

    let mut daemon_restarted = DaemonProcessGuard::spawn(&temp_dir, team);
    daemon_restarted.wait_ready(&temp_dir);

    let liveness_after =
        wait_for_member_visibility(&temp_dir, team, member, Duration::from_secs(2));
    assert_eq!(
        liveness_after.get("members_present"),
        Some(&Some(true)),
        "members should preserve configured member visibility after daemon restart"
    );
    assert_eq!(
        liveness_after.get("status_present"),
        Some(&Some(true)),
        "status should preserve configured member visibility after daemon restart"
    );
}

fn wait_for_member_visibility(
    temp_dir: &TempDir,
    team: &str,
    member: &str,
    timeout: Duration,
) -> HashMap<&'static str, Option<bool>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut latest = read_member_visibility(temp_dir, team, member);
    while std::time::Instant::now() < deadline {
        if latest.get("members_present") == Some(&Some(true))
            && latest.get("status_present") == Some(&Some(true))
        {
            return latest;
        }
        std::thread::sleep(Duration::from_millis(25));
        latest = read_member_visibility(temp_dir, team, member);
    }
    latest
}

fn read_member_visibility(
    temp_dir: &TempDir,
    team: &str,
    member: &str,
) -> HashMap<&'static str, Option<bool>> {
    let mut map = HashMap::new();

    let mut members_cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut members_cmd, temp_dir);
    let members_json: Value = serde_json::from_slice(
        &members_cmd
            .arg("members")
            .arg("--team")
            .arg(team)
            .arg("--json")
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .unwrap();
    let members_liveness = members_json
        .get("members")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .any(|row| row.get("name").and_then(|v| v.as_str()) == Some(member))
        });
    map.insert("members_present", members_liveness);

    let mut status_cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut status_cmd, temp_dir);
    let status_json: Value = serde_json::from_slice(
        &status_cmd
            .arg("status")
            .arg("--team")
            .arg(team)
            .arg("--json")
            .assert()
            .success()
            .get_output()
            .stdout,
    )
    .unwrap();
    let status_liveness = status_json
        .get("members")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .any(|row| row.get("name").and_then(|v| v.as_str()) == Some(member))
        });
    map.insert("status_present", status_liveness);

    map
}
