use assert_cmd::cargo;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env_remove("ATM_CONFIG")
        .env_remove("CLAUDE_SESSION_ID")
        .current_dir(&workdir);
}

fn setup_team(temp_dir: &TempDir, team_name: &str) -> PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    let config = serde_json::json!({
        "name": team_name,
        "description": "Test team",
        "createdAt": 1739284800000i64,
        "leadAgentId": format!("team-lead@{team_name}"),
        "leadSessionId": "test-session-id",
        "members": [
            {
                "agentId": format!("team-lead@{team_name}"),
                "name": "team-lead",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1739284800000i64,
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

    team_dir
}

fn has_member(team_dir: &Path, name: &str) -> bool {
    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(team_dir.join("config.json")).unwrap()).unwrap();
    config["members"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["name"].as_str() == Some(name))
}

#[test]
fn test_teams_join_help_surface() {
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.args(["teams", "join", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("<AGENT>"))
        .stdout(predicate::str::contains("--team"))
        .stdout(predicate::str::contains("--agent-type"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--folder"))
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("team-lead-initiated"));
}

#[test]
fn test_teams_join_team_lead_initiated_json_contract() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_team(&temp_dir, "atm-dev");
    let folder = temp_dir.path().join("repo");
    fs::create_dir_all(&folder).unwrap();
    let canonical_folder = fs::canonicalize(&folder).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env("ATM_TEAM", "atm-dev")
        .env("ATM_IDENTITY", "team-lead")
        .args([
            "teams",
            "join",
            "arch-ctm",
            "--folder",
            folder.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success();

    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();

    assert_eq!(parsed["team"], "atm-dev");
    assert_eq!(parsed["agent"], "arch-ctm");
    assert_eq!(parsed["mode"], "team_lead_initiated");
    assert_eq!(
        parsed["folder"],
        canonical_folder.to_string_lossy().to_string()
    );
    assert!(
        parsed["launch_command"]
            .as_str()
            .unwrap()
            .contains("claude --resume")
    );

    assert!(has_member(&team_dir, "arch-ctm"));
}

#[test]
fn test_teams_join_rejects_team_mismatch_in_lead_mode() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "atm-dev");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env("ATM_TEAM", "atm-dev")
        .env("ATM_IDENTITY", "team-lead")
        .args(["teams", "join", "arch-ctm", "--team", "other-team"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not match your current team"));
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "stdout must be empty on mismatch error, got: {stdout:?}"
    );
}

#[test]
fn test_teams_join_requires_team_in_self_join_mode() {
    let temp_dir = TempDir::new().unwrap();
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);

    cmd.env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .args(["teams", "join", "new-agent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--team is required when caller has no current team context",
        ));
}

#[test]
fn test_teams_join_self_join_success_with_explicit_team() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_team(&temp_dir, "atm-dev");
    let folder = temp_dir.path().join("repo-self");
    fs::create_dir_all(&folder).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .args([
            "teams",
            "join",
            "self-join-agent",
            "--team",
            "atm-dev",
            "--folder",
            folder.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success();

    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(parsed["team"], "atm-dev");
    assert_eq!(parsed["agent"], "self-join-agent");
    assert_eq!(parsed["mode"], "self_join");
    assert!(has_member(&team_dir, "self-join-agent"));
}

#[test]
fn test_teams_join_human_output_contains_folder_and_launch_command() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "atm-dev");
    let folder = temp_dir.path().join("repo2");
    fs::create_dir_all(&folder).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "atm-dev")
        .env("ATM_IDENTITY", "team-lead")
        .args([
            "teams",
            "join",
            "quality-mgr",
            "--folder",
            folder.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Mode: team_lead_initiated"))
        .stdout(predicate::str::contains("Folder:"))
        .stdout(predicate::str::contains("Launch command:"))
        .stdout(predicate::str::contains("claude --resume"));
}
