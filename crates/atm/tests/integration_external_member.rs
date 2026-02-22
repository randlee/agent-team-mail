//! Integration tests for Sprint E.6: External Agent Member Management
//!
//! Tests cover:
//! - `add-member` new flags: `--session-id`, `--backend-type`, `--model`
//! - MCP collision guard in `add-member`
//! - `update-member` subcommand
//! - `members --team` and `status --team` (--team flag fix)
//! - Empty message rejection in send
//! - Model registry validation
//! - BackendType validation including `human:<username>`
//!
//! All tests use `ATM_HOME` instead of `HOME`/`USERPROFILE` for cross-platform
//! Windows CI compatibility (see `docs/cross-platform-guidelines.md`).

use assert_cmd::cargo;
use serial_test::serial;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Set the ATM_HOME environment variable on a command, pointing at the temp dir.
///
/// Uses a subdirectory as CWD to avoid .atm.toml config leaking from the repo.
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).ok();
    cmd.env("ATM_HOME", temp_dir.path())
        .env_remove("ATM_IDENTITY")
        .current_dir(&workdir);
}

/// Create a minimal team under `{temp_dir}/.claude/teams/{team_name}`.
///
/// Returns the path to the team directory.
fn setup_test_team(temp_dir: &TempDir, team_name: &str) -> PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    let config = serde_json::json!({
        "name": team_name,
        "description": "Test team",
        "createdAt": 1739284800000i64,
        "leadAgentId": format!("team-lead@{team_name}"),
        "leadSessionId": "test-session-id",
        "members": [
            {
                "agentId": format!("team-lead@{team_name}"),
                "name": "team-lead",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": []
            }
        ]
    });

    let config_path = team_dir.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    team_dir
}

/// Read the team config and return the list of member names.
fn member_names(team_dir: &Path) -> Vec<String> {
    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(team_dir.join("config.json")).unwrap()).unwrap();
    config["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap().to_string())
        .collect()
}

/// Read the team config and find a member by name. Returns the member JSON.
fn find_member(team_dir: &Path, name: &str) -> serde_json::Value {
    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(team_dir.join("config.json")).unwrap()).unwrap();
    config["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"].as_str() == Some(name))
        .unwrap_or(&serde_json::Value::Null)
        .clone()
}

// ── Fix A: --team flag on members / status ────────────────────────────────────

#[test]
fn test_members_team_flag() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "flag-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("members")
        .arg("--team")
        .arg("flag-team")
        .assert()
        .success();
}

#[test]
fn test_status_team_flag() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "flag-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("status")
        .arg("--team")
        .arg("flag-team")
        .assert()
        .success();
}

// ── Fix B: Empty message rejection ──────────────────────────────────────────

#[test]
#[serial]
fn test_send_empty_message_rejected() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "send-test");

    // Add a target agent so the team member lookup succeeds
    let config_path = team_dir.join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    config["members"].as_array_mut().unwrap().push(serde_json::json!({
        "agentId": "target@send-test",
        "name": "target",
        "agentType": "general-purpose",
        "model": "unknown",
        "joinedAt": 1739284800000i64,
        "cwd": "/tmp",
        "subscriptions": []
    }));
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "send-test")
        .arg("send")
        .arg("target")
        .arg("   ") // whitespace-only message
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be empty"));
}

#[test]
#[serial]
fn test_send_empty_string_message_rejected() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "send-test2");

    let config_path = team_dir.join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    config["members"].as_array_mut().unwrap().push(serde_json::json!({
        "agentId": "target@send-test2",
        "name": "target",
        "agentType": "general-purpose",
        "model": "unknown",
        "joinedAt": 1739284800000i64,
        "cwd": "/tmp",
        "subscriptions": []
    }));
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "send-test2")
        .arg("send")
        .arg("target")
        .arg("")
        .assert()
        .failure();
}

// ── 2A/2B: Model Registry and BackendType via add-member ────────────────────

#[test]
fn test_add_member_with_session_id() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "ext-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("teams")
        .arg("add-member")
        .arg("ext-team")
        .arg("arch-ctm")
        .arg("--session-id")
        .arg("uuid-1234-abcd")
        .assert()
        .success();

    let member = find_member(&team_dir, "arch-ctm");
    assert_eq!(
        member["sessionId"].as_str(),
        Some("uuid-1234-abcd"),
        "sessionId should be stored"
    );
}

#[test]
fn test_add_member_with_codex_backend_type() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "ext-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("teams")
        .arg("add-member")
        .arg("ext-team")
        .arg("codex-agent")
        .arg("--backend-type")
        .arg("codex")
        .assert()
        .success();

    let member = find_member(&team_dir, "codex-agent");
    assert_eq!(
        member["externalBackendType"].as_str(),
        Some("codex"),
        "externalBackendType should be 'codex'"
    );
}

#[test]
fn test_add_member_with_human_backend_type() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "ext-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("teams")
        .arg("add-member")
        .arg("ext-team")
        .arg("randlee")
        .arg("--backend-type")
        .arg("human:randlee")
        .assert()
        .success();

    let member = find_member(&team_dir, "randlee");
    assert_eq!(
        member["externalBackendType"].as_str(),
        Some("human:randlee"),
        "externalBackendType should be 'human:randlee'"
    );
}

#[test]
fn test_add_member_human_without_username_rejected() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "ext-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("teams")
        .arg("add-member")
        .arg("ext-team")
        .arg("nobody")
        .arg("--backend-type")
        .arg("human:")
        .assert()
        .failure()
        .stderr(predicates::str::contains("requires a username"));
}

#[test]
fn test_add_member_known_model_accepted() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "ext-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("teams")
        .arg("add-member")
        .arg("ext-team")
        .arg("codex-agent")
        .arg("--model")
        .arg("gpt5.3-codex")
        .assert()
        .success();

    let member = find_member(&team_dir, "codex-agent");
    assert_eq!(
        member["externalModel"].as_str(),
        Some("gpt5.3-codex"),
        "externalModel should be stored"
    );
    assert_eq!(
        member["model"].as_str(),
        Some("gpt5.3-codex"),
        "model string field should also be updated"
    );
}

#[test]
fn test_add_member_unknown_model_rejected() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "ext-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("teams")
        .arg("add-member")
        .arg("ext-team")
        .arg("bad-model-agent")
        .arg("--model")
        .arg("totally-unknown-model")
        .assert()
        .failure()
        .stderr(predicates::str::contains("Unknown model"));
}

#[test]
fn test_add_member_custom_model_accepted() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "ext-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("teams")
        .arg("add-member")
        .arg("ext-team")
        .arg("custom-agent")
        .arg("--model")
        .arg("custom:my-special-model")
        .assert()
        .success();

    let member = find_member(&team_dir, "custom-agent");
    assert_eq!(
        member["externalModel"].as_str(),
        Some("custom:my-special-model"),
    );
}

// ── 2D: MCP collision guard ──────────────────────────────────────────────────

#[test]
fn test_add_member_active_name_collision_rejected() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "ext-team");

    // First add — active
    {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.arg("teams")
            .arg("add-member")
            .arg("ext-team")
            .arg("my-agent")
            .arg("--session-id")
            .arg("session-a")
            .assert()
            .success();
    }

    // The first add creates member with agentId "my-agent@ext-team" and is_active=true.
    // Second add with a different effective agent_id (same name but active) → collision.
    // We simulate this by modifying the config to use a different agentId but keep is_active=true,
    // then attempt add-member again which creates a new agentId.
    // However, since the same team/name combo produces the same agent_id format,
    // the idempotent path fires. To test collision, we manually alter the stored agentId.
    let config_path = team_dir.join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    // Change agent_id to something different to force collision detection
    for m in config["members"].as_array_mut().unwrap() {
        if m["name"].as_str() == Some("my-agent") {
            m["agentId"] = serde_json::json!("my-agent-old@ext-team");
            m["isActive"] = serde_json::json!(true);
        }
    }
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    // Now add-member again — same name, active, different agentId → collision
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("teams")
        .arg("add-member")
        .arg("ext-team")
        .arg("my-agent")
        .arg("--session-id")
        .arg("session-b")
        .assert()
        .failure()
        .stderr(predicates::str::contains("already exists and is active"));
}

#[test]
fn test_add_member_same_agent_id_is_idempotent() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "ext-team");

    // Add member once
    for _ in 0..2 {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.arg("teams")
            .arg("add-member")
            .arg("ext-team")
            .arg("idem-agent")
            .assert()
            .success();
    }

    // Should still be exactly one member named "idem-agent"
    let names = member_names(&team_dir);
    assert_eq!(
        names.iter().filter(|n| n.as_str() == "idem-agent").count(),
        1,
        "idempotent add should not duplicate the member"
    );
}

// ── 2E: update-member subcommand ─────────────────────────────────────────────

#[test]
fn test_update_member_session_id() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "upd-team");

    // Add member first
    {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.arg("teams")
            .arg("add-member")
            .arg("upd-team")
            .arg("update-target")
            .assert()
            .success();
    }

    // Update session_id
    {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.arg("teams")
            .arg("update-member")
            .arg("upd-team")
            .arg("update-target")
            .arg("--session-id")
            .arg("new-session-xyz")
            .assert()
            .success();
    }

    let member = find_member(&team_dir, "update-target");
    assert_eq!(
        member["sessionId"].as_str(),
        Some("new-session-xyz"),
        "sessionId should be updated"
    );
}

#[test]
fn test_update_member_active_false() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "upd-team");

    // Add member first (active by default)
    {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.arg("teams")
            .arg("add-member")
            .arg("upd-team")
            .arg("deactivate-me")
            .assert()
            .success();
    }

    // Deactivate
    {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.arg("teams")
            .arg("update-member")
            .arg("upd-team")
            .arg("deactivate-me")
            .arg("--active")
            .arg("false")
            .assert()
            .success();
    }

    let member = find_member(&team_dir, "deactivate-me");
    assert_eq!(
        member["isActive"].as_bool(),
        Some(false),
        "isActive should be set to false"
    );
}

#[test]
fn test_update_member_not_found_returns_error() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "upd-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("teams")
        .arg("update-member")
        .arg("upd-team")
        .arg("nonexistent-member")
        .arg("--session-id")
        .arg("some-id")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not found"));
}

#[test]
fn test_update_member_invalid_model_rejected() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "upd-team");

    // Add member
    {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.arg("teams")
            .arg("add-member")
            .arg("upd-team")
            .arg("model-test")
            .assert()
            .success();
    }

    // Update with invalid model
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("teams")
        .arg("update-member")
        .arg("upd-team")
        .arg("model-test")
        .arg("--model")
        .arg("not-a-real-model")
        .assert()
        .failure()
        .stderr(predicates::str::contains("Unknown model"));
}

// ── E.6 combined scenario: full external member lifecycle ────────────────────

#[test]
fn test_full_external_member_lifecycle() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "lifecycle-team");

    // 1. Add external member with all fields
    {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.arg("teams")
            .arg("add-member")
            .arg("lifecycle-team")
            .arg("arch-ctm")
            .arg("--agent-type")
            .arg("codex")
            .arg("--model")
            .arg("gpt5.3-codex")
            .arg("--backend-type")
            .arg("codex")
            .arg("--session-id")
            .arg("initial-session-id")
            .assert()
            .success();
    }

    let member = find_member(&team_dir, "arch-ctm");
    assert_eq!(member["externalModel"].as_str(), Some("gpt5.3-codex"));
    assert_eq!(member["externalBackendType"].as_str(), Some("codex"));
    assert_eq!(member["sessionId"].as_str(), Some("initial-session-id"));

    // 2. Update session ID after a session restart
    {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.arg("teams")
            .arg("update-member")
            .arg("lifecycle-team")
            .arg("arch-ctm")
            .arg("--session-id")
            .arg("new-session-id-after-restart")
            .assert()
            .success();
    }

    let updated = find_member(&team_dir, "arch-ctm");
    assert_eq!(
        updated["sessionId"].as_str(),
        Some("new-session-id-after-restart")
    );
    // Other fields should be preserved
    assert_eq!(updated["externalModel"].as_str(), Some("gpt5.3-codex"));

    // 3. Deactivate when done
    {
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env(&mut cmd, &temp_dir);
        cmd.arg("teams")
            .arg("update-member")
            .arg("lifecycle-team")
            .arg("arch-ctm")
            .arg("--active")
            .arg("false")
            .assert()
            .success();
    }

    let deactivated = find_member(&team_dir, "arch-ctm");
    assert_eq!(deactivated["isActive"].as_bool(), Some(false));
}

// ── E.6 cleanup conservatism: external agents skipped when daemon unreachable ─

/// Verify that `atm teams cleanup` does NOT remove an external agent member
/// when the daemon is not running (daemon unreachable → unknown liveness →
/// conservative = keep the member).
///
/// External agents (those with `externalBackendType` set) are only removed when
/// the daemon *explicitly* confirms the associated session is dead.  Since there
/// is no running daemon socket in this test environment, the cleanup must skip
/// the external agent and exit with a non-zero status (incomplete cleanup).
#[test]
fn test_cleanup_skips_external_agent_without_session_confirmation() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_test_team(&temp_dir, "cleanup-ext-team");

    // Manually inject an external agent into config.json with all required fields.
    let config_path = team_dir.join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    config["members"].as_array_mut().unwrap().push(serde_json::json!({
        "agentId": "arch-ctm@cleanup-ext-team",
        "name": "arch-ctm",
        "agentType": "codex",
        "model": "gpt5.3-codex",
        "joinedAt": 1739284800000i64,
        "cwd": temp_dir.path().to_str().unwrap(),
        "subscriptions": [],
        "externalBackendType": "codex",
        "sessionId": "test-session-123",
        "isActive": true
    }));
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    // Ensure no ATM_HOME socket or daemon process is reachable.
    // The daemon socket path depends on ATM_HOME, which is set to temp_dir.
    // Since we never started a daemon, any socket-based query will fail.

    // Run cleanup — should fail (incomplete) because the daemon is unreachable
    // and the external agent therefore has unknown liveness.
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "cleanup-ext-team")
        .arg("teams")
        .arg("cleanup")
        .arg("cleanup-ext-team")
        .assert()
        .failure(); // Non-zero exit: cleanup incomplete (member skipped)

    // The external agent must still be present in config.json after the failed cleanup.
    let names = member_names(&team_dir);
    assert!(
        names.contains(&"arch-ctm".to_string()),
        "external agent 'arch-ctm' should NOT have been removed when daemon is unreachable; \
         found members: {names:?}"
    );
}
