use assert_cmd::cargo;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn init_cmd<'a>(home: &'a TempDir, repo: &'a Path) -> assert_cmd::Command {
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", home.path())
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .env_remove("ATM_CONFIG")
        .current_dir(repo);
    cmd
}

fn count_nested_command_in_hooks(
    settings_path: &Path,
    hook_category: &str,
    command: &str,
) -> usize {
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(settings_path).unwrap()).unwrap();

    parsed["hooks"][hook_category]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| {
                    entry
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .map(|hooks| {
                            hooks
                                .iter()
                                .any(|h| h.get("command").and_then(|c| c.as_str()) == Some(command))
                        })
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

fn count_flat_command_entries(settings_path: &Path, hook_category: &str, command: &str) -> usize {
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(settings_path).unwrap()).unwrap();

    parsed["hooks"][hook_category]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.get("command").and_then(|c| c.as_str()) == Some(command))
                .count()
        })
        .unwrap_or(0)
}

fn count_matcher_command_entries(
    settings_path: &Path,
    hook_category: &str,
    matcher: &str,
    command: &str,
) -> usize {
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(settings_path).unwrap()).unwrap();

    parsed["hooks"][hook_category]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.get("matcher").and_then(|m| m.as_str()) == Some(matcher))
                .map(|entry| {
                    entry
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .map(|hooks| {
                            hooks
                                .iter()
                                .filter(|h| {
                                    h.get("command").and_then(|c| c.as_str()) == Some(command)
                                })
                                .count()
                        })
                        .unwrap_or(0)
                })
                .sum()
        })
        .unwrap_or(0)
}

#[test]
fn test_init_fresh_repo_creates_atm_toml_team_and_global_hooks() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hook scope: global"));

    assert!(repo.join(".atm.toml").exists());
    assert!(home.path().join(".claude/settings.json").exists());
    assert!(
        home.path()
            .join(".claude/teams/my-team/config.json")
            .exists()
    );
}

#[test]
fn test_init_is_idempotent_on_rerun() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success();

    let settings_path = home.path().join(".claude/settings.json");
    let first = fs::read_to_string(&settings_path).unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already configured"))
        .stdout(predicate::str::contains(".atm.toml already present"))
        .stdout(predicate::str::contains("Team 'my-team' already exists"));

    let second = fs::read_to_string(&settings_path).unwrap();
    assert_eq!(first, second, "settings should be unchanged on rerun");

    let scripts_dir = home.path().join(".claude/scripts");
    let session_start_py = scripts_dir
        .join("session-start.py")
        .to_string_lossy()
        .replace('\\', "/");
    let session_start_cmd = format!(
        "bash -c 'test -f \"${{CLAUDE_PROJECT_DIR}}/.atm.toml\" && python3 \"{session_start_py}\" || true'"
    );
    let session_end_py = scripts_dir
        .join("session-end.py")
        .to_string_lossy()
        .replace('\\', "/");
    let session_end_cmd = format!(
        "bash -c 'test -f \"${{CLAUDE_PROJECT_DIR}}/.atm.toml\" && python3 \"{session_end_py}\" || true'"
    );
    assert_eq!(
        count_nested_command_in_hooks(&settings_path, "SessionStart", &session_start_cmd),
        1,
        "SessionStart hook should not be duplicated"
    );
    assert_eq!(
        count_nested_command_in_hooks(&settings_path, "SessionEnd", &session_end_cmd),
        1,
        "SessionEnd hook should not be duplicated"
    );
    assert_eq!(
        count_flat_command_entries(&settings_path, "SessionStart", &session_start_cmd),
        0,
        "SessionStart must not use flat legacy command entries"
    );
    assert_eq!(
        count_flat_command_entries(&settings_path, "SessionEnd", &session_end_cmd),
        0,
        "SessionEnd must not use flat legacy command entries"
    );
}

#[test]
fn test_init_local_writes_project_settings_only() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team", "--local"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hook scope: local"));

    assert!(repo.join(".claude/settings.json").exists());
    assert!(
        !home.path().join(".claude/settings.json").exists(),
        "global settings should not be created with --local"
    );
}

#[test]
fn test_init_skip_team_does_not_create_team_config() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    init_cmd(&home, &repo)
        .args(["init", "join-team", "--skip-team"])
        .assert()
        .success();

    assert!(repo.join(".atm.toml").exists());
    assert!(
        !home
            .path()
            .join(".claude/teams/join-team/config.json")
            .exists(),
        "team config should be skipped when --skip-team is set"
    );
}

/// Partial-state case: .atm.toml already present, hooks not installed.
/// init should install hooks and create team without touching .atm.toml.
#[test]
fn test_init_with_existing_atm_toml_installs_hooks_and_team() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    // Pre-create .atm.toml with custom identity
    fs::write(
        repo.join(".atm.toml"),
        "[core]\ndefault_team = \"my-team\"\nidentity = \"custom-identity\"\n",
    )
    .unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success();

    // Hooks should be installed
    assert!(home.path().join(".claude/settings.json").exists());
    // Team should be created
    assert!(
        home.path()
            .join(".claude/teams/my-team/config.json")
            .exists()
    );
    // .atm.toml should be unchanged (custom identity preserved)
    let toml = fs::read_to_string(repo.join(".atm.toml")).unwrap();
    assert!(
        toml.contains("identity = \"custom-identity\""),
        ".atm.toml must not be overwritten when already present"
    );
}

/// Partial-state case: hooks already installed, .atm.toml missing.
/// init should create .atm.toml and team without duplicating hooks.
#[test]
fn test_init_with_existing_hooks_creates_atm_toml_without_duplicating_hooks() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    // First run installs everything
    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success();

    // Simulate "hooks present, no .atm.toml" by removing .atm.toml
    fs::remove_file(repo.join(".atm.toml")).unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success();

    // .atm.toml should be recreated
    assert!(repo.join(".atm.toml").exists());
    // Hooks must not be duplicated
    let settings_path = home.path().join(".claude/settings.json");
    let scripts_dir = home.path().join(".claude/scripts");
    let session_start_py = scripts_dir
        .join("session-start.py")
        .to_string_lossy()
        .replace('\\', "/");
    let session_start_cmd = format!(
        "bash -c 'test -f \"${{CLAUDE_PROJECT_DIR}}/.atm.toml\" && python3 \"{session_start_py}\" || true'"
    );
    let session_end_py = scripts_dir
        .join("session-end.py")
        .to_string_lossy()
        .replace('\\', "/");
    let session_end_cmd = format!(
        "bash -c 'test -f \"${{CLAUDE_PROJECT_DIR}}/.atm.toml\" && python3 \"{session_end_py}\" || true'"
    );
    assert_eq!(
        count_nested_command_in_hooks(&settings_path, "SessionStart", &session_start_cmd),
        1,
        "SessionStart hook must not be duplicated when hooks pre-exist"
    );
    assert_eq!(
        count_nested_command_in_hooks(&settings_path, "SessionEnd", &session_end_cmd),
        1,
        "SessionEnd hook must not be duplicated when hooks pre-exist"
    );
    assert_eq!(
        count_flat_command_entries(&settings_path, "SessionStart", &session_start_cmd),
        0,
        "SessionStart must not use flat legacy command entries"
    );
    assert_eq!(
        count_flat_command_entries(&settings_path, "SessionEnd", &session_end_cmd),
        0,
        "SessionEnd must not use flat legacy command entries"
    );
}

#[test]
fn test_init_session_hooks_use_nested_schema_only() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success();

    let settings_path = home.path().join(".claude/settings.json");
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();

    for category in ["SessionStart", "SessionEnd"] {
        let entries = parsed["hooks"][category]
            .as_array()
            .expect("session hook category should be an array");
        assert!(!entries.is_empty(), "{category} should not be empty");
        for entry in entries {
            assert!(
                entry.get("hooks").and_then(|h| h.as_array()).is_some(),
                "{category} entries must use nested hooks schema"
            );
            assert!(
                entry.get("command").is_none(),
                "{category} entries must not use flat legacy command schema"
            );
        }
    }
}

#[test]
fn test_init_identity_flag_writes_identity_to_atm_toml() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team", "--identity", "arch-ctm"])
        .assert()
        .success();

    let atm_toml = fs::read_to_string(repo.join(".atm.toml")).unwrap();
    assert!(atm_toml.contains("default_team = \"my-team\""));
    assert!(atm_toml.contains("identity = \"arch-ctm\""));
}

#[test]
fn test_init_global_relay_hook_paths_are_absolute() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success();

    let settings_path = home.path().join(".claude/settings.json");
    let scripts_dir = home.path().join(".claude/scripts");
    let permission_py = scripts_dir
        .join("permission-request-relay.py")
        .to_string_lossy()
        .replace('\\', "/");
    let stop_py = scripts_dir
        .join("stop-relay.py")
        .to_string_lossy()
        .replace('\\', "/");
    let notify_py = scripts_dir
        .join("notification-idle-relay.py")
        .to_string_lossy()
        .replace('\\', "/");

    let permission_cmd = format!(
        "bash -c 'test -f \"${{CLAUDE_PROJECT_DIR}}/.atm.toml\" && python3 \"{permission_py}\" || true'"
    );
    let stop_cmd = format!(
        "bash -c 'test -f \"${{CLAUDE_PROJECT_DIR}}/.atm.toml\" && python3 \"{stop_py}\" || true'"
    );
    let notify_cmd = format!(
        "bash -c 'test -f \"${{CLAUDE_PROJECT_DIR}}/.atm.toml\" && python3 \"{notify_py}\" || true'"
    );

    assert_eq!(
        count_nested_command_in_hooks(&settings_path, "PermissionRequest", &permission_cmd),
        1
    );
    assert_eq!(
        count_nested_command_in_hooks(&settings_path, "Stop", &stop_cmd),
        1
    );
    assert_eq!(
        count_matcher_command_entries(&settings_path, "Notification", "idle_prompt", &notify_cmd),
        1
    );

    let settings = fs::read_to_string(&settings_path).unwrap();
    assert!(
        !settings.contains("${CLAUDE_PROJECT_DIR}/.claude/scripts/"),
        "global install must not persist project-local relay script paths"
    );
    assert!(
        settings.contains(&permission_py),
        "global install should persist absolute permission relay path"
    );
    assert!(
        settings.contains(&stop_py),
        "global install should persist absolute stop relay path"
    );
    assert!(
        settings.contains(&notify_py),
        "global install should persist absolute notification relay path"
    );
}

#[test]
fn test_init_local_relay_hook_paths_use_claude_project_dir() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team", "--local"])
        .assert()
        .success();

    let settings_path = repo.join(".claude/settings.json");
    let permission_cmd = "bash -c 'test -f \"${CLAUDE_PROJECT_DIR}/.atm.toml\" && python3 \"${CLAUDE_PROJECT_DIR}/.claude/scripts/permission-request-relay.py\" || true'";
    let stop_cmd = "bash -c 'test -f \"${CLAUDE_PROJECT_DIR}/.atm.toml\" && python3 \"${CLAUDE_PROJECT_DIR}/.claude/scripts/stop-relay.py\" || true'";
    let notify_cmd = "bash -c 'test -f \"${CLAUDE_PROJECT_DIR}/.atm.toml\" && python3 \"${CLAUDE_PROJECT_DIR}/.claude/scripts/notification-idle-relay.py\" || true'";

    assert_eq!(
        count_nested_command_in_hooks(&settings_path, "PermissionRequest", permission_cmd),
        1
    );
    assert_eq!(
        count_nested_command_in_hooks(&settings_path, "Stop", stop_cmd),
        1
    );
    assert_eq!(
        count_matcher_command_entries(&settings_path, "Notification", "idle_prompt", notify_cmd),
        1
    );

    let settings = fs::read_to_string(&settings_path).unwrap();
    assert!(settings.contains("${CLAUDE_PROJECT_DIR}/.claude/scripts/permission-request-relay.py"));
    assert!(settings.contains("${CLAUDE_PROJECT_DIR}/.claude/scripts/stop-relay.py"));
    assert!(settings.contains("${CLAUDE_PROJECT_DIR}/.claude/scripts/notification-idle-relay.py"));
    assert!(
        !settings.contains(
            home.path()
                .join(".claude/scripts")
                .to_string_lossy()
                .as_ref()
        ),
        "local install must not embed absolute per-user script paths"
    );
}

#[test]
fn test_init_preserves_existing_non_atm_hooks_integration() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let claude_dir = home.path().join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();

    let keep_cmd = "bash -lc 'echo keep-non-atm-hook'";
    let initial_settings = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "custom_event",
                "hooks": [{
                    "type": "command",
                    "command": keep_cmd
                }]
            }]
        }
    });
    fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&initial_settings).unwrap(),
    )
    .unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success();

    let settings_path = claude_dir.join("settings.json");
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    let session_start = parsed["hooks"]["SessionStart"]
        .as_array()
        .expect("SessionStart hooks should be an array");

    let preserved = session_start.iter().any(|entry| {
        entry.get("matcher").and_then(|v| v.as_str()) == Some("custom_event")
            && entry
                .get("hooks")
                .and_then(|v| v.as_array())
                .is_some_and(|hooks| {
                    hooks
                        .iter()
                        .any(|hook| hook.get("command").and_then(|c| c.as_str()) == Some(keep_cmd))
                })
    });
    assert!(preserved, "existing non-ATM hook should be preserved");
}
