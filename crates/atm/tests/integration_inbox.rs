//! Integration tests for the inbox command

use assert_cmd::cargo;
use predicates::str::contains;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Helper to set home directory for cross-platform test compatibility.
/// Uses `ATM_HOME` which is checked first by `get_home_dir()`, avoiding
/// platform-specific differences in how `dirs::home_dir()` resolves.
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    // Use a subdirectory as CWD to avoid:
    // 1. .atm.toml config leak from the repo root
    // 2. auto-identity CWD matching against team member CWD (temp_dir root)
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).ok();
    cmd.env("ATM_HOME", temp_dir.path())
        .env_remove("ATM_IDENTITY")
        .current_dir(&workdir);
}

/// Create a test team structure
fn setup_test_team(temp_dir: &TempDir, team_name: &str) -> PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");

    fs::create_dir_all(&inboxes_dir).unwrap();

    // Create team config.json
    let config = serde_json::json!({
        "name": team_name,
        "description": "Test team",
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
                "agentId": format!("test-agent@{}", team_name),
                "name": "test-agent",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "prompt": "Test agent",
                "color": "blue",
                "planModeRequired": false,
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%1",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "backendType": "tmux",
                "isActive": true
            }
        ]
    });

    let config_path = team_dir.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    team_dir
}

/// Create a test inbox with messages
fn create_test_inbox(team_dir: &Path, agent_name: &str, messages: Vec<serde_json::Value>) {
    let inbox_path = team_dir.join("inboxes").join(format!("{agent_name}.json"));
    fs::write(&inbox_path, serde_json::to_string_pretty(&messages).unwrap()).unwrap();
}

#[test]
fn test_inbox_single_team() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("inbox")
        .arg("--no-since-last-seen")
        .assert()
        .success();
}

#[test]
fn test_inbox_shows_correct_counts() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Create inbox with mixed read/unread messages
    let messages = vec![
        serde_json::json!({
            "from": "team-lead",
            "text": "Unread message 1",
            "timestamp": "2026-02-11T10:00:00Z",
            "read": false,
            "message_id": "msg-001"
        }),
        serde_json::json!({
            "from": "team-lead",
            "text": "Read message",
            "timestamp": "2026-02-11T11:00:00Z",
            "read": true,
            "message_id": "msg-002"
        }),
        serde_json::json!({
            "from": "ci-agent",
            "text": "Unread message 2",
            "timestamp": "2026-02-11T12:00:00Z",
            "read": false,
            "message_id": "msg-003"
        }),
    ];
    create_test_inbox(&team_dir, "test-agent", messages);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("inbox")
        .arg("--no-since-last-seen")
        .assert()
        .success()
        .stdout(contains("Unread"));

    // When using since-last-seen (default), header should say "New"
    let mut cmd2 = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd2, &temp_dir);
    cmd2.env("ATM_TEAM", "test-team")
        .arg("inbox")
        .assert()
        .success()
        .stdout(contains("New"));
}

#[test]
fn test_inbox_no_messages() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // No inbox files created - should show 0/0 for all agents

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("inbox")
        .arg("--no-since-last-seen")
        .assert()
        .success();
}

#[test]
fn test_inbox_all_teams() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir1 = setup_test_team(&temp_dir, "team-a");
    let _team_dir2 = setup_test_team(&temp_dir, "team-b");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "team-a")
        .arg("inbox")
        .arg("--no-since-last-seen")
        .arg("--all-teams")
        .assert()
        .success();
}

#[test]
fn test_inbox_team_not_found() {
    let temp_dir = TempDir::new().unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "nonexistent-team")
        .arg("inbox")
        .arg("--no-since-last-seen")
        .assert()
        .failure();
}

#[test]
fn test_inbox_with_team_flag() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "override-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "default-team")
        .arg("inbox")
        .arg("--no-since-last-seen")
        .arg("--team")
        .arg("override-team")
        .assert()
        .success();
}
