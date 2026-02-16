//! Integration tests for auto-detect sender identity

use assert_cmd::cargo;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn setup_team() -> (TempDir, PathBuf) {
    let temp_dir = TempDir::new().unwrap();
    let teams_dir = temp_dir.path().join(".claude/teams");
    let team_dir = teams_dir.join("test-team");
    let inboxes_dir = team_dir.join("inboxes");

    fs::create_dir_all(&inboxes_dir).unwrap();

    // Create team config with multiple agents
    let config = serde_json::json!({
        "name": "test-team",
        "createdAt": 1770765919076u64,
        "leadAgentId": "team-lead@test-team",
        "leadSessionId": "6075f866-f103-4be1-b2e9-8dbf66009eb9",
        "members": [
            {
                "agentId": "team-lead@test-team",
                "name": "team-lead",
                "agentType": "general-purpose",
                "model": "claude-haiku-4-5-20251001",
                "joinedAt": 1770765919076u64,
                "cwd": "/test/workspace",
                "subscriptions": []
            },
            {
                "agentId": "alice@test-team",
                "name": "alice",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1770765919077u64,
                "cwd": "/test/alice-workspace",
                "subscriptions": []
            },
            {
                "agentId": "bob@test-team",
                "name": "bob",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1770765919078u64,
                "cwd": "/test/bob-workspace",
                "subscriptions": []
            }
        ]
    });

    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    // Create empty inboxes
    fs::write(team_dir.join("inboxes/team-lead.json"), "[]").unwrap();
    fs::write(team_dir.join("inboxes/alice.json"), "[]").unwrap();
    fs::write(team_dir.join("inboxes/bob.json"), "[]").unwrap();

    (temp_dir, team_dir)
}

#[test]
fn test_send_defaults_to_human_when_no_identity() {
    let (temp_dir, _team_dir) = setup_team();

    // Send without ATM_IDENTITY or --from should default to "human"
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_TEAM", "test-team")
        .env_remove("ATM_IDENTITY") // Ensure no identity env var
        .arg("send")
        .arg("alice")
        .arg("Hello from human");

    cmd.assert().success();

    // Verify message is from "human"
    let inbox = fs::read_to_string(
        temp_dir
            .path()
            .join(".claude/teams/test-team/inboxes/alice.json"),
    )
    .unwrap();
    assert!(inbox.contains("\"from\": \"human\""));
}

#[test]
fn test_send_with_atm_identity_env() {
    let (temp_dir, _team_dir) = setup_team();

    // Send with ATM_IDENTITY env var
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "alice")
        .arg("send")
        .arg("bob")
        .arg("Hello from Alice");

    cmd.assert().success();

    // Verify message is from "alice"
    let inbox = fs::read_to_string(
        temp_dir
            .path()
            .join(".claude/teams/test-team/inboxes/bob.json"),
    )
    .unwrap();
    assert!(inbox.contains("\"from\": \"alice\""));
}

#[test]
fn test_send_with_from_flag_overrides_env() {
    let (temp_dir, _team_dir) = setup_team();

    // Send with --from flag should override ATM_IDENTITY
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "alice")
        .arg("send")
        .arg("bob")
        .arg("--from")
        .arg("team-lead")
        .arg("Hello from team-lead");

    cmd.assert().success();

    // Verify message is from "team-lead"
    let inbox = fs::read_to_string(
        temp_dir
            .path()
            .join(".claude/teams/test-team/inboxes/bob.json"),
    )
    .unwrap();
    assert!(inbox.contains("\"from\": \"team-lead\""));
}

#[test]
fn test_send_without_team_context_defaults_to_human() {
    let temp_dir = TempDir::new().unwrap();

    // Create a team without the sender being a member
    let teams_dir = temp_dir.path().join(".claude/teams");
    let team_dir = teams_dir.join("external-team");
    let inboxes_dir = team_dir.join("inboxes");

    fs::create_dir_all(&inboxes_dir).unwrap();

    let config = serde_json::json!({
        "name": "external-team",
        "createdAt": 1770765919076u64,
        "leadAgentId": "team-lead@external-team",
        "leadSessionId": "some-other-session",
        "members": [
            {
                "agentId": "alice@external-team",
                "name": "alice",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1770765919076u64,
                "cwd": "/test/alice",
                "subscriptions": []
            }
        ]
    });

    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    fs::write(team_dir.join("inboxes/alice.json"), "[]").unwrap();

    // Send without matching identity should default to "human"
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_TEAM", "external-team")
        .env_remove("ATM_IDENTITY")
        .arg("send")
        .arg("alice")
        .arg("Message from outside");

    cmd.assert().success();

    // Verify message is from "human"
    let inbox = fs::read_to_string(
        temp_dir
            .path()
            .join(".claude/teams/external-team/inboxes/alice.json"),
    )
    .unwrap();
    assert!(inbox.contains("\"from\": \"human\""));
}

#[test]
fn test_send_custom_identity_not_in_team() {
    let (temp_dir, _team_dir) = setup_team();

    // Send with custom identity via --from (not in team members)
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_TEAM", "test-team")
        .env_remove("ATM_IDENTITY")
        .arg("send")
        .arg("alice")
        .arg("--from")
        .arg("external-bot")
        .arg("Message from external bot");

    cmd.assert().success();

    // Verify message is from "external-bot"
    let inbox = fs::read_to_string(
        temp_dir
            .path()
            .join(".claude/teams/test-team/inboxes/alice.json"),
    )
    .unwrap();
    assert!(inbox.contains("\"from\": \"external-bot\""));
}
