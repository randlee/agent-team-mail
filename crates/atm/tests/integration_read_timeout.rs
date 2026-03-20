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

fn setup_team_with_members(members: &[&str]) -> (TempDir, PathBuf) {
    let temp_dir = TempDir::new().unwrap();
    let teams_dir = temp_dir.path().join(".claude/teams");
    let team_dir = teams_dir.join("test-team");
    let inboxes_dir = team_dir.join("inboxes");

    fs::create_dir_all(&inboxes_dir).unwrap();

    let members_json: Vec<serde_json::Value> = members
        .iter()
        .map(|name| {
            serde_json::json!({
                "agentId": format!("{name}@test-team"),
                "name": name,
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1770765919076u64,
                "cwd": "/test",
                "subscriptions": []
            })
        })
        .collect();

    let config = serde_json::json!({
        "name": "test-team",
        "createdAt": 1770765919076u64,
        "leadAgentId": "team-lead@test-team",
        "leadSessionId": "6075f866-f103-4be1-b2e9-8dbf66009eb9",
        "members": members_json
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
        .env("ATM_DAEMON_AUTOSTART", "0")
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
        .env("ATM_DAEMON_AUTOSTART", "0")
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
        .env("ATM_DAEMON_AUTOSTART", "0")
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
        .env("ATM_DAEMON_AUTOSTART", "0")
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
        .env("ATM_DAEMON_AUTOSTART", "0")
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

#[test]
fn test_read_timeout_shows_older_unread_even_when_last_seen_is_newer() {
    let (temp_dir, team_dir) = setup_team();

    // Start with empty inbox so read enters wait path.
    let inbox_path = team_dir.join("inboxes/alice.json");
    fs::write(&inbox_path, "[]").unwrap();

    // Seed last-seen after incoming message timestamp.
    let state_path = temp_dir.path().join("state.json");
    let state = serde_json::json!({
        "last_seen": {
            "test-team": {
                "alice": "2026-02-16T12:00:00Z"
            }
        }
    });
    fs::write(&state_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    // Write an unread message whose timestamp is older than last_seen.
    let inbox_path_clone = inbox_path.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(500));
        let messages = serde_json::json!([
            {
                "from": "bob",
                "text": "Older unread while waiting",
                "timestamp": "2026-02-16T00:00:00Z",
                "read": false,
                "summary": "Older unread while waiting",
                "messageId": "msg-timeout-old-unread"
            }
        ]);
        fs::write(&inbox_path_clone, serde_json::to_string(&messages).unwrap()).unwrap();
    });

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "alice")
        .arg("read")
        .arg("--timeout")
        .arg("5");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Older unread while waiting"))
        .stdout(predicate::str::contains("From: bob"));
}

#[test]
fn test_read_timeout_without_agent_uses_config_identity() {
    let (temp_dir, team_dir) = setup_team_with_members(&["team-lead", "arch-ctm"]);

    let inboxes_dir = team_dir.join("inboxes");
    fs::write(inboxes_dir.join("team-lead.json"), "[]").unwrap();
    fs::write(inboxes_dir.join("arch-ctm.json"), "[]").unwrap();

    let workdir = temp_dir.path().join("repo");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(
        workdir.join(".atm.toml"),
        "[core]\ndefault_team = \"test-team\"\nidentity = \"team-lead\"\n",
    )
    .unwrap();

    let arch_inbox = inboxes_dir.join("arch-ctm.json");
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        let messages = serde_json::json!([
            {
                "from": "sender",
                "text": "message for arch",
                "timestamp": "2026-02-16T00:00:00Z",
                "read": false,
                "summary": "message for arch",
                "messageId": "msg-arch-1"
            }
        ]);
        fs::write(&arch_inbox, serde_json::to_string(&messages).unwrap()).unwrap();
    });

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .env_remove("ATM_CONFIG")
        .env_remove("CLAUDE_SESSION_ID")
        .current_dir(&workdir)
        .arg("read")
        .arg("--timeout")
        .arg("1");

    cmd.assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains(
            "Timeout: No new messages for team-lead@test-team",
        ));
}

#[test]
fn test_read_timeout_with_explicit_agent_overrides_default_identity() {
    let (temp_dir, team_dir) = setup_team_with_members(&["team-lead", "arch-ctm"]);

    let inboxes_dir = team_dir.join("inboxes");
    fs::write(inboxes_dir.join("team-lead.json"), "[]").unwrap();
    fs::write(inboxes_dir.join("arch-ctm.json"), "[]").unwrap();

    let workdir = temp_dir.path().join("repo");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(
        workdir.join(".atm.toml"),
        "[core]\ndefault_team = \"test-team\"\nidentity = \"team-lead\"\n",
    )
    .unwrap();

    let arch_inbox = inboxes_dir.join("arch-ctm.json");
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        let messages = serde_json::json!([
            {
                "from": "sender",
                "text": "message for explicit target",
                "timestamp": "2026-02-16T00:00:00Z",
                "read": false,
                "summary": "message for explicit target",
                "messageId": "msg-arch-2"
            }
        ]);
        fs::write(&arch_inbox, serde_json::to_string(&messages).unwrap()).unwrap();
    });

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .env_remove("ATM_CONFIG")
        .env_remove("CLAUDE_SESSION_ID")
        .current_dir(&workdir)
        .arg("read")
        .arg("arch-ctm")
        .arg("--timeout")
        .arg("5");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("message for explicit target"))
        .stdout(predicate::str::contains("Queue for arch-ctm@test-team"))
        .stdout(predicate::str::contains(
            "Unread: 1 | Pending Ack: 0 | History: 0",
        ));
}
