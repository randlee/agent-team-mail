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
    .stderr(predicate::str::contains("does not exist"));
}

#[test]
fn test_spawn_folder_rejects_existing_file_path() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("plain-file.txt");
    fs::write(&file_path, "x").unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.args([
        "teams",
        "spawn",
        "agent-file",
        "--team",
        "atm-dev",
        "--runtime",
        "codex",
        "--folder",
        file_path.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("is not a directory"));
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

#[test]
fn test_spawn_cwd_only_reaches_daemon_with_json_folder_field() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("cwd-only");
    fs::create_dir_all(&folder).unwrap();
    let canonical = fs::canonicalize(&folder).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args([
            "teams",
            "spawn",
            "agent-c",
            "--team",
            "atm-dev",
            "--runtime",
            "codex",
            "--cwd",
            folder.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not running")
    );
    assert_eq!(parsed["folder"], canonical.to_string_lossy().to_string());
}

#[test]
fn test_spawn_dual_flag_match_reaches_daemon_and_keeps_folder_json() {
    let temp_dir = TempDir::new().unwrap();
    let folder = temp_dir.path().join("dual");
    fs::create_dir_all(&folder).unwrap();
    let canonical = fs::canonicalize(&folder).unwrap();
    let alt = temp_dir.path().join("dual/.");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args([
            "teams",
            "spawn",
            "agent-d",
            "--team",
            "atm-dev",
            "--runtime",
            "gemini",
            "--folder",
            folder.to_str().unwrap(),
            "--cwd",
            alt.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not running")
    );
    assert_eq!(parsed["folder"], canonical.to_string_lossy().to_string());
}

#[test]
fn test_spawn_relative_folder_normalizes_to_absolute_in_json_output() {
    let temp_dir = TempDir::new().unwrap();
    let subdir = temp_dir.path().join("workdir").join("subdir");
    fs::create_dir_all(&subdir).unwrap();
    let canonical = fs::canonicalize(&subdir).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let assert = cmd
        .args([
            "teams",
            "spawn",
            "agent-rel",
            "--team",
            "atm-dev",
            "--runtime",
            "codex",
            "--folder",
            "./subdir",
            "--json",
        ])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        parsed["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not running")
    );
    assert_eq!(parsed["folder"], canonical.to_string_lossy().to_string());
}
