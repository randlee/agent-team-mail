use assert_cmd::cargo;
use std::fs;
use tempfile::TempDir;

fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    let runtime_home = temp_dir.path().join("runtime-home");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&runtime_home).unwrap();
    cmd.env("ATM_HOME", &runtime_home)
        .envs([("HOME", temp_dir.path())])
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

fn write_recent_seen_state(home: &TempDir, team: &str, agent: &str) {
    let state_path = home.path().join(".config/atm/state.json");
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let state = serde_json::json!({
        "last_seen": {
            team: {
                agent: "2099-01-01T00:00:00Z"
            }
        }
    });
    fs::write(state_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();
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
    assert!(stdout.contains("forced-cleanup"));
    assert!(stdout.contains("stale-session-metadata"));
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
fn test_teams_cleanup_dry_run_includes_team_lead_protected_row_for_full_team() {
    let temp_dir = TempDir::new().unwrap();
    write_team_config(&temp_dir, "atm-dev", false);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args(["teams", "cleanup", "atm-dev", "--dry-run"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Cleanup preview for team atm-dev:"));
    assert!(stdout.contains("team-lead"));
    assert!(stdout.contains("skip"));
    assert!(stdout.contains("team-lead-protected"));
    assert!(!stdout.contains("Nothing to clean up for team atm-dev."));
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
    assert!(!stdout.contains("session-prune  stale-session-metadata"));
}

#[test]
fn test_teams_cleanup_dry_run_lists_skipped_external_agent_without_session_id() {
    let temp_dir = TempDir::new().unwrap();
    write_team_config(&temp_dir, "atm-dev", true);
    write_recent_seen_state(&temp_dir, "atm-dev", "publisher");

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
    assert!(stdout.contains("external-agent-liveness-unknown"));
}

#[test]
fn test_teams_cleanup_dry_run_treats_codex_agent_type_as_external_for_skip_preview() {
    let temp_dir = TempDir::new().unwrap();
    write_team_config(&temp_dir, "atm-dev", true);
    write_recent_seen_state(&temp_dir, "atm-dev", "publisher");

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
    assert!(stdout.contains("external-agent-no-state"));
}

#[test]
fn test_teams_cleanup_dry_run_totals_match_actual_cleanup_force() {
    let temp_dir = TempDir::new().unwrap();
    write_team_config(&temp_dir, "atm-dev", true);

    // Dry-run preview should predict exactly one member cleanup (publisher):
    // roster-remove + mailbox-delete + session-prune.
    let mut dry = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut dry, &temp_dir);
    let dry_assert = dry
        .args(["teams", "cleanup", "atm-dev", "--dry-run", "--force"])
        .assert()
        .success();
    let dry_stdout = String::from_utf8(dry_assert.get_output().stdout.clone()).unwrap();
    assert!(dry_stdout.contains("roster-remove: 1"));
    assert!(dry_stdout.contains("mailbox-delete: 1"));
    assert!(dry_stdout.contains("session-prune: 1"));
    assert!(dry_stdout.contains("skip: 1"));

    // Actual cleanup should remove that same member + inbox artifact.
    let mut apply = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut apply, &temp_dir);
    let apply_assert = apply
        .args(["teams", "cleanup", "atm-dev", "--force"])
        .assert()
        .success();
    let apply_stdout = String::from_utf8(apply_assert.get_output().stdout.clone()).unwrap();
    assert!(apply_stdout.contains("Removed 1 stale member(s): publisher"));

    let config_path = temp_dir.path().join(".claude/teams/atm-dev/config.json");
    let config_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(config_path).unwrap()).unwrap();
    let names: Vec<String> = config_json["members"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m.get("name").and_then(serde_json::Value::as_str))
        .map(ToString::to_string)
        .collect();
    assert_eq!(names, vec!["team-lead".to_string()]);
    assert!(
        !temp_dir
            .path()
            .join(".claude/teams/atm-dev/inboxes/publisher.json")
            .exists(),
        "publisher inbox must be removed during real cleanup"
    );
}
