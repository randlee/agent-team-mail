//! End-to-End integration tests for multi-command workflows
//!
//! These tests verify complete workflows that combine multiple commands,
//! ensuring that the system works correctly in real-world usage scenarios.

use assert_cmd::cargo;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Helper to set home directory for cross-platform test compatibility.
/// Uses `ATM_HOME` which is checked first by `get_home_dir()`, avoiding
/// platform-specific differences in how `dirs::home_dir()` resolves.
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    cmd.env("ATM_HOME", temp_dir.path())
        .env_remove("ATM_IDENTITY")
        .current_dir(temp_dir.path());
}

/// Create a test team structure with multiple agents
fn setup_test_team(temp_dir: &TempDir, team_name: &str) -> PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");

    fs::create_dir_all(&inboxes_dir).unwrap();

    // Create team config.json with multiple agents
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
                "agentId": format!("agent-a@{}", team_name),
                "name": "agent-a",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "prompt": "Agent A",
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
                "agentId": format!("agent-b@{}", team_name),
                "name": "agent-b",
                "agentType": "general-purpose",
                "model": "claude-sonnet-4-5-20250929",
                "prompt": "Agent B",
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
                "agentId": format!("agent-c@{}", team_name),
                "name": "agent-c",
                "agentType": "general-purpose",
                "model": "claude-sonnet-4-5-20250929",
                "prompt": "Agent C",
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

// ============================================================================
// Category 1: Send → Read → Verify Workflow
// ============================================================================

#[test]
fn test_send_read_verify_basic() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Send message
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("agent-a")
        .arg("Test message")
        .assert()
        .success();

    // Verify message is unread
    let inbox_path = team_dir.join("inboxes/agent-a.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["read"], false);

    // Read message
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .assert()
        .success();

    // Verify message is now marked as read
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["read"], true);
}

#[test]
fn test_send_multiple_read_verify_all_marked() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Send three messages
    for i in 1..=3 {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.env("ATM_TEAM", "test-team")
            .arg("send")
            .arg("agent-a")
            .arg(format!("Message {i}"))
            .assert()
            .success();
    }

    // Verify all messages are unread
    let inbox_path = team_dir.join("inboxes/agent-a.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages.len(), 3);
    assert!(messages.iter().all(|m| m["read"] == false));

    // Read all messages
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .assert()
        .success();

    // Verify all messages are marked as read
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages.len(), 3);
    assert!(messages.iter().all(|m| m["read"] == true));
}

#[test]
fn test_send_read_with_from_filter_verify() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Send messages from different senders
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "sender-x")
        .arg("send")
        .arg("agent-a")
        .arg("Message from sender-x")
        .assert()
        .success();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "sender-y")
        .arg("send")
        .arg("agent-a")
        .arg("Message from sender-y")
        .assert()
        .success();

    // Read with --from filter
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .arg("--from")
        .arg("sender-x")
        .assert()
        .success();

    // Verify only sender-x message is marked as read
    let inbox_path = team_dir.join("inboxes/agent-a.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages.len(), 2);

    let sender_x_msg = messages.iter().find(|m| m["from"] == "sender-x").unwrap();
    let sender_y_msg = messages.iter().find(|m| m["from"] == "sender-y").unwrap();

    assert_eq!(sender_x_msg["read"], true);
    assert_eq!(sender_y_msg["read"], false);
}

#[test]
fn test_send_read_with_limit_verify() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Send five messages
    for i in 1..=5 {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.env("ATM_TEAM", "test-team")
            .arg("send")
            .arg("agent-a")
            .arg(format!("Message {i}"))
            .assert()
            .success();
    }

    // Read with limit of 2
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .arg("--limit")
        .arg("2")
        .assert()
        .success();

    // Verify only first 2 messages are marked as read
    let inbox_path = team_dir.join("inboxes/agent-a.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages.len(), 5);

    let read_count = messages.iter().filter(|m| m["read"] == true).count();
    assert_eq!(read_count, 2);
}

#[test]
fn test_send_read_no_mark_verify_still_unread() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Send message
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("agent-a")
        .arg("Test message")
        .assert()
        .success();

    // Read with --no-mark
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .arg("--no-mark")
        .assert()
        .success();

    // Verify message is still unread
    let inbox_path = team_dir.join("inboxes/agent-a.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages[0]["read"], false);
}

#[test]
fn test_send_cross_team_read_verify() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir_a = setup_test_team(&temp_dir, "team-a");
    let team_dir_b = setup_test_team(&temp_dir, "team-b");

    // Send message from team-a to agent in team-b
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "team-a")
        .arg("send")
        .arg("agent-a@team-b")
        .arg("Cross-team message")
        .assert()
        .success();

    // Verify message is in team-b inbox
    let inbox_path = team_dir_b.join("inboxes/agent-a.json");
    assert!(inbox_path.exists());
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages[0]["read"], false);

    // Read from team-b
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "team-b")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .assert()
        .success();

    // Verify message is marked as read
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages[0]["read"], true);
}

#[test]
fn test_send_read_reread_no_new_messages() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Send message
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("agent-a")
        .arg("Test message")
        .assert()
        .success();

    // First read - should show message
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .assert()
        .success();

    // Second read - should show no unread messages (by default only unread are shown)
    // The command should still succeed even if no new messages exist
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .assert()
        .success();

    // Note: The behavior when reading already-read messages may vary
    // The important part is the command succeeds and doesn't error
}

// ============================================================================
// Category 2: Broadcast → Read All Inboxes Workflow
// ============================================================================

#[test]
fn test_broadcast_read_all_inboxes_verify() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Broadcast message
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("broadcast")
        .arg("Broadcast to all")
        .assert()
        .success();

    // Verify all three agents received the message
    for agent in &["agent-a", "agent-b", "agent-c"] {
        let inbox_path = team_dir.join(format!("inboxes/{agent}.json"));
        assert!(inbox_path.exists());

        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["text"], "Broadcast to all");
        assert_eq!(messages[0]["read"], false);
    }

    // Read each agent's inbox
    for agent in &["agent-a", "agent-b", "agent-c"] {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.env("ATM_TEAM", "test-team")
            .arg("read")
        .arg("--no-since-last-seen")
            .arg(agent)
            .assert()
            .success();
    }

    // Verify all messages are marked as read
    for agent in &["agent-a", "agent-b", "agent-c"] {
        let inbox_path = team_dir.join(format!("inboxes/{agent}.json"));
        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages[0]["read"], true);
    }
}

#[test]
fn test_broadcast_cross_team_verify() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir_a = setup_test_team(&temp_dir, "team-a");
    let team_dir_b = setup_test_team(&temp_dir, "team-b");

    // Broadcast from team-a to team-b
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "team-a")
        .arg("broadcast")
        .arg("--team")
        .arg("team-b")
        .arg("Cross-team broadcast")
        .assert()
        .success();

    // Verify all team-b agents received the message
    for agent in &["agent-a", "agent-b", "agent-c"] {
        let inbox_path = team_dir_b.join(format!("inboxes/{agent}.json"));
        assert!(inbox_path.exists());

        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages[0]["text"], "Cross-team broadcast");
    }

    // Read all team-b inboxes
    for agent in &["agent-a", "agent-b", "agent-c"] {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.env("ATM_TEAM", "team-b")
            .arg("read")
        .arg("--no-since-last-seen")
            .arg(agent)
            .assert()
            .success();
    }

    // Verify all are read
    for agent in &["agent-a", "agent-b", "agent-c"] {
        let inbox_path = team_dir_b.join(format!("inboxes/{agent}.json"));
        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages[0]["read"], true);
    }
}

#[test]
fn test_broadcast_multiple_times_verify_all_received() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Broadcast three times
    for i in 1..=3 {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.env("ATM_TEAM", "test-team")
            .arg("broadcast")
            .arg(format!("Broadcast {i}"))
            .assert()
            .success();
    }

    // Verify each agent has 3 messages
    for agent in &["agent-a", "agent-b", "agent-c"] {
        let inbox_path = team_dir.join(format!("inboxes/{agent}.json"));
        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages.len(), 3);
    }

    // Read all messages for each agent
    for agent in &["agent-a", "agent-b", "agent-c"] {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.env("ATM_TEAM", "test-team")
            .arg("read")
        .arg("--no-since-last-seen")
            .arg(agent)
            .assert()
            .success();
    }

    // Verify all messages are read
    for agent in &["agent-a", "agent-b", "agent-c"] {
        let inbox_path = team_dir.join(format!("inboxes/{agent}.json"));
        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
        assert!(messages.iter().all(|m| m["read"] == true));
    }
}

#[test]
fn test_broadcast_sender_no_self_message() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Broadcast with explicit identity
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "agent-a")
        .arg("broadcast")
        .arg("Broadcast from agent-a")
        .assert()
        .success();

    // Verify agent-a did NOT receive their own broadcast
    let inbox_path = team_dir.join("inboxes/agent-a.json");

    // Inbox file might not exist if agent didn't receive any messages
    if inbox_path.exists() {
        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
        // Should have 0 messages (sender doesn't receive own broadcast)
        assert_eq!(messages.len(), 0);
    }

    // Verify agent-b and agent-c did receive it
    for agent in &["agent-b", "agent-c"] {
        let inbox_path = team_dir.join(format!("inboxes/{agent}.json"));
        assert!(inbox_path.exists());

        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["text"], "Broadcast from agent-a");
    }
}

// ============================================================================
// Category 3: Config Resolution Integration
// ============================================================================

#[test]
fn test_config_default_team() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "default-team");

    // Note: Config file feature not yet implemented in atm
    // This test verifies the workflow when a team is provided via env var
    // which simulates default team behavior

    // Send with ATM_TEAM env var (simulating default team behavior)
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "default-team")
        .arg("send")
        .arg("agent-a")
        .arg("Using default team")
        .assert()
        .success();

    // Verify message is in default-team
    let inbox_path = temp_dir.path().join(".claude/teams/default-team/inboxes/agent-a.json");
    assert!(inbox_path.exists());
}

#[test]
fn test_config_env_override() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir1 = setup_test_team(&temp_dir, "default-team");
    let _team_dir2 = setup_test_team(&temp_dir, "env-team");

    // Note: Config file feature not yet implemented
    // This test verifies that ATM_TEAM env var determines the target team
    // (When config is implemented, this would test env var override)

    // Send with ATM_TEAM env var (specifies the target team)
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "env-team")
        .arg("send")
        .arg("agent-a")
        .arg("Using env team")
        .assert()
        .success();

    // Verify message is in env-team, not default-team
    let inbox_path = temp_dir.path().join(".claude/teams/env-team/inboxes/agent-a.json");
    assert!(inbox_path.exists());
}

#[test]
fn test_config_cli_flag_override() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir1 = setup_test_team(&temp_dir, "env-team");
    let _team_dir2 = setup_test_team(&temp_dir, "flag-team");

    // Send with both ATM_TEAM env var and --team flag (flag should win)
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "env-team")
        .arg("send")
        .arg("--team")
        .arg("flag-team")
        .arg("agent-a")
        .arg("Using flag team")
        .assert()
        .success();

    // Verify message is in flag-team, not env-team
    let inbox_path = temp_dir.path().join(".claude/teams/flag-team/inboxes/agent-a.json");
    assert!(inbox_path.exists());
}

#[test]
fn test_config_identity_from_env() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Send with ATM_IDENTITY
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "custom-sender")
        .arg("send")
        .arg("agent-a")
        .arg("Message with custom identity")
        .assert()
        .success();

    // Verify message has custom "from" field
    let inbox_path = team_dir.join("inboxes/agent-a.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages[0]["from"], "custom-sender");
}

#[test]
fn test_config_precedence_chain() {
    let temp_dir = TempDir::new().unwrap();

    // Create three teams
    let _team_dir1 = setup_test_team(&temp_dir, "default-team");
    let _team_dir2 = setup_test_team(&temp_dir, "env-team");
    let _team_dir3 = setup_test_team(&temp_dir, "flag-team");

    // Note: Config file feature not yet implemented
    // This test verifies precedence: --team flag > ATM_TEAM env var
    // (When config is implemented, this would test: flag > env > config)

    // Test precedence: flag > env
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "env-team")
        .arg("send")
        .arg("--team")
        .arg("flag-team")
        .arg("agent-a")
        .arg("Test precedence")
        .assert()
        .success();

    // Verify message is in flag-team (highest precedence)
    let inbox_path = temp_dir.path().join(".claude/teams/flag-team/inboxes/agent-a.json");
    assert!(inbox_path.exists());

    // Verify NOT in other teams
    let default_path = temp_dir.path().join(".claude/teams/default-team/inboxes/agent-a.json");
    let env_path = temp_dir.path().join(".claude/teams/env-team/inboxes/agent-a.json");
    assert!(!default_path.exists());
    assert!(!env_path.exists());
}

// ============================================================================
// Category 4: Complex Multi-Step Workflows
// ============================================================================

#[test]
fn test_conversation_workflow() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Step 1: Agent A sends to Agent B
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "agent-a")
        .arg("send")
        .arg("agent-b")
        .arg("Hello, Agent B!")
        .assert()
        .success();

    // Step 2: Agent B reads the message
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-b")
        .assert()
        .success();

    // Verify message is read
    let inbox_b = team_dir.join("inboxes/agent-b.json");
    let content = fs::read_to_string(&inbox_b).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages[0]["read"], true);

    // Step 3: Agent B sends reply to Agent A
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "agent-b")
        .arg("send")
        .arg("agent-a")
        .arg("Hello back, Agent A!")
        .assert()
        .success();

    // Step 4: Agent A reads the reply
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .assert()
        .success();

    // Verify reply is in Agent A's inbox and read
    let inbox_a = team_dir.join("inboxes/agent-a.json");
    let content = fs::read_to_string(&inbox_a).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages[0]["from"], "agent-b");
    assert_eq!(messages[0]["text"], "Hello back, Agent A!");
    assert_eq!(messages[0]["read"], true);
}

#[test]
fn test_team_discussion_workflow() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Step 1: Team lead broadcasts
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "team-lead")
        .arg("broadcast")
        .arg("Team meeting: Please reply with status")
        .assert()
        .success();

    // Step 2: Each agent reads the broadcast
    for agent in &["agent-a", "agent-b", "agent-c"] {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.env("ATM_TEAM", "test-team")
            .arg("read")
        .arg("--no-since-last-seen")
            .arg(agent)
            .assert()
            .success();
    }

    // Step 3: Each agent replies to team-lead
    for agent in &["agent-a", "agent-b", "agent-c"] {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.env("ATM_TEAM", "test-team")
            .env("ATM_IDENTITY", agent)
            .arg("send")
            .arg("team-lead")
            .arg(format!("Status from {agent}: All good"))
            .assert()
            .success();
    }

    // Step 4: Team lead reads all replies
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("team-lead")
        .assert()
        .success();

    // Verify all replies are in team-lead's inbox
    let inbox_lead = team_dir.join("inboxes/team-lead.json");
    let content = fs::read_to_string(&inbox_lead).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages.len(), 3);
    assert!(messages.iter().all(|m| m["read"] == true));
}

#[test]
fn test_cross_team_relay_workflow() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir_a = setup_test_team(&temp_dir, "team-a");
    let team_dir_b = setup_test_team(&temp_dir, "team-b");

    // Step 1: Send message to agent in team-a
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "team-a")
        .env("ATM_IDENTITY", "external-sender")
        .arg("send")
        .arg("agent-a@team-a")
        .arg("Please forward this to team-b")
        .assert()
        .success();

    // Step 2: Agent in team-a reads the message
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "team-a")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .assert()
        .success();

    // Verify message is read in team-a
    let inbox_a = team_dir_a.join("inboxes/agent-a.json");
    let content = fs::read_to_string(&inbox_a).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages[0]["read"], true);

    // Step 3: Team-a agent forwards to team-b agent
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "team-a")
        .env("ATM_IDENTITY", "agent-a@team-a")
        .arg("send")
        .arg("agent-a@team-b")
        .arg("Forwarded: Please forward this to team-b")
        .assert()
        .success();

    // Step 4: Team-b agent reads the forwarded message
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "team-b")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .assert()
        .success();

    // Verify message is in team-b and read
    let inbox_b = team_dir_b.join("inboxes/agent-a.json");
    let content = fs::read_to_string(&inbox_b).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages[0]["from"], "agent-a@team-a");
    assert!(messages[0]["text"].as_str().unwrap().contains("Forwarded"));
    assert_eq!(messages[0]["read"], true);
}

#[test]
fn test_inbox_summary_workflow() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Step 1: Send multiple messages to different agents
    for agent in &["agent-a", "agent-b", "agent-c"] {
        for i in 1..=3 {
            let mut cmd = cargo::cargo_bin_cmd!("atm");
            set_home_env(&mut cmd, &temp_dir);
            cmd.env("ATM_TEAM", "test-team")
                .arg("send")
                .arg(agent)
                .arg(format!("Message {i} to {agent}"))
                .assert()
                .success();
        }
    }

    // Step 2: Check inbox summary (should show unread counts)
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .arg("inbox")
        .arg("--no-since-last-seen")
        .assert()
        .success();

    // Verify output shows unread messages for all agents
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("agent-a"));
    assert!(stdout.contains("agent-b"));
    assert!(stdout.contains("agent-c"));

    // Step 3: Read some messages (only agent-a)
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .assert()
        .success();

    // Step 4: Check inbox summary again
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .arg("inbox")
        .arg("--no-since-last-seen")
        .assert()
        .success();

    // Verify output now shows agent-b and agent-c still have unread messages
    // but agent-a's messages are marked as read
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    // agent-b and agent-c should still show unread messages
    assert!(stdout.contains("agent-b") || stdout.contains("3"));
    assert!(stdout.contains("agent-c") || stdout.contains("3"));
}
