#![cfg(unix)]

use super::*;
use crate::plugins::ci_monitor::gh_monitor;
#[cfg(unix)]
use crate::plugins::ci_monitor::health::set_gh_monitor_health_state;
use crate::plugins::ci_monitor::helpers::{
    gh_monitor_key, load_gh_monitor_state_map, upsert_gh_monitor_status,
};
use crate::plugins::ci_monitor::routing::resolve_ci_alert_routing;
use crate::plugins::ci_monitor::test_support::{
    EnvGuard, install_fake_gh_script, read_team_inbox_messages, write_gh_monitor_config,
    write_hook_auth_team_config, write_invalid_gh_monitor_config, write_repo_gh_monitor_config,
};
use crate::plugins::ci_monitor::types::{
    CiMonitorRequest, CiMonitorStatus, CiMonitorTargetKind, GhAlertTargets, GhMonitorHealthUpdate,
};
use agent_team_mail_ci_monitor::repo_state::write_repo_state;
use agent_team_mail_ci_monitor::update_gh_repo_state_in_flight;
use agent_team_mail_ci_monitor::{GhRepoStateFile, GhRepoStateRecord, GhRuntimeOwner};
// These router tests still serialize because EnvGuard mutates process-wide
// ATM_HOME/PATH while fake gh scripts and repo-state files are exercised.
use serial_test::serial;
use std::process::Command;
use tempfile::TempDir;

const FAKE_FOREIGN_DAEMON_BINARY: &str = "fake-daemon-binary";

#[test]
#[cfg(unix)]
fn test_is_gh_command_detection() {
    assert!(is_gh_monitor_command(
        r#"{"version":1,"request_id":"r1","command":"gh-monitor","payload":{}}"#
    ));
    assert!(is_gh_monitor_command(
        r#"{"version":1,"request_id":"r1","command": "gh-monitor","payload":{}}"#
    ));
    assert!(is_gh_status_command(
        r#"{"version":1,"request_id":"r1","command":"gh-status","payload":{}}"#
    ));
    assert!(is_gh_status_command(
        r#"{"version":1,"request_id":"r1","command": "gh-status","payload":{}}"#
    ));
    assert!(is_gh_monitor_control_command(
        r#"{"version":1,"request_id":"r1","command":"gh-monitor-control","payload":{}}"#
    ));
    assert!(is_gh_monitor_health_command(
        r#"{"version":1,"request_id":"r1","command":"gh-monitor-health","payload":{}}"#
    ));
}

#[test]
#[cfg(unix)]
fn test_resolve_ci_alert_routing_enforces_team_and_repo_scope() {
    let temp = TempDir::new().unwrap();
    let repo_dir = temp.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::write(
        repo_dir.join(".atm.toml"),
        r#"[core]
default_team = "scmux-dev"
identity = "team-lead"

[plugins.gh_monitor]
enabled = true
team = "scmux-dev"
agent = "gh-monitor"
repo = "randlee/scmux"
notify_target = "team-lead"
"#,
    )
    .unwrap();

    let (from_agent, targets) = resolve_ci_alert_routing(
        temp.path(),
        "scmux-dev",
        Some(repo_dir.to_string_lossy().as_ref()),
        Some("randlee/scmux"),
        GhAlertTargets::default(),
    );
    assert_eq!(from_agent, "gh-monitor");
    assert_eq!(
        targets,
        vec![("team-lead".to_string(), "scmux-dev".to_string())]
    );

    let (_, wrong_repo_targets) = resolve_ci_alert_routing(
        temp.path(),
        "scmux-dev",
        Some(repo_dir.to_string_lossy().as_ref()),
        Some("randlee/agent-team-mail"),
        GhAlertTargets::default(),
    );
    assert!(
        wrong_repo_targets.is_empty(),
        "repo mismatch must block alert routing"
    );

    let (_, wrong_team_targets) = resolve_ci_alert_routing(
        temp.path(),
        "atm-dev",
        Some(repo_dir.to_string_lossy().as_ref()),
        Some("randlee/scmux"),
        GhAlertTargets::default(),
    );
    assert!(
        wrong_team_targets.is_empty(),
        "team mismatch must block alert routing"
    );
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_monitor_pr_timeout_zero_returns_ci_not_started_and_status_roundtrip() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");
    let req_json = r#"{"version":1,"request_id":"r-gh-1","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"pr","target":"123","start_timeout_secs":0}}"#;
    let monitor_resp = handle_gh_monitor_command(req_json, temp.path()).await;
    assert_eq!(monitor_resp.status, "ok");
    let status_payload = monitor_resp.payload.unwrap();
    assert_eq!(status_payload["state"].as_str(), Some("ci_not_started"));
    assert_eq!(status_payload["target_kind"].as_str(), Some("pr"));
    assert_eq!(status_payload["target"].as_str(), Some("123"));

    let status_req = r#"{"version":1,"request_id":"r-gh-2","command":"gh-status","payload":{"team":"atm-dev","target_kind":"pr","target":"123"}}"#;
    let status_resp = handle_gh_status_command(status_req, temp.path()).await;
    assert_eq!(status_resp.status, "ok");
    let status = status_resp.payload.unwrap();
    assert_eq!(status["state"].as_str(), Some("ci_not_started"));
    assert_eq!(status["target_kind"].as_str(), Some("pr"));
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_monitor_workflow_requires_reference() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");
    let req_json = r#"{"version":1,"request_id":"r-gh-3","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"workflow","target":"ci"}}"#;
    let resp = handle_gh_monitor_command(req_json, temp.path()).await;
    assert_eq!(resp.status, "error");
    let err = resp.error.unwrap();
    assert_eq!(err.code, "MISSING_PARAMETER");
    assert!(err.message.contains("reference"));
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_preflight_dirty_pr_skips_polling() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");
    write_hook_auth_team_config(temp.path(), "atm-dev", "team-lead", &["team-lead"]);
    std::fs::create_dir_all(temp.path().join(".claude/teams/atm-dev/inboxes")).unwrap();
    let run_list_marker = temp.path().join("run-list-marker.txt");
    let _marker_guard = EnvGuard::set(
        "ATM_GH_RUN_LIST_MARKER",
        run_list_marker.to_string_lossy().as_ref(),
    );
    let _path_guard = install_fake_gh_script(
        &temp,
        r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  echo '{"mergeStateStatus":"DIRTY","url":"https://github.com/o/r/pull/123"}'
  exit 0
fi
if [ "$1" = "run" ] && [ "$2" = "list" ]; then
  echo "called" > "${ATM_GH_RUN_LIST_MARKER}"
  echo '[{"databaseId":424242}]'
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 1
"#,
    );

    let req_json = format!(
        r#"{{"version":1,"request_id":"r-gh-preflight-dirty","command":"gh-monitor","payload":{{"team":"atm-dev","target_kind":"pr","target":"123","repo":"o/r","caller_agent":"team-lead","start_timeout_secs":30,"config_cwd":"{}"}}}}"#,
        temp.path().display()
    );
    let resp = handle_gh_monitor_command(&req_json, temp.path()).await;
    assert_eq!(resp.status, "ok");
    let payload = resp.payload.unwrap();
    assert_eq!(payload["state"].as_str(), Some("merge_conflict"));
    assert_eq!(payload["run_id"], serde_json::Value::Null);
    assert!(
        !run_list_marker.exists(),
        "preflight DIRTY must skip CI polling"
    );

    let inbox = read_team_inbox_messages(temp.path(), "atm-dev", "team-lead");
    assert!(
        inbox.iter().any(|msg| {
            msg.text.contains("classification: merge_conflict")
                && msg.text.contains("status: merge_conflict")
                && msg.text.contains("merge_state_status: DIRTY")
                && msg.text.contains("pr_url: https://github.com/o/r/pull/123")
        }),
        "team lead should receive merge_conflict alert with required fields"
    );
    assert!(
        !inbox.iter().any(|msg| {
            msg.text.contains("classification: ci_not_started")
                || msg.text.contains("[ci_not_started]")
        }),
        "DIRTY preflight must suppress ci_not_started alerts"
    );
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_clean_pr_proceeds_to_polling() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");
    write_hook_auth_team_config(temp.path(), "atm-dev", "team-lead", &["team-lead"]);
    std::fs::create_dir_all(temp.path().join(".claude/teams/atm-dev/inboxes")).unwrap();
    let _path_guard = install_fake_gh_script(
        &temp,
        r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ] && [ "$5" = "mergeStateStatus,url" ]; then
  echo '{"mergeStateStatus":"CLEAN","url":"https://github.com/o/r/pull/123"}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "view" ] && [ "$5" = "headRefName,headRefOid,createdAt" ]; then
  echo '{"headRefName":"feature/mock","headRefOid":"abcdef1234567890","createdAt":"2026-03-06T00:00:00Z"}'
  exit 0
fi
if [ "$1" = "run" ] && [ "$2" = "list" ]; then
  echo '[{"databaseId":424242,"headSha":"abcdef1234567890","createdAt":"2026-03-06T00:05:00Z"}]'
  exit 0
fi
if [ "$1" = "run" ] && [ "$2" = "view" ]; then
  echo '{"databaseId":424242,"name":"ci","status":"completed","conclusion":"success","headBranch":"feature/mock","headSha":"abcdef1234567890","url":"https://github.com/o/r/actions/runs/424242","jobs":[{"databaseId":1,"name":"tests","status":"completed","conclusion":"success","startedAt":"2026-03-06T00:00:00Z","completedAt":"2026-03-06T00:00:10Z","steps":[],"url":"https://github.com/o/r/actions/runs/424242/job/1"}],"attempt":1,"pullRequests":[]}'
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 1
"#,
    );

    let req_json = r#"{"version":1,"request_id":"r-gh-preflight-clean","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"pr","target":"123","start_timeout_secs":30}}"#;
    let resp = handle_gh_monitor_command(req_json, temp.path()).await;
    assert_eq!(resp.status, "ok");
    let payload = resp.payload.unwrap();
    assert_eq!(payload["state"].as_str(), Some("monitoring"));
    assert_eq!(payload["run_id"].as_u64(), Some(424242));

    let inbox = read_team_inbox_messages(temp.path(), "atm-dev", "team-lead");
    assert!(
        !inbox
            .iter()
            .any(|msg| msg.text.contains("classification: merge_conflict")),
        "clean preflight should not emit merge_conflict alerts"
    );
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_post_completion_dirty_check() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_hook_auth_team_config(temp.path(), "atm-dev", "team-lead", &["team-lead"]);
    std::fs::create_dir_all(temp.path().join(".claude/teams/atm-dev/inboxes")).unwrap();
    let _path_guard = install_fake_gh_script(
        &temp,
        r#"#!/bin/sh
if [ "$1" = "run" ] && [ "$2" = "view" ]; then
  echo '{"databaseId":42,"name":"ci","status":"completed","conclusion":"success","headBranch":"feature/mock","headSha":"abcdef1234567890","url":"https://github.com/o/r/actions/runs/42","jobs":[{"databaseId":1,"name":"tests","status":"completed","conclusion":"success","startedAt":"2026-03-06T00:00:00Z","completedAt":"2026-03-06T00:00:10Z","steps":[],"url":"https://github.com/o/r/actions/runs/42/job/1"}],"attempt":1,"pullRequests":[]}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "view" ] && [ "$5" = "mergeStateStatus,url" ]; then
  echo '{"mergeStateStatus":"DIRTY","url":"https://github.com/o/r/pull/123"}'
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 1
"#,
    );

    let status_seed = CiMonitorStatus {
        team: "atm-dev".to_string(),
        configured: true,
        enabled: true,
        config_source: None,
        config_path: None,
        target_kind: CiMonitorTargetKind::Pr,
        target: "123".to_string(),
        state: "monitoring".to_string(),
        run_id: Some(42),
        reference: None,
        updated_at: chrono::Utc::now().to_rfc3339(),
        message: None,
        repo_state_updated_at: None,
    };
    let gh_request = CiMonitorRequest {
        team: "atm-dev".to_string(),
        target_kind: CiMonitorTargetKind::Pr,
        target: "123".to_string(),
        reference: None,
        start_timeout_secs: Some(120),
        config_cwd: Some(temp.path().to_string_lossy().to_string()),
    };

    gh_monitor::monitor_gh_run(
        temp.path(),
        &status_seed,
        &gh_request,
        "o/r",
        42,
        Some("o/r"),
        GhAlertTargets {
            caller_agent: Some("team-lead"),
            cc: &[],
        },
    )
    .await
    .expect("monitor_gh_run should complete");

    let inbox = read_team_inbox_messages(temp.path(), "atm-dev", "team-lead");
    assert!(
        inbox.iter().any(|msg| {
            msg.text.contains("classification: merge_conflict")
                && msg.text.contains("status: merge_conflict")
                && msg.text.contains("merge_state_status: DIRTY")
                && msg.text.contains("pr_url: https://github.com/o/r/pull/123")
                && msg.text.contains("run_conclusion: success")
        }),
        "post-terminal DIRTY check must emit merge_conflict alert with run_conclusion"
    );
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_post_completion_clean_check_emits_no_merge_conflict_alert() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_hook_auth_team_config(temp.path(), "atm-dev", "team-lead", &["team-lead"]);
    std::fs::create_dir_all(temp.path().join(".claude/teams/atm-dev/inboxes")).unwrap();
    let _path_guard = install_fake_gh_script(
        &temp,
        r#"#!/bin/sh
if [ "$1" = "run" ] && [ "$2" = "view" ]; then
  echo '{"databaseId":42,"name":"ci","status":"completed","conclusion":"success","headBranch":"feature/mock","headSha":"abcdef1234567890","url":"https://github.com/o/r/actions/runs/42","jobs":[{"databaseId":1,"name":"tests","status":"completed","conclusion":"success","startedAt":"2026-03-06T00:00:00Z","completedAt":"2026-03-06T00:00:10Z","steps":[],"url":"https://github.com/o/r/actions/runs/42/job/1"}],"attempt":1,"pullRequests":[]}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "view" ] && [ "$5" = "mergeStateStatus,url" ]; then
  echo '{"mergeStateStatus":"CLEAN","url":"https://github.com/o/r/pull/123"}'
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 1
"#,
    );

    let status_seed = CiMonitorStatus {
        team: "atm-dev".to_string(),
        configured: true,
        enabled: true,
        config_source: None,
        config_path: None,
        target_kind: CiMonitorTargetKind::Pr,
        target: "123".to_string(),
        state: "monitoring".to_string(),
        run_id: Some(42),
        reference: None,
        updated_at: chrono::Utc::now().to_rfc3339(),
        message: None,
        repo_state_updated_at: None,
    };
    let gh_request = CiMonitorRequest {
        team: "atm-dev".to_string(),
        target_kind: CiMonitorTargetKind::Pr,
        target: "123".to_string(),
        reference: None,
        start_timeout_secs: Some(120),
        config_cwd: None,
    };

    gh_monitor::monitor_gh_run(
        temp.path(),
        &status_seed,
        &gh_request,
        "o/r",
        42,
        None,
        GhAlertTargets::default(),
    )
    .await
    .expect("monitor_gh_run should complete");

    let inbox = read_team_inbox_messages(temp.path(), "atm-dev", "team-lead");
    assert!(
        !inbox
            .iter()
            .any(|msg| msg.text.contains("classification: merge_conflict")),
        "post-terminal CLEAN check must not emit merge_conflict alert"
    );
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_terminal_failure_bypasses_progress_throttle_window() {
    let temp = TempDir::new().unwrap();
    let counter_path = temp.path().join("gh-counter.txt");
    let _counter_guard = EnvGuard::set(
        "ATM_GH_COUNTER_FILE",
        counter_path.to_string_lossy().as_ref(),
    );
    let _path_guard = install_fake_gh_script(
        &temp,
        r#"#!/bin/sh
COUNTER_FILE="${ATM_GH_COUNTER_FILE}"
count=0
if [ -f "$COUNTER_FILE" ]; then
  count=$(cat "$COUNTER_FILE")
fi
count=$((count + 1))
echo "$count" > "$COUNTER_FILE"

if [ "$1" = "run" ] && [ "$2" = "view" ]; then
  if [ "$count" -eq 1 ]; then
    echo '{"databaseId":42,"name":"ci","status":"in_progress","conclusion":null,"headBranch":"develop","headSha":"abcdef1234567890","url":"https://github.com/o/r/actions/runs/42","jobs":[{"databaseId":11,"name":"clippy","status":"completed","conclusion":"success","startedAt":"2026-03-06T00:00:00Z","completedAt":"2026-03-06T00:00:10Z","steps":[],"url":"https://github.com/o/r/actions/runs/42/job/11"},{"databaseId":12,"name":"tests","status":"in_progress","conclusion":null,"startedAt":"2026-03-06T00:00:00Z","completedAt":null,"steps":[],"url":"https://github.com/o/r/actions/runs/42/job/12"}],"attempt":1,"pullRequests":[]}'
  else
    echo '{"databaseId":42,"name":"ci","status":"completed","conclusion":"failure","headBranch":"develop","headSha":"abcdef1234567890","url":"https://github.com/o/r/actions/runs/42","jobs":[{"databaseId":11,"name":"clippy","status":"completed","conclusion":"success","startedAt":"2026-03-06T00:00:00Z","completedAt":"2026-03-06T00:00:10Z","steps":[],"url":"https://github.com/o/r/actions/runs/42/job/11"},{"databaseId":12,"name":"tests","status":"completed","conclusion":"failure","startedAt":"2026-03-06T00:00:00Z","completedAt":"2026-03-06T00:00:20Z","steps":[{"name":"suite","status":"completed","conclusion":"failure"}],"url":"https://github.com/o/r/actions/runs/42/job/12"}],"attempt":1,"pullRequests":[]}'
  fi
  exit 0
fi

echo "unsupported fake gh invocation: $*" >&2
exit 1
"#,
    );

    let status_seed = CiMonitorStatus {
        team: "atm-dev".to_string(),
        configured: true,
        enabled: true,
        config_source: None,
        config_path: None,
        target_kind: CiMonitorTargetKind::Pr,
        target: "123".to_string(),
        state: "tracking".to_string(),
        run_id: Some(42),
        reference: None,
        updated_at: chrono::Utc::now().to_rfc3339(),
        message: None,
        repo_state_updated_at: None,
    };
    let gh_request = CiMonitorRequest {
        team: "atm-dev".to_string(),
        target_kind: CiMonitorTargetKind::Pr,
        target: "123".to_string(),
        reference: None,
        start_timeout_secs: Some(120),
        config_cwd: None,
    };

    let started = std::time::Instant::now();
    gh_monitor::monitor_gh_run(
        temp.path(),
        &status_seed,
        &gh_request,
        "o/r",
        42,
        None,
        GhAlertTargets::default(),
    )
    .await
    .expect("monitor_gh_run should complete");
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "terminal update should bypass progress throttle, elapsed={elapsed:?}"
    );

    let state_map = load_gh_monitor_state_map(temp.path()).expect("state map");
    let key = gh_monitor_key("atm-dev", CiMonitorTargetKind::Pr, "123", None, Some("o/r"));
    let terminal = state_map.get(&key).expect("status entry");
    assert_eq!(terminal.state, "failure");
}

#[test]
#[cfg(unix)]
fn test_format_summary_table_contains_required_columns() {
    let run = gh_monitor::GhRunView {
        database_id: 42,
        name: "ci".to_string(),
        status: "completed".to_string(),
        conclusion: Some("success".to_string()),
        head_branch: "develop".to_string(),
        head_sha: "abcdef1234567890".to_string(),
        url: "https://github.com/o/r/actions/runs/42".to_string(),
        jobs: vec![gh_monitor::GhRunJob {
            database_id: 1,
            name: "clippy".to_string(),
            status: "completed".to_string(),
            conclusion: Some("success".to_string()),
            started_at: Some("2026-03-06T00:00:00Z".to_string()),
            completed_at: Some("2026-03-06T00:00:10Z".to_string()),
            steps: Vec::new(),
            url: Some("https://github.com/o/r/actions/runs/42/job/1".to_string()),
        }],
        attempt: Some(1),
        pull_requests: Vec::new(),
    };

    let table = gh_monitor::format_summary_table(&run);
    assert!(table.contains("| Job/Test | Status | Runtime |"));
    assert!(table.contains("| clippy | success |"));
}

#[test]
#[cfg(unix)]
fn test_derive_pr_url_prefers_pr_target_fallback() {
    let run = gh_monitor::GhRunView {
        database_id: 42,
        name: "ci".to_string(),
        status: "completed".to_string(),
        conclusion: Some("failure".to_string()),
        head_branch: "feature/x".to_string(),
        head_sha: "abcdef1234567890".to_string(),
        url: "https://github.com/o/r/actions/runs/42".to_string(),
        jobs: Vec::new(),
        attempt: Some(1),
        pull_requests: Vec::new(),
    };
    let status_seed = CiMonitorStatus {
        team: "atm-dev".to_string(),
        configured: true,
        enabled: true,
        config_source: None,
        config_path: None,
        target_kind: CiMonitorTargetKind::Pr,
        target: "123".to_string(),
        state: "monitoring".to_string(),
        run_id: Some(42),
        reference: None,
        updated_at: "2026-03-06T00:00:00Z".to_string(),
        message: None,
        repo_state_updated_at: None,
    };
    let request = CiMonitorRequest {
        team: "atm-dev".to_string(),
        target_kind: CiMonitorTargetKind::Pr,
        target: "123".to_string(),
        reference: None,
        start_timeout_secs: Some(120),
        config_cwd: None,
    };
    let pr_url = gh_monitor::derive_pr_url(&run, &status_seed, &request);
    assert_eq!(pr_url.as_deref(), Some("https://github.com/o/r/pull/123"));
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_wait_for_pr_run_start_success_path_finds_run() {
    let temp = TempDir::new().unwrap();
    let _path_guard = install_fake_gh_script(
        &temp,
        r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  echo '{"headRefName":"feature/mock","headRefOid":"sha-pr-123","createdAt":"2026-03-06T00:00:00Z"}'
  exit 0
fi
if [ "$1" = "run" ] && [ "$2" = "list" ]; then
  echo '[{"databaseId":111111,"headSha":"sha-older","createdAt":"2026-03-05T23:59:59Z"},{"databaseId":222222,"headSha":"sha-pr-123","createdAt":"2026-03-06T00:05:00Z"},{"databaseId":333333,"headSha":"sha-pr-123","createdAt":"2026-03-05T23:00:00Z"}]'
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 1
"#,
    );
    let run_id = gh_monitor::wait_for_pr_run_start(temp.path(), "atm-dev", "o/r", 123, 1)
        .await
        .unwrap();
    assert_eq!(run_id, Some(222222));
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_fetch_run_view_injects_repo_scope_flag() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().unwrap();
    let script_path = temp.path().join("gh");
    std::fs::write(
        &script_path,
        r#"#!/bin/sh
if [ "$1" = "-R" ] && [ "$2" = "o/r" ] && [ "$3" = "run" ] && [ "$4" = "view" ] && [ "$5" = "42" ]; then
  echo '{"databaseId":42,"name":"CI","status":"completed","conclusion":"success","headBranch":"main","headSha":"abc123","url":"https://github.com/o/r/actions/runs/42","jobs":[],"attempt":1,"pullRequests":[]}'
  exit 0
fi
echo "missing -R scope: $*" >&2
exit 1
"#,
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms).unwrap();

    let prior_path = std::env::var("PATH").unwrap_or_default();
    let new_path = if prior_path.is_empty() {
        temp.path().display().to_string()
    } else {
        format!("{}:{prior_path}", temp.path().display())
    };
    let _path_guard = EnvGuard::set("PATH", &new_path);

    let output = gh_monitor::fetch_run_view(temp.path(), "atm-dev", "o/r", 42)
        .await
        .unwrap();
    assert_eq!(output.database_id, 42);
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_handle_gh_monitor_command_rejects_config_team_mismatch() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "other-team");

    let req_json = r#"{"version":1,"request_id":"r-gh-team-mismatch","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"run","target":"42"}}"#;
    let resp = handle_gh_monitor_command(req_json, temp.path()).await;
    assert_eq!(resp.status, "error");
    let err = resp.error.unwrap();
    assert_eq!(err.code, "CONFIG_ERROR");
    assert!(
        err.message.contains("team mismatch"),
        "expected team mismatch error, got: {}",
        err.message
    );
}

#[tokio::test]
#[cfg(unix)]
async fn test_build_failure_payload_contains_required_fields() {
    let temp = TempDir::new().unwrap();
    let run = gh_monitor::GhRunView {
        database_id: 42,
        name: "ci".to_string(),
        status: "completed".to_string(),
        conclusion: Some("failure".to_string()),
        head_branch: "feature/x".to_string(),
        head_sha: "abcdef1234567890".to_string(),
        url: "https://github.com/o/r/actions/runs/42".to_string(),
        jobs: Vec::new(),
        attempt: Some(2),
        pull_requests: Vec::new(),
    };
    let status_seed = CiMonitorStatus {
        team: "atm-dev".to_string(),
        configured: true,
        enabled: true,
        config_source: None,
        config_path: None,
        target_kind: CiMonitorTargetKind::Pr,
        target: "123".to_string(),
        state: "monitoring".to_string(),
        run_id: Some(42),
        reference: None,
        updated_at: "2026-03-06T00:00:00Z".to_string(),
        message: None,
        repo_state_updated_at: None,
    };
    let request = CiMonitorRequest {
        team: "atm-dev".to_string(),
        target_kind: CiMonitorTargetKind::Pr,
        target: "123".to_string(),
        reference: None,
        start_timeout_secs: Some(120),
        config_cwd: None,
    };
    let payload = gh_monitor::build_failure_payload(
        temp.path(),
        "atm-dev",
        &run,
        &status_seed,
        &request,
        "o/r",
        "corr-1",
    )
    .await;
    for required in [
        "run_url:",
        "failed_job_urls:",
        "pr_url:",
        "workflow:",
        "job_names:",
        "run_id:",
        "run_attempt:",
        "branch:",
        "commit_short:",
        "commit_full:",
        "classification:",
        "first_failing_step:",
        "log_excerpt:",
        "correlation_id:",
        "next_action_hint:",
    ] {
        assert!(
            payload.contains(required),
            "failure payload missing field marker: {required}"
        );
    }
}

#[test]
#[cfg(unix)]
fn test_classify_failure_infra_when_runner_failure_detected() {
    let run = gh_monitor::GhRunView {
        database_id: 88,
        name: "ci".to_string(),
        status: "completed".to_string(),
        conclusion: Some("failure".to_string()),
        head_branch: "main".to_string(),
        head_sha: "abcdef1234567890".to_string(),
        url: "https://github.com/o/r/actions/runs/88".to_string(),
        jobs: vec![gh_monitor::GhRunJob {
            database_id: 101,
            name: "Runner provisioning failed".to_string(),
            status: "completed".to_string(),
            conclusion: Some("failure".to_string()),
            started_at: None,
            completed_at: None,
            steps: vec![crate::plugins::ci_monitor::github_schema::GhRunStep {
                name: "Set up runner".to_string(),
                status: Some("completed".to_string()),
                conclusion: Some("failure".to_string()),
            }],
            url: None,
        }],
        attempt: Some(1),
        pull_requests: Vec::new(),
    };

    assert_eq!(gh_monitor::classify_failure(&run), "infra");
}

#[tokio::test]
#[serial]
#[cfg(unix)]
async fn test_gh_monitor_run_target_success_status_roundtrip() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");
    let req_json = r#"{"version":1,"request_id":"r-gh-run","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"run","target":"456789"}}"#;
    let monitor_resp = handle_gh_monitor_command(req_json, temp.path()).await;
    assert_eq!(monitor_resp.status, "ok");
    let payload = monitor_resp.payload.unwrap();
    assert_eq!(payload["target_kind"].as_str(), Some("run"));
    assert_eq!(payload["target"].as_str(), Some("456789"));
    assert_eq!(payload["run_id"].as_u64(), Some(456789));
    assert_eq!(payload["state"].as_str(), Some("monitoring"));

    let status_req = r#"{"version":1,"request_id":"r-gh-run-status","command":"gh-status","payload":{"team":"atm-dev","target_kind":"run","target":"456789"}}"#;
    let status_resp = handle_gh_status_command(status_req, temp.path()).await;
    assert_eq!(status_resp.status, "ok");
    let status = status_resp.payload.unwrap();
    assert_eq!(status["target_kind"].as_str(), Some("run"));
    assert_eq!(status["run_id"].as_u64(), Some(456789));
    assert_eq!(status["state"].as_str(), Some("monitoring"));
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_monitor_workflow_success_status_roundtrip() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");
    let _path_guard = install_fake_gh_script(
        &temp,
        r#"#!/bin/sh
if [ "$1" = "run" ] && [ "$2" = "list" ]; then
  echo '[{"databaseId":987654,"headBranch":"develop","headSha":"abcd1234"}]'
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 1
"#,
    );
    let req_json = r#"{"version":1,"request_id":"r-gh-workflow","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"workflow","target":"ci","reference":"develop","start_timeout_secs":30}}"#;
    let monitor_resp = handle_gh_monitor_command(req_json, temp.path()).await;
    assert_eq!(monitor_resp.status, "ok");
    let payload = monitor_resp.payload.unwrap();
    assert_eq!(payload["target_kind"].as_str(), Some("workflow"));
    assert_eq!(payload["target"].as_str(), Some("ci"));
    assert_eq!(payload["reference"].as_str(), Some("develop"));
    assert_eq!(payload["run_id"].as_u64(), Some(987654));
    assert_eq!(payload["state"].as_str(), Some("monitoring"));

    let status_req = r#"{"version":1,"request_id":"r-gh-workflow-status","command":"gh-status","payload":{"team":"atm-dev","target_kind":"workflow","target":"ci"}}"#;
    let status_resp = handle_gh_status_command(status_req, temp.path()).await;
    assert_eq!(status_resp.status, "ok");
    let status = status_resp.payload.unwrap();
    assert_eq!(status["target_kind"].as_str(), Some("workflow"));
    assert_eq!(status["target"].as_str(), Some("ci"));
    assert_eq!(status["reference"].as_str(), Some("develop"));
    assert_eq!(status["run_id"].as_u64(), Some(987654));
    assert_eq!(status["state"].as_str(), Some("monitoring"));
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_monitor_uses_repo_config_source_from_payload_cwd() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    let repo_dir = temp.path().join("repo");
    write_repo_gh_monitor_config(&repo_dir, "atm-dev");

    let req_json = format!(
        r#"{{"version":1,"request_id":"r-gh-repo-src","command":"gh-monitor","payload":{{"team":"atm-dev","target_kind":"run","target":"42","config_cwd":"{}"}}}}"#,
        repo_dir.to_string_lossy()
    );
    let resp = handle_gh_monitor_command(&req_json, temp.path()).await;
    assert_eq!(resp.status, "ok");
    let payload = resp.payload.unwrap();
    assert_eq!(payload["configured"].as_bool(), Some(true));
    assert_eq!(payload["enabled"].as_bool(), Some(true));
    assert_eq!(payload["config_source"].as_str(), Some("repo"));
    assert_eq!(
        payload["config_path"].as_str(),
        Some(repo_dir.join(".atm.toml").to_string_lossy().as_ref())
    );
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_status_distinguishes_repo_scopes_for_same_target() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");

    let status_a = CiMonitorStatus {
        team: "atm-dev".to_string(),
        configured: true,
        enabled: true,
        config_source: Some("repo".to_string()),
        config_path: Some(temp.path().join(".atm.toml").to_string_lossy().to_string()),
        target_kind: CiMonitorTargetKind::Run,
        target: "42".to_string(),
        state: "tracking".to_string(),
        run_id: Some(42),
        reference: None,
        updated_at: chrono::Utc::now().to_rfc3339(),
        message: Some("repo a".to_string()),
        repo_state_updated_at: None,
    };
    let status_b = CiMonitorStatus {
        message: Some("repo b".to_string()),
        ..status_a.clone()
    };

    crate::plugins::ci_monitor::helpers::upsert_gh_monitor_status_for_repo(
        temp.path(),
        status_a,
        Some("acme/repo-a"),
    )
    .unwrap();
    crate::plugins::ci_monitor::helpers::upsert_gh_monitor_status_for_repo(
        temp.path(),
        status_b,
        Some("acme/repo-b"),
    )
    .unwrap();

    let req_json = r#"{"version":1,"request_id":"r-gh-repo-status","command":"gh-status","payload":{"team":"atm-dev","target_kind":"run","target":"42","repo":"acme/repo-b"}}"#;
    let resp = handle_gh_status_command(req_json, temp.path()).await;
    assert_eq!(resp.status, "ok");
    let payload = resp.payload.unwrap();
    assert_eq!(payload["message"].as_str(), Some("repo b"));
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_status_uses_global_config_source_when_repo_missing() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");

    let status_seed = CiMonitorStatus {
        team: "atm-dev".to_string(),
        configured: true,
        enabled: true,
        config_source: None,
        config_path: None,
        target_kind: CiMonitorTargetKind::Run,
        target: "9001".to_string(),
        state: "monitoring".to_string(),
        run_id: Some(9001),
        reference: None,
        updated_at: "2026-03-06T00:00:00Z".to_string(),
        message: None,
        repo_state_updated_at: None,
    };
    upsert_gh_monitor_status(temp.path(), status_seed).unwrap();

    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let req_json = format!(
        r#"{{"version":1,"request_id":"r-gh-global-src","command":"gh-status","payload":{{"team":"atm-dev","target_kind":"run","target":"9001","config_cwd":"{}"}}}}"#,
        outside.to_string_lossy()
    );
    let resp = handle_gh_status_command(&req_json, temp.path()).await;
    assert_eq!(resp.status, "ok");
    let payload = resp.payload.unwrap();
    assert_eq!(payload["configured"].as_bool(), Some(true));
    assert_eq!(payload["enabled"].as_bool(), Some(true));
    assert_eq!(payload["config_source"].as_str(), Some("global"));
    assert_eq!(
        payload["config_path"].as_str(),
        Some(
            temp.path()
                .join(".config/atm/config.toml")
                .to_string_lossy()
                .as_ref()
        )
    );
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_monitor_health_reports_global_config_source() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");

    let outside = temp.path().join("outside-health");
    std::fs::create_dir_all(&outside).unwrap();
    let req_json = format!(
        r#"{{"version":1,"request_id":"r-gh-health-src","command":"gh-monitor-health","payload":{{"team":"atm-dev","config_cwd":"{}"}}}}"#,
        outside.to_string_lossy()
    );
    let resp = handle_gh_monitor_health_command(&req_json, temp.path()).await;
    assert_eq!(resp.status, "ok");
    let payload = resp.payload.unwrap();
    assert_eq!(payload["configured"].as_bool(), Some(true));
    assert_eq!(payload["enabled"].as_bool(), Some(true));
    assert_eq!(payload["config_source"].as_str(), Some("global"));
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_status_workflow_reference_disambiguates_parallel_runs() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");
    let status_a = CiMonitorStatus {
        team: "atm-dev".to_string(),
        configured: true,
        enabled: true,
        config_source: None,
        config_path: None,
        target_kind: CiMonitorTargetKind::Workflow,
        target: "ci".to_string(),
        state: "monitoring".to_string(),
        run_id: Some(111),
        reference: Some("develop".to_string()),
        updated_at: "2026-03-06T00:00:10Z".to_string(),
        message: None,
        repo_state_updated_at: None,
    };
    let status_b = CiMonitorStatus {
        team: "atm-dev".to_string(),
        configured: true,
        enabled: true,
        config_source: None,
        config_path: None,
        target_kind: CiMonitorTargetKind::Workflow,
        target: "ci".to_string(),
        state: "monitoring".to_string(),
        run_id: Some(222),
        reference: Some("release/v1".to_string()),
        updated_at: "2026-03-06T00:00:11Z".to_string(),
        message: None,
        repo_state_updated_at: None,
    };
    upsert_gh_monitor_status(temp.path(), status_a).unwrap();
    upsert_gh_monitor_status(temp.path(), status_b).unwrap();

    let status_req = r#"{"version":1,"request_id":"r-gh-workflow-ref","command":"gh-status","payload":{"team":"atm-dev","target_kind":"workflow","target":"ci","reference":"release/v1"}}"#;
    let status_resp = handle_gh_status_command(status_req, temp.path()).await;
    assert_eq!(status_resp.status, "ok");
    let status = status_resp.payload.unwrap();
    assert_eq!(status["reference"].as_str(), Some("release/v1"));
    assert_eq!(status["run_id"].as_u64(), Some(222));
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_monitor_control_start_stop_restart_and_health() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");

    let start_req = r#"{"version":1,"request_id":"r-gh-start","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"start"}}"#;
    let start_resp = handle_gh_monitor_control_command(start_req, temp.path()).await;
    assert_eq!(start_resp.status, "ok");
    let start = start_resp.payload.unwrap();
    assert_eq!(start["lifecycle_state"].as_str(), Some("running"));

    let stop_req = r#"{"version":1,"request_id":"r-gh-stop","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"stop","drain_timeout_secs":1}}"#;
    let stop_resp = handle_gh_monitor_control_command(stop_req, temp.path()).await;
    assert_eq!(stop_resp.status, "ok");
    let stop = stop_resp.payload.unwrap();
    assert_eq!(stop["lifecycle_state"].as_str(), Some("stopped"));

    let restart_req = r#"{"version":1,"request_id":"r-gh-restart","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"restart","drain_timeout_secs":1}}"#;
    let restart_resp = handle_gh_monitor_control_command(restart_req, temp.path()).await;
    assert_eq!(restart_resp.status, "ok");
    let restart = restart_resp.payload.unwrap();
    assert_eq!(restart["lifecycle_state"].as_str(), Some("running"));

    let health_req = r#"{"version":1,"request_id":"r-gh-health","command":"gh-monitor-health","payload":{"team":"atm-dev"}}"#;
    let health_resp = handle_gh_monitor_health_command(health_req, temp.path()).await;
    assert_eq!(health_resp.status, "ok");
    let health = health_resp.payload.unwrap();
    assert_eq!(health["team"].as_str(), Some("atm-dev"));
    assert_eq!(health["lifecycle_state"].as_str(), Some("running"));
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_monitor_control_cross_team_requires_user_authorized() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "ops-team");

    let start_req = r#"{"version":1,"request_id":"r-gh-start-cross-team","command":"gh-monitor-control","payload":{"team":"ops-team","action":"start","actor":"team-lead","actor_team":"atm-dev"}}"#;
    let start_resp = handle_gh_monitor_control_command(start_req, temp.path()).await;
    assert_eq!(start_resp.status, "error");
    let start_err = start_resp.error.expect("cross-team start should fail");
    assert_eq!(start_err.code, "AUTHORIZATION_REQUIRED");

    let stop_req = r#"{"version":1,"request_id":"r-gh-stop-cross-team","command":"gh-monitor-control","payload":{"team":"ops-team","action":"stop","actor":"team-lead","actor_team":"atm-dev","drain_timeout_secs":1}}"#;
    let stop_resp = handle_gh_monitor_control_command(stop_req, temp.path()).await;
    assert_eq!(stop_resp.status, "error");
    let err = stop_resp.error.expect("cross-team stop should fail");
    assert_eq!(err.code, "AUTHORIZATION_REQUIRED");
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_monitor_control_cross_team_stop_and_restart_notify_team_lead() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "ops-team");
    write_hook_auth_team_config(temp.path(), "ops-team", "team-lead", &["team-lead"]);
    std::fs::create_dir_all(temp.path().join(".claude/teams/ops-team/inboxes")).unwrap();

    let stop_req = r#"{"version":1,"request_id":"r-gh-stop-cross-team-notify","command":"gh-monitor-control","payload":{"team":"ops-team","action":"stop","actor":"team-lead","actor_team":"atm-dev","user_authorized":true,"operator_reason":"manual rollback","drain_timeout_secs":1}}"#;
    let stop_resp = handle_gh_monitor_control_command(stop_req, temp.path()).await;
    assert_eq!(stop_resp.status, "ok");

    let restart_req = r#"{"version":1,"request_id":"r-gh-restart-cross-team-notify","command":"gh-monitor-control","payload":{"team":"ops-team","action":"restart","actor":"team-lead","actor_team":"atm-dev","user_authorized":true,"operator_reason":"resume service","drain_timeout_secs":1}}"#;
    let restart_resp = handle_gh_monitor_control_command(restart_req, temp.path()).await;
    assert_eq!(restart_resp.status, "ok");

    let inbox = read_team_inbox_messages(temp.path(), "ops-team", "team-lead");
    assert!(
        inbox.iter().any(|msg| {
            msg.text
                .contains("your gh monitor was stopped by team-lead@atm-dev")
                && msg.text.contains("manual rollback")
        }),
        "cross-team stop should notify affected team lead with actor and reason"
    );
    assert!(
        inbox.iter().any(|msg| {
            msg.text
                .contains("your gh monitor was restarted by team-lead@atm-dev")
                && msg.text.contains("resume service")
        }),
        "cross-team restart should notify affected team lead with actor and reason"
    );
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_monitor_health_includes_owner_metadata_fields() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");
    update_gh_repo_state_in_flight(temp.path(), "atm-dev", "o/r", 1, "atm-daemon").unwrap();

    let health_req = r#"{"version":1,"request_id":"r-gh-health-owner","command":"gh-monitor-health","payload":{"team":"atm-dev"}}"#;
    let health_resp = handle_gh_monitor_health_command(health_req, temp.path()).await;
    assert_eq!(health_resp.status, "ok");
    let health = health_resp.payload.unwrap();
    assert_eq!(health["owner_runtime_kind"].as_str(), Some("isolated"));
    assert_eq!(health["owner_repo"].as_str(), Some("o/r"));
    assert_eq!(health["owner_poll_interval_secs"].as_u64(), Some(60));
    assert_eq!(
        health["owner_pid"].as_u64(),
        Some(std::process::id() as u64)
    );
    assert!(
        health["owner_binary_path"]
            .as_str()
            .unwrap_or_default()
            .contains("agent_team_mail_daemon"),
        "expected owner_binary_path in health payload: {health:?}"
    );
    let canonical_home = std::fs::canonicalize(temp.path()).unwrap();
    assert_eq!(
        health["owner_atm_home"].as_str(),
        Some(canonical_home.to_string_lossy().as_ref())
    );
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_monitor_health_surfaces_live_foreign_owner_conflict() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");
    let mut child = Command::new("sleep").arg("2").spawn().unwrap();
    let now = chrono::Utc::now();
    write_repo_state(
        temp.path(),
        &GhRepoStateFile {
            records: vec![GhRepoStateRecord {
                team: "atm-dev".to_string(),
                repo: "o/r".to_string(),
                updated_at: now.to_rfc3339(),
                cache_expires_at: (now + chrono::Duration::seconds(300)).to_rfc3339(),
                last_refresh_at: None,
                budget_limit_per_hour: 100,
                budget_used_in_window: 0,
                budget_window_started_at: now.to_rfc3339(),
                budget_warning_threshold: 50,
                warning_emitted_at: None,
                blocked: false,
                in_flight: 0,
                idle_poll_interval_secs: 300,
                active_poll_interval_secs: 60,
                branch_ref_counts: Vec::new(),
                last_call: None,
                rate_limit: None,
                owner: Some(GhRuntimeOwner {
                    runtime: "dev".to_string(),
                    executable_path: FAKE_FOREIGN_DAEMON_BINARY.to_string(),
                    home_scope: temp.path().to_string_lossy().to_string(),
                    pid: child.id(),
                }),
            }],
        },
    )
    .unwrap();

    let health_req = r#"{"version":1,"request_id":"r-gh-health-conflict","command":"gh-monitor-health","payload":{"team":"atm-dev"}}"#;
    let health_resp = handle_gh_monitor_health_command(health_req, temp.path()).await;
    assert_eq!(health_resp.status, "ok");
    let health = health_resp.payload.unwrap();
    assert_eq!(health["team"].as_str(), Some("atm-dev"));
    assert_eq!(health["availability_state"].as_str(), Some("degraded"));
    assert_eq!(health["owner_pid"].as_u64(), Some(child.id() as u64));
    assert_eq!(
        health["owner_binary_path"].as_str(),
        Some(FAKE_FOREIGN_DAEMON_BINARY)
    );
    assert!(
        health["message"]
            .as_str()
            .unwrap_or_default()
            .contains("lease conflict")
    );

    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_monitor_restart_reloads_updated_config_without_daemon_restart() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");

    let start_req = r#"{"version":1,"request_id":"r-gh-start-reload","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"start"}}"#;
    let start_resp = handle_gh_monitor_control_command(start_req, temp.path()).await;
    assert_eq!(start_resp.status, "ok");

    write_invalid_gh_monitor_config(temp.path(), "atm-dev");
    let restart_req = r#"{"version":1,"request_id":"r-gh-restart-invalid","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"restart","drain_timeout_secs":1}}"#;
    let restart_resp = handle_gh_monitor_control_command(restart_req, temp.path()).await;
    assert_eq!(restart_resp.status, "error");
    let err = restart_resp
        .error
        .expect("restart should return config error");
    assert_eq!(err.code, "CONFIG_ERROR");
    assert!(
        err.message.contains("gh_monitor unavailable after reload"),
        "unexpected restart error: {}",
        err.message
    );

    let health_req = r#"{"version":1,"request_id":"r-gh-health-invalid","command":"gh-monitor-health","payload":{"team":"atm-dev"}}"#;
    let health_resp = handle_gh_monitor_health_command(health_req, temp.path()).await;
    assert_eq!(health_resp.status, "ok");
    let health = health_resp.payload.unwrap();
    assert_eq!(health["lifecycle_state"].as_str(), Some("stopped"));
    assert_eq!(
        health["availability_state"].as_str(),
        Some("disabled_config_error")
    );
    assert!(
        !health["message"].as_str().unwrap_or_default().is_empty(),
        "expected actionable config error message in health payload"
    );

    write_gh_monitor_config(temp.path(), "atm-dev");
    let restart_recover_req = r#"{"version":1,"request_id":"r-gh-restart-recover","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"restart","drain_timeout_secs":1}}"#;
    let restart_recover_resp =
        handle_gh_monitor_control_command(restart_recover_req, temp.path()).await;
    assert_eq!(restart_recover_resp.status, "ok");
    let restart_recover = restart_recover_resp.payload.unwrap();
    assert_eq!(restart_recover["lifecycle_state"].as_str(), Some("running"));
    assert_eq!(
        restart_recover["availability_state"].as_str(),
        Some("healthy")
    );

    let health_recover_req = r#"{"version":1,"request_id":"r-gh-health-recover","command":"gh-monitor-health","payload":{"team":"atm-dev"}}"#;
    let health_recover_resp =
        handle_gh_monitor_health_command(health_recover_req, temp.path()).await;
    assert_eq!(health_recover_resp.status, "ok");
    let health_recover = health_recover_resp.payload.unwrap();
    assert_eq!(health_recover["lifecycle_state"].as_str(), Some("running"));
    assert_eq!(
        health_recover["availability_state"].as_str(),
        Some("healthy")
    );
}

#[tokio::test]
#[cfg(unix)]
#[serial]
async fn test_gh_monitor_command_rejected_when_lifecycle_stopped() {
    let temp = TempDir::new().unwrap();
    let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
    write_gh_monitor_config(temp.path(), "atm-dev");
    let _ = set_gh_monitor_health_state(
        temp.path(),
        "atm-dev",
        GhMonitorHealthUpdate {
            lifecycle_state: Some("stopped"),
            availability_state: Some("healthy"),
            in_flight: Some(0),
            message: Some("manually stopped for test".to_string()),
            ..Default::default()
        },
    );

    let req_json = r#"{"version":1,"request_id":"r-gh-stopped","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"run","target":"42"}}"#;
    let resp = handle_gh_monitor_command(req_json, temp.path()).await;
    assert_eq!(resp.status, "error");
    let err = resp.error.unwrap();
    assert_eq!(err.code, "MONITOR_STOPPED");
}

#[tokio::test]
#[cfg(unix)]
async fn test_gh_monitor_invalid_config_transitions_to_disabled_config_error() {
    let temp = TempDir::new().unwrap();
    write_invalid_gh_monitor_config(temp.path(), "atm-dev");

    let req_json = r#"{"version":1,"request_id":"r-gh-config","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"run","target":"42"}}"#;
    let resp = handle_gh_monitor_command(req_json, temp.path()).await;
    assert_eq!(resp.status, "error");
    let err = resp.error.unwrap();
    assert_eq!(err.code, "CONFIG_ERROR");

    let health_req = r#"{"version":1,"request_id":"r-gh-health","command":"gh-monitor-health","payload":{"team":"atm-dev"}}"#;
    let health_resp = handle_gh_monitor_health_command(health_req, temp.path()).await;
    assert_eq!(health_resp.status, "ok");
    let health = health_resp.payload.unwrap();
    assert_eq!(
        health["availability_state"].as_str(),
        Some("disabled_config_error")
    );
}

#[tokio::test]
#[cfg(not(unix))]
async fn test_gh_monitor_non_unix_returns_unsupported_platform() {
    let req_json = r#"{"version":1,"request_id":"r-gh-stub","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"run","target":"1"}}"#;
    let monitor_resp = handle_gh_monitor_command(req_json, std::path::Path::new(".")).await;
    assert_eq!(monitor_resp.status, "error");
    let error = monitor_resp.error.unwrap();
    assert_eq!(error.code, "UNSUPPORTED_PLATFORM");
}

#[tokio::test]
#[cfg(not(unix))]
async fn test_gh_status_non_unix_returns_unsupported_platform() {
    let req_json = r#"{"version":1,"request_id":"r-gh-status-stub","command":"gh-status","payload":{"team":"atm-dev","target_kind":"run","target":"1"}}"#;
    let status_resp = handle_gh_status_command(req_json, std::path::Path::new(".")).await;
    assert_eq!(status_resp.status, "error");
    let error = status_resp.error.unwrap();
    assert_eq!(error.code, "UNSUPPORTED_PLATFORM");
}

#[tokio::test]
#[cfg(not(unix))]
async fn test_gh_monitor_control_non_unix_returns_unsupported_platform() {
    let req_json = r#"{"version":1,"request_id":"r-gh-control-stub","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"stop"}}"#;
    let resp = handle_gh_monitor_control_command(req_json, std::path::Path::new(".")).await;
    assert_eq!(resp.status, "error");
    let error = resp.error.unwrap();
    assert_eq!(error.code, "UNSUPPORTED_PLATFORM");
}

#[tokio::test]
#[cfg(not(unix))]
async fn test_gh_monitor_health_non_unix_returns_unsupported_platform() {
    let req_json = r#"{"version":1,"request_id":"r-gh-health-stub","command":"gh-monitor-health","payload":{"team":"atm-dev"}}"#;
    let resp = handle_gh_monitor_health_command(req_json, std::path::Path::new(".")).await;
    assert_eq!(resp.status, "error");
    let error = resp.error.unwrap();
    assert_eq!(error.code, "UNSUPPORTED_PLATFORM");
}
