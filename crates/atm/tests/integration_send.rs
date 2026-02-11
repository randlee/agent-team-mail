//! Integration tests for the send command

use assert_cmd::cargo;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Helper to set home directory for cross-platform test compatibility.
/// Uses `ATM_HOME` which is checked first by `get_home_dir()`, avoiding
/// platform-specific differences in how `dirs::home_dir()` resolves
/// (HOME on Unix, Windows API on Windows).
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    cmd.env("ATM_HOME", temp_dir.path());
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

#[test]
fn test_send_basic_message() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("Hello, test agent!")
        .assert()
        .success();

    // Verify inbox file was created
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/test-agent.json");

    assert!(inbox_path.exists());

    // Verify message content
    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["from"], "human");
    assert_eq!(messages[0]["text"], "Hello, test agent!");
    assert_eq!(messages[0]["read"], false);
    assert!(messages[0]["message_id"].is_string());
}

#[test]
fn test_send_cross_team_addressing() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir1 = setup_test_team(&temp_dir, "team-a");
    let _team_dir2 = setup_test_team(&temp_dir, "team-b");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "team-a")
        .arg("send")
        .arg("test-agent@team-b")
        .arg("Cross-team message")
        .assert()
        .success();

    // Verify inbox file was created in team-b
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/team-b/inboxes/test-agent.json");

    assert!(inbox_path.exists());

    // Verify message content
    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["text"], "Cross-team message");
}

#[test]
fn test_send_with_team_flag() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "override-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "default-team")
        .arg("send")
        .arg("--team")
        .arg("override-team")
        .arg("test-agent")
        .arg("Message with team flag")
        .assert()
        .success();

    // Verify inbox file was created in override-team
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/override-team/inboxes/test-agent.json");

    assert!(inbox_path.exists());
}

#[test]
fn test_send_with_summary() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("--summary")
        .arg("Custom summary")
        .arg("Long message content that would normally be truncated")
        .assert()
        .success();

    // Verify message has custom summary
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/test-agent.json");

    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages[0]["summary"], "Custom summary");
}

#[test]
fn test_send_json_output() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("--json")
        .arg("Test message")
        .assert()
        .success();

    // Verify output is valid JSON (assert_cmd captures stdout)
    // We can't easily verify the exact JSON output without more complex assertion
    // but the command succeeding with --json is a good smoke test
}

#[test]
fn test_send_dry_run() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("--dry-run")
        .arg("Dry run message")
        .assert()
        .success();

    // Verify inbox file was NOT created
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/test-agent.json");

    assert!(!inbox_path.exists());
}

#[test]
fn test_send_with_stdin() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("--stdin")
        .write_stdin("Message from stdin")
        .assert()
        .success();

    // Verify message content
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/test-agent.json");

    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages[0]["text"], "Message from stdin");
}

#[test]
fn test_send_with_file_reference() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Create a test file in the temp directory
    let test_file = temp_dir.path().join("test-file.txt");
    fs::write(&test_file, "File content").unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .current_dir(temp_dir.path())
        .arg("send")
        .arg("test-agent")
        .arg("--file")
        .arg(&test_file)
        .assert()
        .success();

    // Verify message includes file reference
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/test-agent.json");

    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    let text = messages[0]["text"].as_str().unwrap();
    assert!(text.contains("File reference:"));
    assert!(text.contains("test-file.txt"));
}

#[test]
fn test_send_agent_not_found() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("nonexistent-agent")
        .arg("Test message")
        .assert()
        .failure();
}

#[test]
fn test_send_team_not_found() {
    let temp_dir = TempDir::new().unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "nonexistent-team")
        .arg("send")
        .arg("test-agent")
        .arg("Test message")
        .assert()
        .failure();
}

#[test]
fn test_send_file_not_found() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("--file")
        .arg("/nonexistent/file.txt")
        .assert()
        .failure();
}

#[test]
fn test_send_multiple_messages_append() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Send first message
    let mut cmd1 = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd1, &temp_dir);
    cmd1.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("First message")
        .assert()
        .success();

    // Send second message
    let mut cmd2 = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd2, &temp_dir);
    cmd2.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("test-agent")
        .arg("Second message")
        .assert()
        .success();

    // Verify both messages are in inbox
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/test-agent.json");

    let inbox_content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&inbox_content).unwrap();

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["text"], "First message");
    assert_eq!(messages[1]["text"], "Second message");
}
