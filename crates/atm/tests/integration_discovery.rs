//! Integration tests for discovery commands (teams, members, status, config)

use assert_cmd::cargo;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Helper to set home directory env vars for cross-platform test compatibility
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    cmd.env("HOME", temp_dir.path())
        .env("USERPROFILE", temp_dir.path());
}

/// Create a test team structure
fn setup_test_team(temp_dir: &TempDir, team_name: &str) -> PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");

    fs::create_dir_all(&inboxes_dir).unwrap();

    // Create team config.json
    let config = serde_json::json!({
        "name": team_name,
        "description": format!("Test team: {}", team_name),
        "createdAt": 1739284800000i64,
        "leadAgentId": format!("team-lead@{}", team_name),
        "leadSessionId": "test-session-id",
        "members": [
            {
                "agentId": format!("team-lead@{}", team_name),
                "name": "team-lead",
                "agentType": "general-purpose",
                "model": "claude-haiku-4-5-20251001",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": []
            },
            {
                "agentId": format!("agent-1@{}", team_name),
                "name": "agent-1",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "prompt": "Test agent 1",
                "color": "blue",
                "planModeRequired": false,
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%1",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "backendType": "tmux",
                "isActive": true
            },
            {
                "agentId": format!("agent-2@{}", team_name),
                "name": "agent-2",
                "agentType": "general-purpose",
                "model": "claude-sonnet-4-5-20250929",
                "prompt": "Test agent 2",
                "color": "green",
                "planModeRequired": false,
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%2",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "backendType": "tmux",
                "isActive": false
            }
        ]
    });

    let config_path = team_dir.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    team_dir
}

/// Create inbox files with messages
fn create_inbox_with_messages(team_dir: &Path, agent_name: &str, unread_count: usize) {
    let inbox_path = team_dir.join("inboxes").join(format!("{agent_name}.json"));

    let messages: Vec<serde_json::Value> = (0..unread_count)
        .map(|i| {
            serde_json::json!({
                "from": "human",
                "text": format!("Test message {}", i),
                "timestamp": "2026-02-11T12:00:00Z",
                "read": false,
                "summary": format!("Test message {}", i),
            })
        })
        .collect();

    fs::write(&inbox_path, serde_json::to_string_pretty(&messages).unwrap()).unwrap();
}

#[test]
fn test_teams_command_with_multiple_teams() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "team-alpha");
    setup_test_team(&temp_dir, "team-beta");
    setup_test_team(&temp_dir, "team-gamma");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("teams").assert().success();
}

#[test]
fn test_teams_command_no_teams() {
    let temp_dir = TempDir::new().unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("teams").assert().success();
}

#[test]
fn test_teams_command_json_output() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("teams")
        .arg("--json")
        .assert()
        .success();
}

#[test]
fn test_members_command_default_team() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "default-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "default-team")
        .arg("members")
        .assert()
        .success();
}

#[test]
fn test_members_command_explicit_team() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "explicit-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("members")
        .arg("explicit-team")
        .assert()
        .success();
}

#[test]
fn test_members_command_team_not_found() {
    let temp_dir = TempDir::new().unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "nonexistent-team")
        .arg("members")
        .assert()
        .failure();
}

#[test]
fn test_members_command_json_output() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("members")
        .arg("--json")
        .assert()
        .success();
}

#[test]
fn test_status_command_default_team() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "default-team");
    create_inbox_with_messages(&team_dir, "agent-1", 3);
    create_inbox_with_messages(&team_dir, "agent-2", 1);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "default-team")
        .arg("status")
        .assert()
        .success();
}

#[test]
fn test_status_command_explicit_team() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "explicit-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("status")
        .arg("explicit-team")
        .assert()
        .success();
}

#[test]
fn test_status_command_team_not_found() {
    let temp_dir = TempDir::new().unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "nonexistent-team")
        .arg("status")
        .assert()
        .failure();
}

#[test]
fn test_status_command_json_output() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("status")
        .arg("--json")
        .assert()
        .success();
}

#[test]
fn test_config_command_default() {
    let temp_dir = TempDir::new().unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("config").assert().success();
}

#[test]
fn test_config_command_json_output() {
    let temp_dir = TempDir::new().unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("config")
        .arg("--json")
        .assert()
        .success();
}

#[test]
fn test_empty_team_members() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = temp_dir.path().join(".claude/teams/empty-team");
    fs::create_dir_all(&team_dir).unwrap();

    // Create config with no members
    let config = serde_json::json!({
        "name": "empty-team",
        "createdAt": 1739284800000i64,
        "leadAgentId": "team-lead@empty-team",
        "leadSessionId": "test-session-id",
        "members": []
    });

    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap()
    ).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "empty-team")
        .arg("members")
        .assert()
        .success();
}
