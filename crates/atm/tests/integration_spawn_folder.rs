use assert_cmd::cargo;
use predicates::prelude::*;
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

#[test]
fn test_spawn_folder_rejects_nonexistent_directory() {
    let temp_dir = TempDir::new().unwrap();
    let missing = temp_dir.path().join("missing");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.args([
        "teams",
        "spawn",
        "agent-a",
        "--team",
        "atm-dev",
        "--runtime",
        "codex",
        "--folder",
        missing.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("--folder path"))
    .stderr(predicate::str::contains("does not exist"));
}

#[test]
fn test_spawn_folder_and_cwd_mismatch_rejected() {
    let temp_dir = TempDir::new().unwrap();
    let folder_a = temp_dir.path().join("a");
    let folder_b = temp_dir.path().join("b");
    fs::create_dir_all(&folder_a).unwrap();
    fs::create_dir_all(&folder_b).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.args([
        "teams",
        "spawn",
        "agent-b",
        "--team",
        "atm-dev",
        "--runtime",
        "gemini",
        "--folder",
        folder_a.to_str().unwrap(),
        "--cwd",
        folder_b.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("resolve to different directories"));
}
