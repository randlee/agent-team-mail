use assert_cmd::cargo;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn init_cmd<'a>(home: &'a TempDir, repo: &'a Path) -> assert_cmd::Command {
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", home.path())
        .envs([("HOME", home.path())])
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env("ATM_HOOK_PYTHON", "python3")
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

fn count_gemini_hook_command(settings_path: &Path, category: &str, command: &str) -> usize {
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(settings_path).unwrap()).unwrap();
    parsed["hooks"][category]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| {
                    entry
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .is_some_and(|hooks| {
                            hooks.iter().any(|hook| {
                                hook.get("command").and_then(|c| c.as_str()) == Some(command)
                            })
                        })
                })
                .count()
        })
        .unwrap_or(0)
}

fn normalize_runtime_path(path: &str) -> String {
    #[cfg(windows)]
    {
        path.replace('\\', "/").to_lowercase()
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

// Windows: dirs::home_dir() uses the registry profile path, not the HOME
// env var, so HOME-based team-config isolation does not work on Windows.
// The tested logic is platform-independent; only the test setup is not.
#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
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
    assert!(repo.join(".prompts").exists());
    let gitignore = fs::read_to_string(repo.join(".gitignore")).unwrap();
    assert!(
        gitignore.lines().any(|line| line.trim() == ".prompts/"),
        "init should add .prompts/ to .gitignore"
    );
    assert!(home.path().join(".claude/settings.json").exists());
    assert!(
        home.path()
            .join(".claude/teams/my-team/config.json")
            .exists()
    );
}

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
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
    let gitignore = fs::read_to_string(repo.join(".gitignore")).unwrap();
    let prompts_lines = gitignore
        .lines()
        .filter(|line| line.trim() == ".prompts/")
        .count();
    assert_eq!(
        prompts_lines, 1,
        "init rerun should not duplicate .prompts/ entry in .gitignore"
    );

    let scripts_dir = home.path().join(".claude/scripts");
    let session_start_py = scripts_dir
        .join("session-start.py")
        .to_string_lossy()
        .replace('\\', "/");
    let session_start_cmd = format!("python3 \"{session_start_py}\"");
    let session_end_py = scripts_dir
        .join("session-end.py")
        .to_string_lossy()
        .replace('\\', "/");
    let session_end_cmd = format!("python3 \"{session_end_py}\"");
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

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
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

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
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
#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
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
#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
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
    let session_start_cmd = format!("python3 \"{session_start_py}\"");
    let session_end_py = scripts_dir
        .join("session-end.py")
        .to_string_lossy()
        .replace('\\', "/");
    let session_end_cmd = format!("python3 \"{session_end_py}\"");
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

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
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

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
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

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
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

    let permission_cmd = format!("python3 \"{permission_py}\"");
    let stop_cmd = format!("python3 \"{stop_py}\"");
    let notify_cmd = format!("python3 \"{notify_py}\"");

    assert_eq!(
        count_nested_command_in_hooks(&settings_path, "PermissionRequest", &permission_cmd),
        1
    );
    assert_eq!(
        count_nested_command_in_hooks(&settings_path, "Stop", &stop_cmd),
        1
    );
    assert_eq!(
        count_matcher_command_entries(&settings_path, "Notification", "idle_prompt", &notify_cmd,),
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

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
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
    let permission_cmd =
        "python3 \"${CLAUDE_PROJECT_DIR}/.claude/scripts/permission-request-relay.py\"";
    let stop_cmd = "python3 \"${CLAUDE_PROJECT_DIR}/.claude/scripts/stop-relay.py\"";
    let notify_cmd = "python3 \"${CLAUDE_PROJECT_DIR}/.claude/scripts/notification-idle-relay.py\"";

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

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
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

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
#[test]
fn test_init_installs_codex_notify_when_detected_by_config() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let codex_dir = home.path().join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(codex_dir.join("config.toml"), "model = \"gpt-5\"\n").unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success()
        .stdout(predicate::str::contains("codex: updated"));

    let cfg = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
    let parsed: toml::Value = cfg.parse().unwrap();
    let notify = parsed
        .get("notify")
        .and_then(|v| v.as_array())
        .expect("notify array");
    assert_eq!(notify.len(), 2);
    assert_eq!(notify[0].as_str(), Some("python3"));
    let relay = home
        .path()
        .join(".claude/scripts/atm-hook-relay.py")
        .to_string_lossy()
        .to_string();
    let actual = notify[1].as_str().expect("notify relay path");
    assert_eq!(
        normalize_runtime_path(actual),
        normalize_runtime_path(&relay)
    );
}

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
#[test]
fn test_init_reports_codex_already_configured_on_second_run() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let codex_dir = home.path().join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(codex_dir.join("config.toml"), "model = \"gpt-5\"\n").unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success()
        .stdout(predicate::str::contains("codex: updated"));

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success()
        .stdout(predicate::str::contains("codex: already-configured"));
}

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
#[test]
fn test_init_dry_run_shows_planned_actions_without_writes() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run: no files were written."))
        .stdout(predicate::str::contains("Would create .atm.toml"))
        .stdout(predicate::str::contains("Would create team 'my-team'"))
        .stdout(predicate::str::contains("claude: would-install"))
        .stdout(predicate::str::contains("codex:"))
        .stdout(predicate::str::contains("gemini:"));

    assert!(
        !repo.join(".atm.toml").exists(),
        "dry-run must not write .atm.toml"
    );
    assert!(
        !home.path().join(".claude/settings.json").exists(),
        "dry-run must not write settings.json"
    );
    assert!(
        !home
            .path()
            .join(".claude/teams/my-team/config.json")
            .exists(),
        "dry-run must not create team config"
    );
    assert!(
        !repo.join(".prompts").exists(),
        "dry-run must not create .prompts directory"
    );
    assert!(
        !repo.join(".gitignore").exists(),
        "dry-run must not create .gitignore for compose bootstrap"
    );
}

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
#[test]
fn test_init_reports_codex_notify_conflict_without_failing_command() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let codex_dir = home.path().join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(
        codex_dir.join("config.toml"),
        "notify = [\"echo\", \"hello\"]\n",
    )
    .unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success()
        .stdout(predicate::str::contains("codex: error"))
        .stdout(predicate::str::contains(
            "Detected existing Codex notify configuration",
        ));
}

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
#[test]
fn test_init_installs_gemini_hooks_when_detected_by_config_dir() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(home.path().join(".gemini")).unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gemini: installed"));

    let settings_path = home.path().join(".gemini/settings.json");
    assert!(settings_path.exists(), "gemini settings should be created");

    let scripts_dir = home.path().join(".claude/scripts");
    let session_start = format!(
        "python3 \"{}\"",
        scripts_dir
            .join("session-start.py")
            .to_string_lossy()
            .replace('\\', "/")
    );
    let session_end = format!(
        "python3 \"{}\"",
        scripts_dir
            .join("session-end.py")
            .to_string_lossy()
            .replace('\\', "/")
    );
    let after_agent = format!(
        "python3 \"{}\"",
        scripts_dir
            .join("teammate-idle-relay.py")
            .to_string_lossy()
            .replace('\\', "/")
    );

    assert_eq!(
        count_gemini_hook_command(&settings_path, "SessionStart", &session_start),
        1
    );
    assert_eq!(
        count_gemini_hook_command(&settings_path, "SessionEnd", &session_end),
        1
    );
    assert_eq!(
        count_gemini_hook_command(&settings_path, "AfterAgent", &after_agent),
        1
    );
}

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
#[test]
fn test_init_gemini_hook_install_is_idempotent() {
    let home = TempDir::new().unwrap();
    let repo = home.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(home.path().join(".gemini")).unwrap();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success();

    init_cmd(&home, &repo)
        .args(["init", "my-team"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gemini: already-configured"));
}
