use assert_cmd::cargo;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn init_cmd<'a>(home: &'a TempDir, repo: &'a Path) -> assert_cmd::Command {
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", home.path())
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .env_remove("ATM_CONFIG")
        .current_dir(repo);
    cmd
}

fn count_command_in_hooks(settings_path: &Path, hook_category: &str, command: &str) -> usize {
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(settings_path).unwrap()).unwrap();

    match hook_category {
        "SessionStart" => parsed["hooks"]["SessionStart"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter(|h| h["command"].as_str() == Some(command))
            .count(),
        _ => 0,
    }
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
        .success();

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
    assert_eq!(
        count_command_in_hooks(&settings_path, "SessionStart", &session_start_cmd),
        1,
        "SessionStart hook should not be duplicated"
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
    assert_eq!(
        count_command_in_hooks(&settings_path, "SessionStart", &session_start_cmd),
        1,
        "SessionStart hook must not be duplicated when hooks pre-exist"
    );
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
