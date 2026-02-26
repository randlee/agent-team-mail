//! Integration tests for `atm mcp` install/uninstall/status.
//!
//! Tests isolate filesystem effects using `ATM_HOME` + a temporary workdir.

use assert_cmd::cargo;
use predicates::prelude::*;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn make_fake_binary(temp: &TempDir) -> PathBuf {
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("create bin dir");
    let bin = bin_dir.join("atm-agent-mcp");
    fs::write(&bin, b"#!/bin/sh\nexit 0\n").expect("write fake binary");
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&bin).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin, perms).expect("chmod");
    }
    bin
}

fn workdir(temp: &TempDir) -> PathBuf {
    let wd = temp.path().join("workdir");
    fs::create_dir_all(&wd).expect("create workdir");
    wd
}

fn configure_cmd(cmd: &mut assert_cmd::Command, temp: &TempDir, path: &Path, wd: &Path) {
    cmd.env("ATM_HOME", temp.path())
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .env_remove("ATM_CONFIG")
        .env("PATH", path)
        .current_dir(wd);
}

#[test]
fn test_mcp_install_claude_global_and_idempotent_reinstall() {
    let temp = TempDir::new().expect("tempdir");
    let wd = workdir(&temp);
    let fake = make_fake_binary(&temp);
    let path_env = temp.path().join("bin");

    let mut first = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut first, &temp, &path_env, &wd);
    first
        .args([
            "mcp",
            "install",
            "claude",
            "global",
            "--binary",
            fake.to_str().expect("fake path utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Installed atm MCP server for Claude Code"));

    let claude_global = temp.path().join(".claude.json");
    let content = fs::read_to_string(&claude_global).expect("claude config exists");
    assert!(
        content.contains("\"atm\""),
        "expected atm server entry in {}",
        claude_global.display()
    );

    let mut second = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut second, &temp, &path_env, &wd);
    second
        .args([
            "mcp",
            "install",
            "claude",
            "global",
            "--binary",
            fake.to_str().expect("fake path utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("already configured"));
}

#[test]
fn test_mcp_install_codex_local_scope_errors() {
    let temp = TempDir::new().expect("tempdir");
    let wd = workdir(&temp);
    let fake = make_fake_binary(&temp);
    let path_env = temp.path().join("bin");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp, &path_env, &wd);
    cmd.args([
        "mcp",
        "install",
        "codex",
        "local",
        "--binary",
        fake.to_str().expect("fake path utf-8"),
    ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("only supports global"));
}

#[test]
fn test_mcp_install_and_uninstall_gemini_local() {
    let temp = TempDir::new().expect("tempdir");
    let wd = workdir(&temp);
    let fake = make_fake_binary(&temp);
    let path_env = temp.path().join("bin");

    let mut install = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut install, &temp, &path_env, &wd);
    install
        .args([
            "mcp",
            "install",
            "gemini",
            "local",
            "--binary",
            fake.to_str().expect("fake path utf-8"),
        ])
        .assert()
        .success();

    let gemini_local = wd.join(".gemini/settings.json");
    let content = fs::read_to_string(&gemini_local).expect("gemini local config exists");
    assert!(content.contains("\"atm\""));

    let mut uninstall = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut uninstall, &temp, &path_env, &wd);
    uninstall
        .args(["mcp", "uninstall", "gemini", "local"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed atm MCP server"));
}

#[test]
fn test_mcp_uninstall_codex_not_present_is_non_fatal() {
    let temp = TempDir::new().expect("tempdir");
    let wd = workdir(&temp);
    let path_env = temp.path().join("bin");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp, &path_env, &wd);
    cmd.args(["mcp", "uninstall", "codex", "global"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not present for Codex"));
}

#[test]
fn test_mcp_status_reports_sections() {
    let temp = TempDir::new().expect("tempdir");
    let wd = workdir(&temp);
    let _fake = make_fake_binary(&temp);
    let path_env = temp.path().join("bin");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp, &path_env, &wd);
    cmd.args(["mcp", "status"]).assert().success().stdout(
        predicate::str::contains("ATM MCP Server Status")
            .and(predicate::str::contains("Claude Code"))
            .and(predicate::str::contains("Codex"))
            .and(predicate::str::contains("Gemini CLI")),
    );
}
