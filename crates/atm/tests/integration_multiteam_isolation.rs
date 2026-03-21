//! Multi-team CLI isolation and daemon restart state preservation tests.

#![cfg(unix)]

use agent_team_mail_core::daemon_client::{RegisterHintOutcome, register_hint};
use assert_cmd::cargo;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tempfile::TempDir;
#[path = "support/daemon_process_guard.rs"]
mod daemon_process_guard;
#[path = "support/daemon_test_registry.rs"]
mod daemon_test_registry;
#[path = "support/env_guard.rs"]
mod env_guard;
use daemon_process_guard::DaemonProcessGuard;
use env_guard::EnvGuard;

fn register_hint_with_retry(
    team: &str,
    agent: &str,
    session_id: &str,
    process_id: u32,
) -> anyhow::Result<RegisterHintOutcome> {
    let mut last_err = None;
    for _ in 0..20 {
        match register_hint(
            team,
            agent,
            session_id,
            process_id,
            Some("codex"),
            None,
            None,
            None,
        ) {
            Ok(outcome) => return Ok(outcome),
            Err(err) if err.to_string().contains("AGENT_NOT_FOUND") => {
                last_err = Some(err);
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => return Err(err),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("register-hint retry budget exhausted")))
}

/// Helper to set home directory for cross-platform test compatibility.
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).ok();
    cmd.env(
        "ATM_HOME",
        daemon_process_guard::DaemonProcessGuard::runtime_home_path(temp_dir),
    )
    .envs([("HOME", temp_dir.path())])
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

    let _home = EnvGuard::set("HOME", temp_dir.path());
    let mut daemon = DaemonProcessGuard::spawn(&temp_dir, team_a);
    daemon.wait_ready(&temp_dir);

    let _atm_home = EnvGuard::set(
        "ATM_HOME",
        daemon_process_guard::DaemonProcessGuard::runtime_home_path(&temp_dir),
    );
    let _identity_alpha = EnvGuard::set("ATM_IDENTITY", alpha_member);
    let hint_alpha =
        register_hint_with_retry(team_a, alpha_member, "sess-alpha-1", std::process::id())
            .expect("register-hint for alpha member");
    assert_eq!(hint_alpha, RegisterHintOutcome::Registered);
    drop(_identity_alpha);

    let _identity_beta = EnvGuard::set("ATM_IDENTITY", beta_member);
    let hint_beta =
        register_hint_with_retry(team_b, beta_member, "sess-beta-1", std::process::id())
            .expect("register-hint for beta member");
    assert_eq!(hint_beta, RegisterHintOutcome::Registered);

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

    let _home = EnvGuard::set("HOME", temp_dir.path());
    let mut daemon = DaemonProcessGuard::spawn(&temp_dir, team);
    daemon.wait_ready(&temp_dir);

    let _atm_home = EnvGuard::set(
        "ATM_HOME",
        daemon_process_guard::DaemonProcessGuard::runtime_home_path(&temp_dir),
    );
    let _identity = EnvGuard::set("ATM_IDENTITY", member);
    let outcome = register_hint(
        team,
        member,
        "persisted-session-1",
        std::process::id(),
        Some("codex"),
        None,
        None,
        None,
    )
    .expect("register-hint before restart");
    assert_eq!(outcome, RegisterHintOutcome::Registered);

    let liveness_before = wait_for_member_liveness(&temp_dir, team, member, Duration::from_secs(2));
    assert_eq!(
        liveness_before.get("members"),
        Some(&Some(true)),
        "members should report persisted member as Online before restart"
    );
    assert_eq!(
        liveness_before.get("status"),
        Some(&Some(true)),
        "status should report persisted member as Online before restart"
    );

    drop(daemon);

    let mut daemon_restarted = DaemonProcessGuard::spawn(&temp_dir, team);
    daemon_restarted.wait_ready(&temp_dir);

    let liveness_after = wait_for_member_liveness(&temp_dir, team, member, Duration::from_secs(2));
    assert_eq!(
        liveness_after.get("members"),
        Some(&Some(true)),
        "members should preserve Online state after daemon restart"
    );
    assert_eq!(
        liveness_after.get("status"),
        Some(&Some(true)),
        "status should preserve Online state after daemon restart"
    );
}

fn wait_for_member_liveness(
    temp_dir: &TempDir,
    team: &str,
    member: &str,
    timeout: Duration,
) -> HashMap<&'static str, Option<bool>> {
    let deadline = Instant::now() + timeout;
    let mut latest = read_member_liveness(temp_dir, team, member);
    while Instant::now() < deadline {
        if latest.get("members") == Some(&Some(true)) && latest.get("status") == Some(&Some(true)) {
            return latest;
        }
        std::thread::sleep(Duration::from_millis(25));
        latest = read_member_liveness(temp_dir, team, member);
    }
    latest
}

fn read_member_liveness(
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
        .and_then(|rows| {
            rows.iter().find_map(|row| {
                (row.get("name").and_then(|v| v.as_str()) == Some(member))
                    .then(|| row.get("liveness").and_then(|v| v.as_bool()))
                    .flatten()
            })
        });
    map.insert("members", members_liveness);

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
        .and_then(|rows| {
            rows.iter().find_map(|row| {
                (row.get("name").and_then(|v| v.as_str()) == Some(member))
                    .then(|| row.get("liveness").and_then(|v| v.as_bool()))
                    .flatten()
            })
        });
    map.insert("status", status_liveness);

    map
}
