//! Integration tests for the read command

use assert_cmd::cargo;
use predicates::prelude::*;
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
fn test_read_unread_messages() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Create inbox with unread messages
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
            "text": "Already read",
            "timestamp": "2026-02-11T09:00:00Z",
            "read": true,
            "message_id": "msg-002"
        }),
        serde_json::json!({
            "from": "ci-agent",
            "text": "Unread message 2",
            "timestamp": "2026-02-11T11:00:00Z",
            "read": false,
            "message_id": "msg-003"
        }),
    ];
    create_test_inbox(&team_dir, "test-agent", messages);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("test-agent")
        .assert()
        .success();
}

#[test]
fn test_read_all_messages() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    let messages = vec![
        serde_json::json!({
            "from": "team-lead",
            "text": "Message 1",
            "timestamp": "2026-02-11T10:00:00Z",
            "read": false,
            "message_id": "msg-001"
        }),
        serde_json::json!({
            "from": "team-lead",
            "text": "Message 2",
            "timestamp": "2026-02-11T11:00:00Z",
            "read": true,
            "message_id": "msg-002"
        }),
    ];
    create_test_inbox(&team_dir, "test-agent", messages);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("test-agent")
        .arg("--all")
        .assert()
        .success();
}

#[test]
fn test_read_no_mark() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    let messages = vec![
        serde_json::json!({
            "from": "team-lead",
            "text": "Unread message",
            "timestamp": "2026-02-11T10:00:00Z",
            "read": false,
            "message_id": "msg-001"
        }),
    ];
    create_test_inbox(&team_dir, "test-agent", messages);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("test-agent")
        .arg("--no-mark")
        .assert()
        .success();

    // Verify message is still unread
    let inbox_path = team_dir.join("inboxes/test-agent.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages[0]["read"], false);
}

#[test]
fn test_read_marks_as_read() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    let messages = vec![
        serde_json::json!({
            "from": "team-lead",
            "text": "Unread message",
            "timestamp": "2026-02-11T10:00:00Z",
            "read": false,
            "message_id": "msg-001"
        }),
    ];
    create_test_inbox(&team_dir, "test-agent", messages);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("test-agent")
        .assert()
        .success();

    // Verify message was marked as read
    let inbox_path = team_dir.join("inboxes/test-agent.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages[0]["read"], true);
}

#[test]
fn test_read_filter_by_from() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    let messages = vec![
        serde_json::json!({
            "from": "team-lead",
            "text": "From team-lead",
            "timestamp": "2026-02-11T10:00:00Z",
            "read": false,
            "message_id": "msg-001"
        }),
        serde_json::json!({
            "from": "ci-agent",
            "text": "From ci-agent",
            "timestamp": "2026-02-11T11:00:00Z",
            "read": false,
            "message_id": "msg-002"
        }),
    ];
    create_test_inbox(&team_dir, "test-agent", messages);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("test-agent")
        .arg("--from")
        .arg("team-lead")
        .assert()
        .success();
}

#[test]
fn test_read_with_limit() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    let messages = vec![
        serde_json::json!({
            "from": "team-lead",
            "text": "Message 1",
            "timestamp": "2026-02-11T10:00:00Z",
            "read": false,
            "message_id": "msg-001"
        }),
        serde_json::json!({
            "from": "team-lead",
            "text": "Message 2",
            "timestamp": "2026-02-11T11:00:00Z",
            "read": false,
            "message_id": "msg-002"
        }),
        serde_json::json!({
            "from": "team-lead",
            "text": "Message 3",
            "timestamp": "2026-02-11T12:00:00Z",
            "read": false,
            "message_id": "msg-003"
        }),
    ];
    create_test_inbox(&team_dir, "test-agent", messages);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("test-agent")
        .arg("--limit")
        .arg("2")
        .assert()
        .success();
}

#[test]
fn test_read_empty_inbox() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // No inbox file created - should be treated as empty, not error

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("test-agent")
        .assert()
        .success();
}

#[test]
fn test_read_agent_not_found() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("nonexistent-agent")
        .assert()
        .failure();
}

#[test]
fn test_read_team_not_found() {
    let temp_dir = TempDir::new().unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "nonexistent-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("test-agent")
        .assert()
        .failure();
}

#[test]
fn test_read_json_output() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    let messages = vec![
        serde_json::json!({
            "from": "team-lead",
            "text": "Test message",
            "timestamp": "2026-02-11T10:00:00Z",
            "read": false,
            "message_id": "msg-001"
        }),
    ];
    create_test_inbox(&team_dir, "test-agent", messages);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("test-agent")
        .arg("--json")
        .assert()
        .success();
}

#[test]
fn test_read_since_last_seen_default() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    let messages = vec![
        serde_json::json!({
            "from": "team-lead",
            "text": "Old message",
            "timestamp": "2026-02-11T10:00:00Z",
            "read": true,
            "message_id": "msg-010"
        }),
        serde_json::json!({
            "from": "team-lead",
            "text": "New message",
            "timestamp": "2026-02-11T11:00:00Z",
            "read": true,
            "message_id": "msg-011"
        }),
    ];
    create_test_inbox(&team_dir, "test-agent", messages);

    // Seed last-seen state at 10:30
    let state_path = temp_dir.path().join("state.json");
    let state = serde_json::json!({
        "last_seen": {
            "test-team": {
                "test-agent": "2026-02-11T10:30:00Z"
            }
        }
    });
    fs::write(&state_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("test-agent")
        .assert()
        .success()
        .stdout(predicates::str::contains("New message"))
        .stdout(predicates::str::contains("Old message").not());

    // Verify last-seen updated to latest message
    let updated = fs::read_to_string(&state_path).unwrap();
    let updated_json: serde_json::Value = serde_json::from_str(&updated).unwrap();
    let ts = updated_json["last_seen"]["test-team"]["test-agent"]
        .as_str()
        .unwrap();
    assert!(ts.starts_with("2026-02-11T11:00:00"));
}

#[test]
fn test_read_no_update_seen() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    let messages = vec![
        serde_json::json!({
            "from": "team-lead",
            "text": "New message",
            "timestamp": "2026-02-11T11:00:00Z",
            "read": true,
            "message_id": "msg-020"
        }),
    ];
    create_test_inbox(&team_dir, "test-agent", messages);

    // Seed last-seen state at 10:00
    let state_path = temp_dir.path().join("state.json");
    let state = serde_json::json!({
        "last_seen": {
            "test-team": {
                "test-agent": "2026-02-11T10:00:00Z"
            }
        }
    });
    fs::write(&state_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("test-agent")
        .arg("--no-update-seen")
        .assert()
        .success()
        .stdout(predicates::str::contains("New message"));

    // Verify last-seen was NOT updated (still at 10:00)
    let updated = fs::read_to_string(&state_path).unwrap();
    let updated_json: serde_json::Value = serde_json::from_str(&updated).unwrap();
    let ts = updated_json["last_seen"]["test-team"]["test-agent"]
        .as_str()
        .unwrap();
    assert!(ts.starts_with("2026-02-11T10:00:00"));
}
