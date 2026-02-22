//! Integration tests for `atm teams backup` and `atm teams restore`

use assert_cmd::cargo;
use predicates::prelude::*;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Set ATM_HOME so all commands use the temp directory.
///
/// Uses `ATM_HOME` (not `HOME` or `USERPROFILE`) for cross-platform compatibility.
/// Also removes ATM_IDENTITY and sets a clean working directory.
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).ok();
    cmd.env("ATM_HOME", temp_dir.path())
        .env_remove("ATM_IDENTITY")
        .current_dir(&workdir);
}

/// Create a tasks directory with sample task files for the given team.
fn setup_tasks(temp_dir: &TempDir, team_name: &str) {
    let tasks_dir = temp_dir.path().join(".claude/tasks").join(team_name);
    fs::create_dir_all(&tasks_dir).unwrap();
    fs::write(tasks_dir.join("task-a.json"), r#"{"id":"task-a","status":"open"}"#).unwrap();
    fs::write(tasks_dir.join("task-b.json"), r#"{"id":"task-b","status":"done"}"#).unwrap();
}

/// Create a test team with two members: team-lead and test-member.
fn setup_test_team(temp_dir: &TempDir, team_name: &str) -> PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");

    fs::create_dir_all(&inboxes_dir).unwrap();

    let config = serde_json::json!({
        "name": team_name,
        "description": "Test team",
        "createdAt": 1739284800000i64,
        "leadAgentId": format!("team-lead@{}", team_name),
        "leadSessionId": "test-session-id-original",
        "members": [
            {
                "agentId": format!("team-lead@{}", team_name),
                "name": "team-lead",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": []
            },
            {
                "agentId": format!("test-member@{}", team_name),
                "name": "test-member",
                "agentType": "general-purpose",
                "model": "claude-haiku-4-5",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%2",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": true
            }
        ]
    });

    let config_path = team_dir.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    team_dir
}

// ---------------------------------------------------------------------------
// backup tests
// ---------------------------------------------------------------------------

#[test]
fn test_backup_creates_backup() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Create an inbox for test-member
    let inbox = team_dir.join("inboxes/test-member.json");
    fs::write(&inbox, r#"[{"from":"team-lead","text":"hi","timestamp":"2026-01-01T00:00:00Z","read":false}]"#).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.args(["teams", "backup", "test-team"]);

    cmd.assert().success();

    // Verify backup directory structure
    let backups_root = temp_dir.path().join(".claude/teams/.backups/test-team");
    assert!(backups_root.exists(), "backups root should exist");

    let entries: Vec<_> = fs::read_dir(&backups_root)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1, "should have exactly one timestamped backup dir");

    let backup_dir = entries[0].path();
    assert!(backup_dir.join("config.json").exists(), "config.json should be in backup");
    assert!(
        backup_dir.join("inboxes/test-member.json").exists(),
        "inbox should be in backup"
    );
}

#[test]
fn test_backup_unknown_team_fails() {
    let temp_dir = TempDir::new().unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.args(["teams", "backup", "no-such-team"]);

    cmd.assert().failure();
}

// ---------------------------------------------------------------------------
// restore tests
// ---------------------------------------------------------------------------

#[test]
fn test_restore_round_trip() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Create inbox for test-member
    let inbox = team_dir.join("inboxes/test-member.json");
    fs::write(&inbox, r#"[{"from":"team-lead","text":"hello","timestamp":"2026-01-01T00:00:00Z","read":false}]"#).unwrap();

    // Step 1: backup
    {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.args(["teams", "backup", "test-team"]);
        cmd.assert().success();
    }

    // Step 2: remove test-member from config and delete inbox
    {
        let config_path = team_dir.join("config.json");
        let content = fs::read_to_string(&config_path).unwrap();
        let mut config: serde_json::Value = serde_json::from_str(&content).unwrap();
        if let Some(members) = config["members"].as_array_mut() {
            members.retain(|m| m["name"] != "test-member");
        }
        fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
    }
    fs::remove_file(&inbox).unwrap();

    // Step 3: restore
    {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.args(["teams", "restore", "test-team"]);
        cmd.assert().success();
    }

    // Verify test-member is back in config
    let config_path = team_dir.join("config.json");
    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    let members = config["members"].as_array().unwrap();
    assert!(
        members.iter().any(|m| m["name"] == "test-member"),
        "test-member should be restored"
    );

    // Verify inbox is back
    assert!(inbox.exists(), "test-member inbox should be restored");
}

#[test]
fn test_restore_dry_run_no_changes() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    let inbox = team_dir.join("inboxes/test-member.json");
    fs::write(&inbox, "[]").unwrap();

    // Step 1: backup
    {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.args(["teams", "backup", "test-team"]);
        cmd.assert().success();
    }

    // Step 2: remove test-member
    {
        let config_path = team_dir.join("config.json");
        let content = fs::read_to_string(&config_path).unwrap();
        let mut config: serde_json::Value = serde_json::from_str(&content).unwrap();
        if let Some(members) = config["members"].as_array_mut() {
            members.retain(|m| m["name"] != "test-member");
        }
        fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
    }
    fs::remove_file(&inbox).unwrap();

    // Step 3: dry-run restore — nothing should change
    {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.args(["teams", "restore", "test-team", "--dry-run"]);
        cmd.assert().success();
    }

    // Verify test-member was NOT restored
    let config_path = team_dir.join("config.json");
    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    let members = config["members"].as_array().unwrap();
    assert!(
        !members.iter().any(|m| m["name"] == "test-member"),
        "dry-run must not add test-member"
    );
    assert!(!inbox.exists(), "dry-run must not restore inbox file");
}

// ---------------------------------------------------------------------------
// tasks backup / restore integration tests
// ---------------------------------------------------------------------------

#[test]
fn test_backup_includes_tasks_dir() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    setup_tasks(&temp_dir, "test-team");

    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.args(["teams", "backup", "test-team"]);
    cmd.assert().success();

    // Locate the single backup directory
    let backups_root = temp_dir.path().join(".claude/teams/.backups/test-team");
    let backup_dir = fs::read_dir(&backups_root)
        .unwrap()
        .filter_map(|e| e.ok())
        .next()
        .expect("backup dir should exist")
        .path();

    let tasks_backup = backup_dir.join("tasks");
    assert!(tasks_backup.exists(), "tasks/ should be included in backup");
    assert!(
        tasks_backup.join("task-a.json").exists(),
        "task-a.json should be in backup"
    );
    assert!(
        tasks_backup.join("task-b.json").exists(),
        "task-b.json should be in backup"
    );
}

#[test]
fn test_restore_with_skip_tasks() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");
    setup_tasks(&temp_dir, "test-team");

    let inbox = team_dir.join("inboxes/test-member.json");
    fs::write(&inbox, "[]").unwrap();

    // Step 1: backup (captures tasks)
    {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.args(["teams", "backup", "test-team"]);
        cmd.assert().success();
    }

    // Step 2: remove tasks directory
    let tasks_dir = temp_dir.path().join(".claude/tasks/test-team");
    fs::remove_dir_all(&tasks_dir).unwrap();
    assert!(!tasks_dir.exists(), "tasks dir should be removed before restore");

    // Step 3: restore with --skip-tasks
    {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.args(["teams", "restore", "test-team", "--skip-tasks"]);
        cmd.assert().success();
    }

    // Tasks directory must NOT be restored
    assert!(
        !tasks_dir.exists(),
        "tasks dir should NOT be restored when --skip-tasks is passed"
    );
}

// ---------------------------------------------------------------------------
// error-path tests
// ---------------------------------------------------------------------------

#[test]
fn test_restore_from_nonexistent_path_fails() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");

    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.args(["teams", "restore", "test-team", "--from", "/nonexistent/path/that/does/not/exist"]);

    cmd.assert()
        .failure()
        .stderr(predicates::str::contains("not found").or(predicates::str::contains("No backup")));
}

#[test]
fn test_restore_from_backup_missing_tasks_dir_succeeds() {
    // Restoring from a pre-Phase-2 backup (no tasks/ dir) should not error
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "test-team");

    // Create a backup manually WITHOUT a tasks/ dir
    let backup_dir = temp_dir.path()
        .join(".claude/teams/.backups/test-team/20240101T000000Z");
    std::fs::create_dir_all(backup_dir.join("inboxes")).unwrap();
    std::fs::copy(
        team_dir.join("config.json"),
        backup_dir.join("config.json"),
    ).unwrap();

    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.args(["teams", "restore", "test-team", "--from", backup_dir.to_str().unwrap()]);

    cmd.assert().success();
}
