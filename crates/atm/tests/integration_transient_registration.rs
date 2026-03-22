use assert_cmd::cargo;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn configure_cmd(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    let runtime_home = temp_dir.path().join("runtime-home");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&runtime_home).unwrap();
    cmd.env("ATM_HOME", &runtime_home)
        .envs([("HOME", temp_dir.path())])
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .env_remove("ATM_CONFIG")
        .env_remove("CLAUDE_SESSION_ID")
        .current_dir(&workdir);
}

fn write_team_config(temp_dir: &TempDir, team: &str, members: &[&str]) -> std::path::PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team);
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    let members_json: Vec<serde_json::Value> = members
        .iter()
        .map(|name| {
            serde_json::json!({
                "agentId": format!("{name}@{team}"),
                "name": name,
                "agentType": "general-purpose",
                "model": "unknown",
                "joinedAt": 1739284800000u64,
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": []
            })
        })
        .collect();

    let config = serde_json::json!({
        "name": team,
        "description": "test team",
        "createdAt": 1739284800000u64,
        "leadAgentId": format!("team-lead@{team}"),
        "leadSessionId": "lead-sess",
        "members": members_json
    });
    let config_path = team_dir.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
    for name in members {
        fs::write(inboxes_dir.join(format!("{name}.json")), "[]").unwrap();
    }
    config_path
}

fn member_names(config_path: &std::path::Path) -> Vec<String> {
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(config_path).unwrap()).unwrap();
    value["members"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|member| member["name"].as_str().map(str::to_string))
        .collect()
}

fn assert_transient_absent_from_discovery_views(
    temp_dir: &TempDir,
    team: &str,
    transient_identity: &str,
) {
    let mut members_cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut members_cmd, temp_dir);
    let members_output = members_cmd
        .args(["members", "--team", team])
        .output()
        .expect("members command should run");
    assert!(
        members_output.status.success(),
        "members command failed: {}",
        String::from_utf8_lossy(&members_output.stderr)
    );
    let members_stdout = String::from_utf8_lossy(&members_output.stdout);
    assert!(
        !members_stdout.contains(transient_identity),
        "transient identity unexpectedly appeared in members output"
    );

    let mut status_cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut status_cmd, temp_dir);
    let status_output = status_cmd
        .args(["status", "--team", team])
        .output()
        .expect("status command should run");
    assert!(
        status_output.status.success(),
        "status command failed: {}",
        String::from_utf8_lossy(&status_output.stderr)
    );
    let status_stdout = String::from_utf8_lossy(&status_output.stdout);
    assert!(
        !status_stdout.contains(transient_identity),
        "transient identity unexpectedly appeared in status output"
    );

    let mut doctor_cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut doctor_cmd, temp_dir);
    let doctor_output = doctor_cmd
        .args(["doctor", "--team", team])
        .output()
        .expect("doctor command should run");
    let doctor_stdout = String::from_utf8_lossy(&doctor_output.stdout);
    assert!(
        !doctor_stdout.contains(transient_identity),
        "transient identity unexpectedly appeared in doctor output"
    );
}

// Windows: dirs::home_dir() uses the registry profile path, not the HOME
// env var, so HOME-based team-config isolation does not work on Windows.
// The tested logic is platform-independent; only the test setup is not.
#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
#[test]
fn test_transient_send_does_not_add_sender_to_roster() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = write_team_config(&temp_dir, "atm-dev", &["team-lead", "arch-ctm"]);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    cmd.env("ATM_IDENTITY", "transient-worker")
        .args([
            "send",
            "--team",
            "atm-dev",
            "arch-ctm",
            "hello from transient",
        ])
        .assert()
        .success();

    let names = member_names(&config_path);
    assert!(
        !names.iter().any(|name| name == "transient-worker"),
        "transient sender must not be persisted in team roster"
    );
    assert_transient_absent_from_discovery_views(&temp_dir, "atm-dev", "transient-worker");
}

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
#[test]
fn test_transient_read_does_not_add_reader_to_roster() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = write_team_config(&temp_dir, "atm-dev", &["team-lead"]);

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    cmd.args([
        "read",
        "--team",
        "atm-dev",
        "--as",
        "transient-reader",
        "--no-mark",
        "--no-update-seen",
    ])
    .assert()
    .success();

    let names = member_names(&config_path);
    assert!(
        !names.iter().any(|name| name == "transient-reader"),
        "transient reader must not be persisted in team roster"
    );
    assert_transient_absent_from_discovery_views(&temp_dir, "atm-dev", "transient-reader");
}

#[cfg_attr(
    windows,
    ignore = "Windows: dirs::home_dir() uses the registry profile path, not the HOME env var, so HOME-based team-config isolation does not work on Windows. The tested logic is platform-independent; only the test setup is not."
)]
#[test]
fn test_transient_spawn_policy_failure_does_not_mutate_roster() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = write_team_config(&temp_dir, "atm-dev", &["team-lead", "arch-ctm"]);
    let workdir = temp_dir.path().join("workdir");
    let launch_dir = temp_dir.path().join("spawn-folder");
    fs::create_dir_all(&workdir).unwrap();
    fs::create_dir_all(&launch_dir).unwrap();
    fs::write(
        workdir.join(".atm.toml"),
        r#"
[core]
default_team = "atm-dev"
identity = "team-lead"

[team."atm-dev"]
spawn_policy = "leaders-only"
co_leaders = []
"#
        .trim_start(),
    )
    .unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    configure_cmd(&mut cmd, &temp_dir);
    cmd.env("ATM_IDENTITY", "transient-worker")
        .args([
            "teams",
            "spawn",
            "transient-child",
            "--runtime",
            "codex",
            "--folder",
            launch_dir.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("SPAWN_UNAUTHORIZED"));

    let names = member_names(&config_path);
    assert!(
        !names.iter().any(|name| name == "transient-child"),
        "unauthorized transient spawn must not mutate roster"
    );

    let transient_mailbox = temp_dir
        .path()
        .join(".claude/teams/atm-dev/inboxes/transient-child.json");
    assert!(
        !transient_mailbox.exists(),
        "unauthorized transient spawn must not create mailbox artifacts"
    );
}
