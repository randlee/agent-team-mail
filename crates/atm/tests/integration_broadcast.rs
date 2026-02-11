//! Integration tests for the broadcast command

use assert_cmd::cargo;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Helper to set home directory for cross-platform test compatibility.
/// Uses `ATM_HOME` which is checked first by `get_home_dir()`, avoiding
/// platform-specific differences in how `dirs::home_dir()` resolves.
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    cmd.env("ATM_HOME", temp_dir.path());
}

/// Create a test team structure with multiple agents
fn setup_test_team(temp_dir: &TempDir, team_name: &str) -> PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");

    fs::create_dir_all(&inboxes_dir).unwrap();

    // Create team config.json with multiple agents
    let config = serde_json::json!({
        "name": team_name,
        "description": "Test team for broadcast",
        "createdAt": 1739284800000i64,
        "leadAgentId": format!("human@{}", team_name),
        "leadSessionId": "test-session-id",
        "members": [
            {
                "agentId": format!("human@{}", team_name),
                "name": "human",
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
                "model": "claude-opus-4-6",
                "prompt": "Test agent 2",
                "color": "green",
                "planModeRequired": false,
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%2",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "backendType": "tmux",
                "isActive": true
            },
            {
                "agentId": format!("agent-3@{}", team_name),
                "name": "agent-3",
                "agentType": "general-purpose",
                "model": "claude-sonnet-4-5-20250929",
                "prompt": "Test agent 3",
                "color": "yellow",
                "planModeRequired": false,
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%3",
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

#[test]
fn test_broadcast_basic_message() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("broadcast")
        .arg("Hello, team!")
        .assert()
        .success();

    // Verify inbox files were created for all agents (except sender)
    let inboxes_dir = temp_dir.path().join(".claude/teams/test-team/inboxes");

    // agent-1, agent-2, agent-3 should have messages
    for agent in &["agent-1", "agent-2", "agent-3"] {
        let inbox_path = inboxes_dir.join(format!("{agent}.json"));
        assert!(inbox_path.exists(), "Inbox for {agent} should exist");

        let inbox_content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["from"], "human");
        assert_eq!(messages[0]["text"], "Hello, team!");
        assert_eq!(messages[0]["read"], false);
        assert!(messages[0]["message_id"].is_string());
    }

    // human (sender) should NOT have a message
    let human_inbox = inboxes_dir.join("human.json");
    assert!(!human_inbox.exists(), "Sender should not receive broadcast");
}

#[test]
fn test_broadcast_with_team_flag() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "override-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "default-team")
        .arg("broadcast")
        .arg("--team")
        .arg("override-team")
        .arg("Team broadcast message")
        .assert()
        .success();

    // Verify messages were sent to override-team
    let inboxes_dir = temp_dir.path().join(".claude/teams/override-team/inboxes");

    for agent in &["agent-1", "agent-2", "agent-3"] {
        let inbox_path = inboxes_dir.join(format!("{agent}.json"));
        assert!(inbox_path.exists());
    }
}

#[test]
fn test_broadcast_with_stdin() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("broadcast")
        .arg("--stdin")
        .write_stdin("Broadcast from stdin")
        .assert()
        .success();

    // Verify message content
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-1.json");

    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages[0]["text"], "Broadcast from stdin");
}

#[test]
fn test_broadcast_with_summary() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("broadcast")
        .arg("--summary")
        .arg("Custom broadcast summary")
        .arg("Long message content that would normally be truncated")
        .assert()
        .success();

    // Verify message has custom summary
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-1.json");

    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages[0]["summary"], "Custom broadcast summary");
}

#[test]
fn test_broadcast_json_output() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("broadcast")
        .arg("--json")
        .arg("Test broadcast")
        .assert()
        .success();

    // Command succeeding with --json is a good smoke test
    // Could add stdout validation for JSON structure if needed
}

#[test]
fn test_broadcast_dry_run() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("broadcast")
        .arg("--dry-run")
        .arg("Dry run broadcast")
        .assert()
        .success();

    // Verify NO inbox files were created
    let inboxes_dir = temp_dir.path().join(".claude/teams/test-team/inboxes");

    for agent in &["agent-1", "agent-2", "agent-3"] {
        let inbox_path = inboxes_dir.join(format!("{agent}.json"));
        assert!(!inbox_path.exists(), "Dry run should not create inboxes");
    }
}

#[test]
fn test_broadcast_multiple_times_append() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // First broadcast
    let mut cmd1 = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd1, &temp_dir);
    cmd1.env("ATM_TEAM", "test-team")
        .arg("broadcast")
        .arg("First broadcast")
        .assert()
        .success();

    // Second broadcast
    let mut cmd2 = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd2, &temp_dir);
    cmd2.env("ATM_TEAM", "test-team")
        .arg("broadcast")
        .arg("Second broadcast")
        .assert()
        .success();

    // Verify both messages are in each agent's inbox
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-1.json");

    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["text"], "First broadcast");
    assert_eq!(messages[1]["text"], "Second broadcast");
}

#[test]
fn test_broadcast_team_not_found() {
    let temp_dir = TempDir::new().unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "nonexistent-team")
        .arg("broadcast")
        .arg("Test message")
        .assert()
        .failure();
}

#[test]
fn test_broadcast_empty_team() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = temp_dir.path().join(".claude/teams/empty-team");
    fs::create_dir_all(&team_dir).unwrap();

    // Create team config with only sender (no other agents)
    let config = serde_json::json!({
        "name": "empty-team",
        "description": "Team with only sender",
        "createdAt": 1739284800000i64,
        "leadAgentId": "human@empty-team",
        "leadSessionId": "test-session-id",
        "members": [
            {
                "agentId": "human@empty-team",
                "name": "human",
                "agentType": "general-purpose",
                "model": "claude-haiku-4-5-20251001",
                "joinedAt": 1739284800000i64,
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": []
            }
        ]
    });

    let config_path = team_dir.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "empty-team")
        .arg("broadcast")
        .arg("Test message")
        .assert()
        .failure(); // Should fail - no agents to broadcast to
}

#[test]
fn test_broadcast_cross_team() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir1 = setup_test_team(&temp_dir, "team-a");
    let _team_dir2 = setup_test_team(&temp_dir, "team-b");

    // Broadcast to team-b while default is team-a
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "team-a")
        .arg("broadcast")
        .arg("--team")
        .arg("team-b")
        .arg("Cross-team broadcast")
        .assert()
        .success();

    // Verify messages went to team-b
    let team_b_inbox = temp_dir
        .path()
        .join(".claude/teams/team-b/inboxes/agent-1.json");

    assert!(team_b_inbox.exists());

    let inbox_content = fs::read_to_string(&team_b_inbox).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages[0]["text"], "Cross-team broadcast");

    // Verify no messages in team-a
    let team_a_inbox = temp_dir
        .path()
        .join(".claude/teams/team-a/inboxes/agent-1.json");

    assert!(!team_a_inbox.exists());
}
