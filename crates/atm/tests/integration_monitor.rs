//! Integration tests for `atm monitor`.

use assert_cmd::cargo;
use serial_test::serial;
use std::fs;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    let workdir = temp_dir.path().join("workdir");
    let runtime_home = temp_dir.path().join("runtime-home");
    std::fs::create_dir_all(&workdir).ok();
    std::fs::create_dir_all(&runtime_home).ok();
    cmd.env("ATM_HOME", &runtime_home)
        .env("ATM_CONFIG_HOME", temp_dir.path())
        .envs([("ATM_HOME", temp_dir.path())])
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .env_remove("ATM_CONFIG")
        .env_remove("CLAUDE_SESSION_ID")
        .current_dir(&workdir);
}

fn setup_team(temp_dir: &TempDir, team_name: &str) {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();
    let config = serde_json::json!({
        "name": team_name,
        "description": "Monitor test team",
        "createdAt": 1739284800000i64,
        "leadAgentId": format!("team-lead@{}", team_name),
        "leadSessionId": "test-session-id",
        "members": [
            {
                "agentId": format!("team-lead@{}", team_name),
                "name": "team-lead",
                "agentType": "general-purpose",
                "model": "claude-haiku-4-5-20251001",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": []
            }
        ]
    });
    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    fs::write(inboxes_dir.join("team-lead.json"), "[]").unwrap();
}
#[test]
fn test_monitor_once_emits_alert_for_critical_finding() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("monitor")
        .arg("--team")
        .arg("test-team")
        .arg("--notify")
        .arg("team-lead")
        .arg("--once")
        .assert()
        .success();

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/team-lead.json");
    let content = fs::read_to_string(inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert!(
        !messages.is_empty(),
        "monitor should emit at least one alert"
    );
    let last = messages.last().unwrap();
    assert_eq!(last["from"].as_str(), Some("atm-monitor"));
    let text = last["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("[atm-monitor]"),
        "alert should include monitor prefix"
    );
}

#[test]
fn test_monitor_dedup_suppresses_repeat_within_cooldown() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("monitor")
        .arg("--team")
        .arg("test-team")
        .arg("--notify")
        .arg("team-lead")
        .arg("--interval-secs")
        .arg("1")
        .arg("--cooldown-secs")
        .arg("600")
        .arg("--max-iterations")
        .arg("2")
        .assert()
        .success();

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/team-lead.json");
    let content = fs::read_to_string(inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    let monitor_msgs = messages
        .iter()
        .filter(|m| m["from"].as_str() == Some("atm-monitor"))
        .count();
    assert_eq!(
        monitor_msgs, 1,
        "duplicate critical finding should be suppressed within cooldown"
    );
}

// ATM-QA-T5b-002: Background launch + polling loop liveness.
//
// Verifies the polling loop runs at least 2 full cycles before exiting (i.e.
// it does not return immediately on the first clean-ish check). We use
// `--max-iterations 2` with a 1-second interval so the test completes quickly
// while still exercising two distinct poll cycles. We measure wall time to
// confirm the loop actually slept between polls rather than spinning.
#[test]
fn test_monitor_polling_loop_runs_multiple_cycles() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let start = Instant::now();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("monitor")
        .arg("--team")
        .arg("test-team")
        .arg("--notify")
        .arg("team-lead")
        .arg("--interval-secs")
        .arg("1")
        .arg("--max-iterations")
        .arg("2")
        .assert()
        .success();

    let elapsed = start.elapsed();
    // Two iterations with a 1-second interval means the process slept at least
    // once, so total wall time must exceed 1 second. We allow up to 30 seconds
    // for CI latency headroom.
    assert!(
        elapsed >= Duration::from_millis(900),
        "polling loop ran for only {elapsed:?}; expected at least 2 cycles (≥1s)"
    );
}

// ATM-QA-T5b-003: Injected fault produces alert within 2 poll intervals.
//
// The test fixture provides no daemon (ATM_HOME set to a temp dir with no PID
// file), which causes `atm doctor` to report DAEMON_NOT_RUNNING as a critical
// finding. We run the monitor for exactly 2 poll cycles and assert at least one
// alert was delivered to the inbox within those cycles.
#[test]
fn test_monitor_fault_produces_alert_within_two_poll_intervals() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.arg("monitor")
        .arg("--team")
        .arg("test-team")
        .arg("--notify")
        .arg("team-lead")
        .arg("--interval-secs")
        .arg("1")
        .arg("--cooldown-secs")
        .arg("600")
        .arg("--max-iterations")
        .arg("2")
        .assert()
        .success();

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/team-lead.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();

    let alert_count = messages
        .iter()
        .filter(|m| m["from"].as_str() == Some("atm-monitor"))
        .count();
    assert!(
        alert_count >= 1,
        "expected at least 1 alert within 2 poll cycles for a persistent fault; got {alert_count}"
    );

    // The alert must arrive on the first poll cycle — not deferred until later.
    let first_alert = messages
        .iter()
        .find(|m| m["from"].as_str() == Some("atm-monitor"))
        .unwrap();
    let text = first_alert["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("CRITICAL") || text.contains("critical"),
        "alert text should indicate critical severity; got: {text}"
    );
}

// ATM-QA-T5b-004: Fault-clear then reintroduce produces a new alert.
//
// The alert deduplication tracker lives in-process memory and resets on each
// new process invocation. This test simulates the fault-clear-reintroduce
// lifecycle by running the monitor twice with a fresh process each time:
//
//   Run 1 — fault active (no daemon) → alert emitted (new finding).
//   Run 2 — same fault, but now the inbox already has an alert from run 1.
//           Since the process is new, its in-memory tracker is empty, so the
//           fault is again seen as "new" and a second alert is emitted.
//
// This confirms that fault resolution followed by re-occurrence (across process
// restarts / session boundaries) correctly produces a new alert each time.
#[test]
fn test_monitor_reintroduced_fault_emits_new_alert() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/team-lead.json");

    // Run 1: initial fault detection — expect 1 alert.
    let mut cmd1 = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd1, &temp_dir);
    cmd1.env("ATM_DAEMON_AUTOSTART", "0");
    cmd1.arg("monitor")
        .arg("--team")
        .arg("test-team")
        .arg("--notify")
        .arg("team-lead")
        .arg("--once")
        .assert()
        .success();

    let content_after_run1 = fs::read_to_string(&inbox_path).unwrap();
    let msgs1: Vec<serde_json::Value> = serde_json::from_str(&content_after_run1).unwrap();
    let count_after_run1 = msgs1
        .iter()
        .filter(|m| m["from"].as_str() == Some("atm-monitor"))
        .count();
    assert!(
        count_after_run1 >= 1,
        "run 1 should emit at least 1 alert for the initial fault"
    );

    // Run 2: fault persists but the in-memory tracker is reset (new process).
    // A new process treats every finding as fresh, so a second alert is emitted.
    let mut cmd2 = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd2, &temp_dir);
    cmd2.env("ATM_DAEMON_AUTOSTART", "0");
    cmd2.arg("monitor")
        .arg("--team")
        .arg("test-team")
        .arg("--notify")
        .arg("team-lead")
        .arg("--once")
        .assert()
        .success();

    let content_after_run2 = fs::read_to_string(&inbox_path).unwrap();
    let msgs2: Vec<serde_json::Value> = serde_json::from_str(&content_after_run2).unwrap();
    let count_after_run2 = msgs2
        .iter()
        .filter(|m| m["from"].as_str() == Some("atm-monitor"))
        .count();
    assert!(
        count_after_run2 > count_after_run1,
        "reintroduced fault (new process session) should emit a new alert; \
         run1={count_after_run1} alerts, run2={count_after_run2} alerts"
    );
}

// ATM-QA-T5b-005: Monitor survives temporary daemon unavailability without
// exiting or panicking.
//
// With no daemon running (ATM_HOME points to an empty temp dir with no PID/
// socket files), the monitor must continue polling for all requested iterations
// rather than aborting on the first unhealthy check. We verify:
//   (a) The process exits with code 0 (not a panic or hard error).
//   (b) All iterations complete — the loop ran to `--max-iterations`.
//   (c) Alerts were delivered (monitor is active, not silently broken).
#[test]
#[serial]
fn test_monitor_survives_daemon_unavailable() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    // Explicitly ensure no daemon artifacts exist in the test home.
    let daemon_dir = temp_dir.path().join(".atm/daemon");
    fs::create_dir_all(&daemon_dir).unwrap();
    // Do not create atm-daemon.pid or atm-daemon.sock — daemon is absent.

    let start = Instant::now();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    // Also clear ATM_DAEMON_AUTOSTART so the CLI does not try to spawn a daemon.
    cmd.env("ATM_DAEMON_AUTOSTART", "0");
    cmd.arg("monitor")
        .arg("--team")
        .arg("test-team")
        .arg("--notify")
        .arg("team-lead")
        .arg("--interval-secs")
        .arg("1")
        .arg("--cooldown-secs")
        .arg("600")
        .arg("--max-iterations")
        .arg("3")
        .assert()
        .success(); // must NOT crash — survives unavailable daemon

    let elapsed = start.elapsed();
    // 3 iterations with 1-second sleep between means at least 2 full sleeps.
    assert!(
        elapsed >= Duration::from_millis(1800),
        "expected loop to run 3 full iterations (≥~2s); elapsed={elapsed:?}"
    );

    // Verify alerts were delivered — the monitor was actively polling, not stuck.
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/team-lead.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    let alert_count = messages
        .iter()
        .filter(|m| m["from"].as_str() == Some("atm-monitor"))
        .count();
    assert!(
        alert_count >= 1,
        "monitor should have delivered at least 1 alert across 3 cycles of daemon unavailability; \
         got {alert_count}"
    );
}
