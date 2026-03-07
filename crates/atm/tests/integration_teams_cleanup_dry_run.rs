use assert_cmd::cargo;
use std::fs;
use tempfile::TempDir;

fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env_remove("ATM_CONFIG")
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .current_dir(&workdir);
}

fn write_team_config(home: &TempDir, team: &str, include_publisher: bool) {
    let team_dir = home.path().join(".claude/teams").join(team);
    fs::create_dir_all(team_dir.join("inboxes")).unwrap();
    fs::create_dir_all(team_dir.join("mailboxes")).unwrap();

    let mut members = vec![serde_json::json!({
        "agentId": format!("team-lead@{team}"),
        "name": "team-lead",
        "agentType": "general-purpose",
        "model": "unknown",
        "joinedAt": 1739284800000u64,
        "tmuxPaneId": "",
        "cwd": ".",
        "subscriptions": []
    })];
    if include_publisher {
        members.push(serde_json::json!({
            "agentId": format!("publisher@{team}"),
            "name": "publisher",
            "agentType": "codex",
            "model": "unknown",
            "joinedAt": 1739284800000u64,
            "tmuxPaneId": "",
            "cwd": ".",
            "subscriptions": [],
            "sessionId": "publisher-session"
        }));
        fs::write(team_dir.join("inboxes/publisher.json"), "[]").unwrap();
    }

    let config = serde_json::json!({
        "name": team,
        "createdAt": 1739284800000u64,
        "leadAgentId": format!("team-lead@{team}"),
        "leadSessionId": "lead-sess",
        "members": members
    });
    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
}

#[test]
fn test_teams_cleanup_dry_run_preview_table_and_no_mutation() {
    let temp_dir = TempDir::new().unwrap();
    write_team_config(&temp_dir, "atm-dev", true);

    let config_before =
        fs::read_to_string(temp_dir.path().join(".claude/teams/atm-dev/config.json")).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args(["teams", "cleanup", "atm-dev", "--dry-run", "--force"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Cleanup preview for team atm-dev:"));
    assert!(stdout.contains("Reason"));
    assert!(stdout.contains("roster-remove"));
    assert!(stdout.contains("mailbox-delete"));
    assert!(stdout.contains("session-prune"));
    assert!(stdout.contains("forced cleanup (--force)"));
    assert!(stdout.contains("stale session metadata"));
    assert!(stdout.contains("Totals:"));

    let config_after =
        fs::read_to_string(temp_dir.path().join(".claude/teams/atm-dev/config.json")).unwrap();
    assert_eq!(
        config_before, config_after,
        "dry-run must not mutate config"
    );
    assert!(
        temp_dir
            .path()
            .join(".claude/teams/atm-dev/inboxes/publisher.json")
            .exists(),
        "dry-run must not remove inbox files"
    );
}

#[test]
fn test_teams_cleanup_dry_run_empty_uses_exact_noop_message() {
    let temp_dir = TempDir::new().unwrap();
    write_team_config(&temp_dir, "atm-dev", false);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args(["teams", "cleanup", "atm-dev", "--dry-run"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Nothing to clean up for team atm-dev."));
    assert!(
        !stdout.contains("Agent  Action"),
        "no table header for empty dry-run"
    );
}

#[test]
fn test_teams_cleanup_noop_uses_exact_message() {
    let temp_dir = TempDir::new().unwrap();
    write_team_config(&temp_dir, "atm-dev", false);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd.args(["teams", "cleanup", "atm-dev"]).assert().success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Nothing to clean up for team atm-dev."));
}

#[test]
fn test_teams_cleanup_dry_run_suppresses_session_prune_without_session_id() {
    let temp_dir = TempDir::new().unwrap();
    write_team_config(&temp_dir, "atm-dev", true);

    // Remove sessionId from publisher to verify session-prune preview suppression.
    let config_path = temp_dir.path().join(".claude/teams/atm-dev/config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    if let Some(members) = config
        .get_mut("members")
        .and_then(serde_json::Value::as_array_mut)
    {
        if let Some(publisher) = members
            .iter_mut()
            .find(|m| m.get("name").and_then(serde_json::Value::as_str) == Some("publisher"))
            && let Some(obj) = publisher.as_object_mut()
        {
            obj.remove("sessionId");
        }
    }
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args(["teams", "cleanup", "atm-dev", "--dry-run", "--force"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("roster-remove"));
    assert!(stdout.contains("mailbox-delete"));
    assert!(!stdout.contains("session-prune  stale session metadata"));
}

#[test]
fn test_teams_cleanup_dry_run_lists_skipped_external_agent_without_session_id() {
    let temp_dir = TempDir::new().unwrap();
    write_team_config(&temp_dir, "atm-dev", true);

    // Mark publisher as external and remove sessionId so cleanup must skip it.
    let config_path = temp_dir.path().join(".claude/teams/atm-dev/config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    if let Some(members) = config
        .get_mut("members")
        .and_then(serde_json::Value::as_array_mut)
        && let Some(publisher) = members
            .iter_mut()
            .find(|m| m.get("name").and_then(serde_json::Value::as_str) == Some("publisher"))
        && let Some(obj) = publisher.as_object_mut()
    {
        obj.remove("sessionId");
        obj.insert(
            "externalBackendType".to_string(),
            serde_json::Value::String("codex".to_string()),
        );
    }
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args(["teams", "cleanup", "atm-dev", "publisher", "--dry-run"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Cleanup preview for team atm-dev:"));
    assert!(stdout.contains("publisher"));
    assert!(stdout.contains("skip"));
    assert!(stdout.contains("external-agent-no-state"));
}

#[test]
fn test_teams_cleanup_dry_run_treats_codex_agent_type_as_external_for_skip_preview() {
    let temp_dir = TempDir::new().unwrap();
    write_team_config(&temp_dir, "atm-dev", true);

    // Simulate legacy roster entry: codex agentType + sessionId, but no externalBackendType.
    let config_path = temp_dir.path().join(".claude/teams/atm-dev/config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    if let Some(members) = config
        .get_mut("members")
        .and_then(serde_json::Value::as_array_mut)
        && let Some(publisher) = members
            .iter_mut()
            .find(|m| m.get("name").and_then(serde_json::Value::as_str) == Some("publisher"))
        && let Some(obj) = publisher.as_object_mut()
    {
        obj.insert(
            "agentType".to_string(),
            serde_json::Value::String("codex".to_string()),
        );
        obj.insert(
            "sessionId".to_string(),
            serde_json::Value::String("publisher-session".to_string()),
        );
        obj.remove("externalBackendType");
    }
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args(["teams", "cleanup", "atm-dev", "--dry-run"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("publisher"));
    assert!(stdout.contains("skip"));
    assert!(stdout.contains("external agent liveness unknown"));
}
