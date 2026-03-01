//! Integration tests for `atm monitor`.

use assert_cmd::cargo;
use std::fs;
use tempfile::TempDir;

fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).ok();
    cmd.env("ATM_HOME", temp_dir.path())
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .env_remove("ATM_CONFIG")
        .env_remove("CLAUDE_SESSION_ID")
        .current_dir(&workdir);
}

fn setup_team(temp_dir: &TempDir, team_name: &str) {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();
    let config = serde_json::json!({
        "name": team_name,
        "description": "Monitor test team",
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
            }
        ]
    });
    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    fs::write(inboxes_dir.join("team-lead.json"), "[]").unwrap();
}

#[test]
fn test_monitor_once_emits_alert_for_critical_finding() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("monitor")
        .arg("--team")
        .arg("test-team")
        .arg("--notify")
        .arg("team-lead")
        .arg("--once")
        .assert()
        .success();

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/team-lead.json");
    let content = fs::read_to_string(inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert!(
        !messages.is_empty(),
        "monitor should emit at least one alert"
    );
    let last = messages.last().unwrap();
    assert_eq!(last["from"].as_str(), Some("atm-monitor"));
    let text = last["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("[atm-monitor]"),
        "alert should include monitor prefix"
    );
}

#[test]
fn test_monitor_dedup_suppresses_repeat_within_cooldown() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("monitor")
        .arg("--team")
        .arg("test-team")
        .arg("--notify")
        .arg("team-lead")
        .arg("--interval-secs")
        .arg("1")
        .arg("--cooldown-secs")
        .arg("600")
        .arg("--max-iterations")
        .arg("2")
        .assert()
        .success();

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/team-lead.json");
    let content = fs::read_to_string(inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    let monitor_msgs = messages
        .iter()
        .filter(|m| m["from"].as_str() == Some("atm-monitor"))
        .count();
    assert_eq!(
        monitor_msgs, 1,
        "duplicate critical finding should be suppressed within cooldown"
    );
}
