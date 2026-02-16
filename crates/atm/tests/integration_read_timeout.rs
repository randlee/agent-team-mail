//! Integration tests for `atm read --timeout`

use assert_cmd::cargo;
use predicates::prelude::*;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn setup_team() -> (TempDir, PathBuf) {
    let temp_dir = TempDir::new().unwrap();
    let teams_dir = temp_dir.path().join(".claude/teams");
    let team_dir = teams_dir.join("test-team");
    let inboxes_dir = team_dir.join("inboxes");

    fs::create_dir_all(&inboxes_dir).unwrap();

    // Create team config
    let config = serde_json::json!({
        "name": "test-team",
        "createdAt": 1770765919076u64,
        "leadAgentId": "team-lead@test-team",
        "leadSessionId": "6075f866-f103-4be1-b2e9-8dbf66009eb9",
        "members": [
            {
                "agentId": "alice@test-team",
                "name": "alice",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1770765919076u64,
                "cwd": "/test",
                "subscriptions": []
            }
        ]
    });

    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    (temp_dir, team_dir)
}

#[test]
fn test_read_timeout_expires() {
    let (temp_dir, team_dir) = setup_team();

    // Create empty inbox
    let inbox_path = team_dir.join("inboxes/alice.json");
    fs::write(&inbox_path, "[]").unwrap();

    // Read with short timeout - should exit with code 1
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "alice")
        .arg("read")
        .arg("--timeout")
        .arg("1");

    cmd.assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("Timeout: No new messages"));
}

#[test]
fn test_read_timeout_message_arrives() {
    let (temp_dir, team_dir) = setup_team();

    // Create empty inbox
    let inbox_path = team_dir.join("inboxes/alice.json");
    fs::write(&inbox_path, "[]").unwrap();

    // Spawn thread to write message after 500ms
    let inbox_path_clone = inbox_path.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(500));
        let messages = serde_json::json!([
            {
                "from": "bob",
                "text": "Hello Alice!",
                "timestamp": "2026-02-16T00:00:00Z",
                "read": false,
                "summary": "Hello Alice!",
                "messageId": "msg-1"
            }
        ]);
        fs::write(&inbox_path_clone, serde_json::to_string(&messages).unwrap()).unwrap();
    });

    // Read with timeout - should receive message and exit with code 0
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "alice")
        .arg("read")
        .arg("--timeout")
        .arg("5");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Hello Alice!"))
        .stdout(predicate::str::contains("From: bob"));
}

#[test]
fn test_read_timeout_json_output() {
    let (temp_dir, team_dir) = setup_team();

    // Create empty inbox
    let inbox_path = team_dir.join("inboxes/alice.json");
    fs::write(&inbox_path, "[]").unwrap();

    // Read with timeout and JSON output
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "alice")
        .arg("read")
        .arg("--timeout")
        .arg("1")
        .arg("--json");

    cmd.assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("\"timeout\": true"))
        .stdout(predicate::str::contains("\"count\": 0"));
}

#[test]
fn test_read_no_timeout_no_messages() {
    let (temp_dir, team_dir) = setup_team();

    // Create empty inbox
    let inbox_path = team_dir.join("inboxes/alice.json");
    fs::write(&inbox_path, "[]").unwrap();

    // Read without timeout - should exit immediately with code 0
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "alice")
        .arg("read");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("No messages found"));
}

#[test]
fn test_read_timeout_with_existing_messages() {
    let (temp_dir, team_dir) = setup_team();

    // Create inbox with existing message
    let inbox_path = team_dir.join("inboxes/alice.json");
    let messages = serde_json::json!([
        {
            "from": "bob",
            "text": "Existing message",
            "timestamp": "2026-02-16T00:00:00Z",
            "read": false,
            "summary": "Existing message",
            "messageId": "msg-1"
        }
    ]);
    fs::write(&inbox_path, serde_json::to_string(&messages).unwrap()).unwrap();

    // Read with timeout - should return immediately with existing message
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "alice")
        .arg("read")
        .arg("--timeout")
        .arg("5");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Existing message"))
        .stdout(predicate::str::contains("From: bob"));
}
