//! Conflict & Edge Case Integration Tests
//!
//! Tests for concurrent writes, lock contention, spool drain cycles,
//! malformed JSON recovery, large inbox performance, missing files,
//! and permission-denied scenarios.

use assert_cmd::cargo;
use serial_test::serial;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;
#[path = "support/daemon_process_guard.rs"]
mod daemon_process_guard;
#[path = "support/daemon_test_registry.rs"]
mod daemon_test_registry;
#[path = "support/env_guard.rs"]
mod env_guard;
use daemon_process_guard::{DaemonProcessGuard, daemon_binary_path, pid_alive, wait_for_pid_exit};
use env_guard::EnvGuard;

/// Helper to set home directory for cross-platform test compatibility.
/// Uses `ATM_HOME` which is checked first by `get_home_dir()`, avoiding
/// platform-specific differences in how `dirs::home_dir()` resolves.
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    set_home_env_path(cmd, temp_dir.path());
}

fn set_home_env_path(cmd: &mut assert_cmd::Command, home: &std::path::Path) {
    let runtime_home = home.join("runtime-home");
    // Use a subdirectory as CWD to avoid:
    // 1. .atm.toml config leak from the repo root
    // 2. auto-identity CWD matching against team member CWD (ATM_HOME root)
    let workdir = home.join("workdir");
    std::fs::create_dir_all(&workdir).ok();
    std::fs::create_dir_all(&runtime_home).ok();
    cmd.env("ATM_HOME", &runtime_home)
        .env("HOME", home)
        // Prevent opportunistic daemon autostart from changing expected
        // offline/online label behavior in deterministic integration tests.
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .env_remove("ATM_CONFIG")
        .env_remove("CLAUDE_SESSION_ID")
        .current_dir(&workdir);
}

#[cfg(unix)]
/// Guards cleanup of a daemon registered via PID file, not a direct `Child` handle.
struct RuntimeDaemonCleanupGuard {
    home: PathBuf,
    daemon_guard: Option<DaemonProcessGuard>,
}

#[cfg(unix)]
impl RuntimeDaemonCleanupGuard {
    fn new(temp_dir: &TempDir) -> Self {
        daemon_test_registry::sweep_stale_test_daemons();
        Self {
            home: temp_dir.path().join("runtime-home"),
            daemon_guard: None,
        }
    }

    fn adopt_running_pid(
        &mut self,
        daemon_bin: &std::path::Path,
        timeout: Duration,
    ) -> Option<u32> {
        if let Some(existing) = self.daemon_guard.as_ref() {
            return Some(existing.pid());
        }

        let pid_path = self.home.join(".atm/daemon/atm-daemon.pid");
        self.daemon_guard =
            DaemonProcessGuard::adopt_from_pid_file(&pid_path, daemon_bin, &self.home, timeout);
        self.daemon_guard.as_ref().map(DaemonProcessGuard::pid)
    }
}

#[cfg(unix)]
impl Drop for RuntimeDaemonCleanupGuard {
    fn drop(&mut self) {
        drop(self.daemon_guard.take());
        daemon_test_registry::sweep_stale_test_daemons();
    }
}

/// Create a test team structure with multiple agents
fn setup_test_team(temp_dir: &TempDir, team_name: &str) -> PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");

    fs::create_dir_all(&inboxes_dir).unwrap();

    // isActive values in test fixtures are activity hints only; liveness is daemon-derived.
    let config = serde_json::json!({
        "name": team_name,
        "description": "Conflict test team",
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
                "subscriptions": [],
                "isActive": true
            },
            {
                "agentId": format!("agent-a@{}", team_name),
                "name": "agent-a",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%1",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": true
            },
            {
                "agentId": format!("agent-b@{}", team_name),
                "name": "agent-b",
                "agentType": "general-purpose",
                "model": "claude-sonnet-4-5-20250929",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%2",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": true
            }
        ]
    });

    let config_path = team_dir.join("config.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    team_dir
}

#[cfg(unix)]
fn mirror_team_config_to_home(temp_dir: &TempDir, team_name: &str, home_root: &std::path::Path) {
    let source_team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let target_team_dir = home_root.join(".claude/teams").join(team_name);
    fs::create_dir_all(target_team_dir.join("inboxes")).expect("create mirrored team inbox dir");
    fs::copy(
        source_team_dir.join("config.json"),
        target_team_dir.join("config.json"),
    )
    .expect("copy mirrored team config");
}

#[cfg(unix)]
fn daemon_pid_path(temp_dir: &TempDir) -> PathBuf {
    temp_dir
        .path()
        .join("runtime-home/.atm/daemon/atm-daemon.pid")
}

#[cfg(unix)]
fn read_daemon_pid(temp_dir: &TempDir) -> Option<u32> {
    let pid_path = daemon_pid_path(temp_dir);
    let raw = fs::read_to_string(pid_path).ok()?;
    raw.trim().parse::<u32>().ok()
}

#[cfg(unix)]
fn wait_for_daemon_pid_change(temp_dir: &TempDir, previous_pid: u32, timeout: Duration) -> u32 {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(pid) = read_daemon_pid(temp_dir)
            && pid != previous_pid
            && pid_alive(pid as i32)
        {
            return pid;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("daemon pid did not change from {previous_pid} within {timeout:?}");
}

#[cfg(unix)]
fn write_lock_metadata(temp_dir: &TempDir, pid: u32, home_scope: String, executable_path: String) {
    let metadata_path = temp_dir
        .path()
        .join("runtime-home/.atm/daemon/daemon.lock.meta.json");
    if let Some(parent) = metadata_path.parent() {
        fs::create_dir_all(parent).expect("create metadata dir");
    }
    let payload = serde_json::json!({
        "pid": pid,
        "home_scope": home_scope,
        "executable_path": executable_path,
        "version": env!("CARGO_PKG_VERSION"),
        "written_at": "2026-01-01T00:00:00Z",
    });
    fs::write(
        metadata_path,
        serde_json::to_string_pretty(&payload).expect("serialize metadata"),
    )
    .expect("write metadata");
}

// ============================================================================
// Category 1: Concurrent Write Tests
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn test_concurrent_sends_no_data_loss() {
    // Still serialized: DaemonProcessGuard records spawned daemon PIDs in the
    // shared test registry, which remains process-global in AP.3.
    // Root-cause note for #372: CI hangs were traced to harness lifecycle
    // nondeterminism (startup/teardown timing), not a confirmed production
    // data-loss defect in send/inbox persistence.
    // Multi-threaded test: simulate atm CLI and Claude Code writing to same inbox
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");
    let mut daemon_guard = DaemonProcessGuard::spawn(&temp_dir, "test-team");
    let daemon_pid = daemon_guard.pid();
    assert!(daemon_pid > 1, "daemon must expose a stable known PID");
    daemon_guard.wait_ready(&temp_dir);

    let num_senders = 5;
    let messages_per_sender = 4;
    let expected = (num_senders * messages_per_sender) as usize;

    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).unwrap();
    let atm_bin = cargo::cargo_bin!("atm");

    // Send messages in parallel using async tasks and bound the full send phase.
    let mut senders = tokio::task::JoinSet::new();

    for sender_id in 0..num_senders {
        let temp_path = temp_dir.path().to_path_buf();
        let runtime_home = temp_path.join("runtime-home");
        let workdir_path = workdir.clone();
        let atm_bin_path = atm_bin.to_path_buf();
        senders.spawn(async move {
            std::fs::create_dir_all(&runtime_home).expect("create runtime home for sender");
            for msg_id in 0..messages_per_sender {
                let mut attempts = 0u8;
                loop {
                    attempts += 1;
                    let mut cmd = tokio::process::Command::new(&atm_bin_path);
                    cmd.env("ATM_HOME", &runtime_home)
                        .env("HOME", &temp_path)
                        .env("ATM_DAEMON_AUTOSTART", "0")
                        .env_remove("ATM_CONFIG")
                        .env_remove("CLAUDE_SESSION_ID")
                        .env("ATM_TEAM", "test-team")
                        .env("ATM_IDENTITY", format!("sender-{sender_id}"))
                        .current_dir(&workdir_path);
                    cmd.arg("send")
                        .arg("agent-a")
                        .arg(format!("Message {msg_id} from sender {sender_id}"));
                    let output = tokio::time::timeout(Duration::from_secs(10), cmd.output())
                        .await
                        .map_err(|_| {
                            format!(
                                "atm send timeout for sender={sender_id} msg={msg_id} (daemon pid={daemon_pid})"
                            )
                        })?
                        .map_err(|e| {
                            format!("failed to execute atm send for sender={sender_id}: {e}")
                        })?;
                    if output.status.success() {
                        break;
                    }

                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let transient_missing_config = stderr.contains("Team config not found");
                    let transient_missing_file = stderr.contains("(os error 2)")
                        || stderr.contains("No such file or directory")
                        || stderr.contains("The system cannot find the file specified");
                    if (transient_missing_config || transient_missing_file) && attempts < 6 {
                        // Retry backoff only: this gives the daemon time to finish
                        // startup/config materialization, not a timing fence for correctness.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    return Err(format!(
                        "atm send failed for sender={sender_id} msg={msg_id} attempts={attempts}: stderr={stderr} stdout={stdout}",
                    ));
                }
            }
            Ok::<(), String>(())
        });
    }

    let send_phase = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(joined) = senders.join_next().await {
            match joined {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(e),
                Err(e) => return Err(format!("sender task failed: {e}")),
            }
        }
        Ok::<(), String>(())
    })
    .await;

    match send_phase {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("concurrent send phase failed: {e}"),
        Err(_) => {
            senders.abort_all();
            panic!("concurrent send phase timed out after 60s");
        }
    }

    // Deterministic drain convergence with bounded wall-clock timeout.
    let teams_dir = temp_dir.path().join(".claude/teams");
    let spool_base = temp_dir.path();
    let drain_timeout_secs = if cfg!(windows) { 20 } else { 10 };
    let deadline = Instant::now() + Duration::from_secs(drain_timeout_secs);
    let mut status =
        agent_team_mail_core::io::spool::spool_drain_with_base(&teams_dir, Some(spool_base))
            .unwrap();
    while status.pending > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        status =
            agent_team_mail_core::io::spool::spool_drain_with_base(&teams_dir, Some(spool_base))
                .unwrap();
    }
    assert_eq!(
        status.failed, 0,
        "Expected no spool drain failures, got: {:?}",
        status
    );
    assert_eq!(
        status.pending, 0,
        "Expected no pending spool entries after bounded drain retries, got: {:?}",
        status
    );

    // Verify: all messages should be in the inbox (possibly some in spool)
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");

    assert!(inbox_path.exists(), "Inbox file should exist after sends");
    let read_messages = || -> Vec<serde_json::Value> {
        let content = fs::read_to_string(&inbox_path).unwrap();
        serde_json::from_str(&content).unwrap()
    };
    let mut messages = read_messages();
    let delivery_timeout_secs = if cfg!(windows) { 30 } else { 15 };
    let delivery_deadline = Instant::now() + Duration::from_secs(delivery_timeout_secs);
    while messages.len() < expected && Instant::now() < delivery_deadline {
        std::thread::sleep(Duration::from_millis(50));
        let _ =
            agent_team_mail_core::io::spool::spool_drain_with_base(&teams_dir, Some(spool_base))
                .unwrap();
        messages = read_messages();
    }

    assert_eq!(
        messages.len(),
        expected,
        "Expected all messages to be delivered after bounded convergence window"
    );

    // Verify no duplicate message IDs
    let msg_ids: Vec<&str> = messages
        .iter()
        .filter_map(|m| m["message_id"].as_str())
        .collect();
    let unique_ids: std::collections::HashSet<&str> = msg_ids.iter().copied().collect();
    assert_eq!(
        msg_ids.len(),
        unique_ids.len(),
        "No duplicate message_ids should exist"
    );
}

#[test]
#[serial]
fn test_concurrent_cli_and_direct_write_no_loss() {
    // Still serialized: RuntimeDaemonCleanupGuard sweeps the per-binary daemon
    // registry on entry/drop and must not race with live daemon-backed tests.
    // Simulate CLI and Claude Code writing simultaneously
    use std::sync::{Arc, Barrier};

    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");
    #[cfg(unix)]
    let mut daemon_cleanup = RuntimeDaemonCleanupGuard::new(&temp_dir);

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");

    // Seed inbox with initial message
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "team-lead")
        .arg("send")
        .arg("agent-a")
        .arg("Initial message")
        .assert()
        .success();

    let barrier = Arc::new(Barrier::new(2));
    let temp_path = temp_dir.path().to_path_buf();

    // Thread 1: CLI send
    let barrier1 = Arc::clone(&barrier);
    let temp_path1 = temp_path.clone();
    let handle1 = std::thread::spawn(move || {
        barrier1.wait();
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env_path(&mut cmd, &temp_path1);
        cmd.env("ATM_TEAM", "test-team");
        cmd.env("ATM_IDENTITY", "cli-sender");
        cmd.arg("send").arg("agent-a").arg("CLI message");
        cmd.assert().success();
    });

    // Thread 2: CLI send (simulating Claude Code sending at same time)
    let barrier2 = Arc::clone(&barrier);
    let handle2 = std::thread::spawn(move || {
        barrier2.wait();
        let mut cmd = cargo::cargo_bin_cmd!("atm");
        set_home_env_path(&mut cmd, &temp_path);
        cmd.env("ATM_TEAM", "test-team");
        cmd.env("ATM_IDENTITY", "claude-code");
        cmd.arg("send").arg("agent-a").arg("Claude Code message");
        cmd.assert().success();
    });

    handle1.join().unwrap();
    handle2.join().unwrap();
    // Best-effort adoption: daemon may not have auto-started for these file-based operations.
    // adopt_running_pid returns None if no PID file was written; RuntimeDaemonCleanupGuard's
    // Drop sweep still attempts cleanup of any leaked daemon processes.
    #[cfg(unix)]
    let _adopted =
        daemon_cleanup.adopt_running_pid(&daemon_binary_path(), Duration::from_millis(250));

    // Verify all messages are present
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();

    // Should have at least 2 messages (initial + at least one concurrent)
    // Both concurrent sends should succeed due to atomic write + merge
    assert!(
        messages.len() >= 2,
        "Expected at least 2 messages, got {}",
        messages.len()
    );
}

// ============================================================================
// Category 2: Lock Contention Tests
// ============================================================================

#[test]
fn test_lock_contention_queues_to_spool() {
    // When the lock can't be acquired, messages should be spooled
    // We simulate this by having one thread hold the lock while another sends
    use agent_team_mail_core::io::lock::acquire_lock;

    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Create the inbox file and hold its lock
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");
    fs::write(&inbox_path, "[]").unwrap();

    let lock_path = inbox_path.with_extension("lock");
    let _held_lock = acquire_lock(&lock_path, 0).unwrap();

    // Try to send while lock is held - should queue to spool
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "team-lead")
        .arg("send")
        .arg("agent-a")
        .arg("Message during lock")
        .assert()
        .success();

    // Verify stderr warning about queuing
    let stderr = String::from_utf8(output.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("queued") || stderr.contains("Warning"),
        "Expected queue warning in stderr, got: {stderr}"
    );
}

// ============================================================================
// Category 3: Spool → Drain → Delivery Cycle
// ============================================================================

#[test]
#[serial]
fn test_spool_drain_delivery_cycle() {
    // Still serialized: this test mutates ATM_HOME process-wide while exercising
    // the spool path resolved from that shared env var.
    // This test uses the library API directly since spool_drain is not yet
    // exposed via CLI command.
    use agent_team_mail_core::io::inbox::inbox_append;
    use agent_team_mail_core::io::lock::acquire_lock;
    use agent_team_mail_core::schema::InboxMessage;
    use std::collections::HashMap;

    let temp_dir = TempDir::new().unwrap();
    // Use ATM_HOME to redirect spool dir — works cross-platform (dirs::config_dir()
    // ignores HOME/USERPROFILE on Windows)
    let _atm_home = EnvGuard::set("ATM_HOME", temp_dir.path());
    let teams_dir = temp_dir.path().join("teams");
    let team_dir = teams_dir.join("test-team");
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    let inbox_path = inboxes_dir.join("agent-a.json");
    fs::write(&inbox_path, "[]").unwrap();

    // Step 1: Hold the lock to force spool
    let lock_path = inbox_path.with_extension("lock");
    let held_lock = acquire_lock(&lock_path, 0).unwrap();

    // Step 2: Try to append message - should be queued
    let message = InboxMessage {
        from: "tester".to_string(),
        source_team: None,
        text: "Spooled message".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        read: false,
        summary: None,
        message_id: Some("spool-test-001".to_string()),
        unknown_fields: HashMap::new(),
    };

    let outcome = inbox_append(&inbox_path, &message, "test-team", "agent-a").unwrap();
    assert!(
        matches!(
            outcome,
            agent_team_mail_core::io::WriteOutcome::Queued { .. }
        ),
        "Expected Queued outcome when lock held"
    );

    // Step 3: Release the lock
    drop(held_lock);

    // Step 4: Drain the spool - message should be delivered
    // Note: spool_drain uses system-global spool dir, so other tests may have
    // queued messages. We verify our message was delivered rather than exact count.
    let drain_start = Instant::now();
    let status = agent_team_mail_core::io::spool::spool_drain(&teams_dir).unwrap();
    let drain_elapsed = drain_start.elapsed();
    assert!(
        status.delivered >= 1,
        "At least our spooled message should be delivered, got: {}",
        status.delivered
    );
    assert!(
        drain_elapsed < Duration::from_secs(5),
        "spool drain took too long: {drain_elapsed:?}"
    );

    // Step 5: Verify message is in inbox
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert!(
        messages.iter().any(|m| m["text"] == "Spooled message"),
        "Delivered message should be in inbox"
    );
}

// ============================================================================
// Category 4: Malformed JSON Recovery
// ============================================================================

#[test]
fn test_malformed_inbox_json_graceful_failure() {
    // Write corrupt JSON to inbox, then try to send - should fail gracefully
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Write malformed JSON to inbox
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");
    fs::write(&inbox_path, "{ this is not valid json ]]]").unwrap();

    // Try to send - should fail (not panic) since inbox can't be parsed
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("agent-a")
        .arg("Test message")
        .assert()
        .failure();
}

#[test]
fn test_empty_json_array_inbox_ok() {
    // Empty JSON array is valid - send should succeed
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");
    fs::write(&inbox_path, "[]").unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "team-lead")
        .arg("send")
        .arg("agent-a")
        .arg("Message after empty inbox")
        .assert()
        .success();

    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages.len(), 1);
}

#[test]
fn test_malformed_inbox_read_graceful() {
    // Read command on malformed inbox should handle error gracefully
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Write malformed JSON
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");
    fs::write(&inbox_path, "NOT JSON AT ALL").unwrap();

    // Read should fail gracefully (not panic)
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .assert()
        .failure();
}

#[test]
fn test_read_skips_malformed_records_and_legacy_content_alias() {
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");
    fs::write(
        &inbox_path,
        r#"[
            {"from":"team-lead","text":"good","timestamp":"2026-02-11T14:30:00Z","read":false,"message_id":"msg-1"},
            {"from":"broken","timestamp":"2026-02-11T14:31:00Z"},
            {"from":"legacy","content":"legacy content","timestamp":"2026-02-11T14:32:00Z","message_id":"msg-2"}
        ]"#,
    )
    .unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "team-lead")
        .arg("read")
        .arg("--json")
        .arg("--no-mark")
        .arg("--no-since-last-seen")
        .arg("agent-a");

    let output = cmd.output().expect("read command should run");
    assert!(
        output.status.success(),
        "read should skip malformed records: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let messages = parsed["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["text"], "legacy content");
    assert_eq!(messages[1]["text"], "good");
    assert_eq!(parsed["bucket_counts"]["unread"], 2);
    assert_eq!(messages[1]["read"], false);
}

// ============================================================================
// Category 5: Large Inbox Performance
// ============================================================================

#[test]
fn test_large_inbox_10k_messages() {
    // Verify no degradation with 10K+ messages in inbox
    use std::time::Instant;

    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Pre-populate inbox with 10K messages directly (faster than CLI)
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");

    let mut messages: Vec<serde_json::Value> = Vec::with_capacity(10_000);
    for i in 0..10_000 {
        messages.push(serde_json::json!({
            "from": format!("sender-{}", i % 100),
            "text": format!("Message number {i}"),
            "timestamp": format!("2026-02-11T{:02}:{:02}:{:02}Z", (i / 3600) % 24, (i / 60) % 60, i % 60),
            "read": i < 9000, // First 9000 are read, last 1000 unread
            "message_id": format!("msg-{i:05}"),
            "summary": format!("Message {i}")
        }));
    }
    fs::write(&inbox_path, serde_json::to_string(&messages).unwrap()).unwrap();

    // Timed send: appending to 10K inbox should complete in reasonable time
    let start = Instant::now();
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "team-lead")
        .arg("send")
        .arg("agent-a")
        .arg("Message to large inbox")
        .assert()
        .success();
    let send_elapsed = start.elapsed();

    // Verify message was appended
    let content = fs::read_to_string(&inbox_path).unwrap();
    let updated_messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(updated_messages.len(), 10_001);

    // Performance check: send should complete within 5 seconds even with 10K messages
    assert!(
        send_elapsed.as_secs() < 5,
        "Send to large inbox took {send_elapsed:?}, expected < 5s"
    );

    // Timed read: reading from 10K inbox should also be reasonable
    let start = Instant::now();
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "team-lead")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .arg("--limit")
        .arg("10")
        .assert()
        .success();
    let read_elapsed = start.elapsed();

    assert!(
        read_elapsed.as_secs() < 5,
        "Read from large inbox took {read_elapsed:?}, expected < 5s"
    );
}

// ============================================================================
// Category 6: Missing / Empty File Handling
// ============================================================================

#[test]
fn test_send_to_nonexistent_inbox_creates_file() {
    // First send to an agent with no existing inbox file
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");

    // Verify inbox does NOT exist yet
    assert!(!inbox_path.exists());

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "team-lead")
        .arg("send")
        .arg("agent-a")
        .arg("First message to new inbox")
        .assert()
        .success();

    // Verify inbox was created with the message
    assert!(inbox_path.exists());
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["text"], "First message to new inbox");
}

#[test]
fn test_read_nonexistent_inbox_graceful() {
    // Reading an agent's inbox when no inbox file exists
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Read agent-a's inbox - no file exists
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "team-lead")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a")
        .assert()
        .success();

    // Should succeed with no messages shown
}

#[test]
fn test_empty_inbox_file_read() {
    // Empty file (not valid JSON) should fail gracefully
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");
    fs::write(&inbox_path, "").unwrap();

    // Read should handle empty file - the behavior depends on the implementation
    // It may succeed (treating empty as no messages) or fail (invalid JSON)
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("read")
        .arg("--no-since-last-seen")
        .arg("agent-a");

    // We just verify it doesn't panic - either success or controlled error is OK
    let _output = cmd.output().expect("Command should not panic");
}

#[test]
fn test_status_with_missing_inboxes_dir() {
    // Status command when inboxes directory doesn't exist
    let temp_dir = TempDir::new().unwrap();
    let team_dir = temp_dir.path().join(".claude/teams/test-team");

    // Create team config but NO inboxes directory
    fs::create_dir_all(&team_dir).unwrap();
    let config = serde_json::json!({
        "name": "test-team",
        "description": "Test",
        "createdAt": 1739284800000i64,
        "leadAgentId": "lead@test-team",
        "leadSessionId": "test",
        "members": [
            {
                "agentId": "lead@test-team",
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

    // Status should succeed even without inboxes directory
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("status")
        .assert()
        .success();
}

// ============================================================================
// Category 7: Permission Denied Handling
// ============================================================================

#[cfg(unix)]
#[test]
fn test_permission_denied_lock_file_creation() {
    // Make lock file a directory so exclusive lock acquisition fails

    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Create inbox
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");
    fs::write(&inbox_path, "[]").unwrap();

    // Create lock file as a directory (can't open directory for write)
    let lock_path = inbox_path.with_extension("lock");
    fs::create_dir_all(&lock_path).unwrap();

    // Try to send - lock file is a directory, so open-for-write fails
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("agent-a")
        .arg("Should fail");

    let output = cmd.output().expect("Command should not panic");
    // The command may fail or succeed (if it falls through to spool) - just verify no panic
    // and if it succeeds, it was queued rather than written directly
    let _ = output;
}

#[cfg(unix)]
#[test]
#[serial]
fn test_permission_denied_inboxes_dir() {
    // Still serialized: RuntimeDaemonCleanupGuard sweeps the per-binary daemon
    // registry on entry/drop and must not race with live daemon-backed tests.
    // Make inboxes directory read-only, then try to send to new inbox
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");
    let mut daemon_cleanup = RuntimeDaemonCleanupGuard::new(&temp_dir);

    let inboxes_dir = temp_dir.path().join(".claude/teams/test-team/inboxes");

    // Make inboxes directory read-only
    fs::set_permissions(&inboxes_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    // Try to send - should fail (can't create lock file or inbox)
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("send")
        .arg("agent-a")
        .arg("Should fail");

    // Just verify it doesn't panic - it should fail with an error
    let output = cmd.output().expect("Command should not panic");
    assert!(
        !output.status.success(),
        "Send should fail when directory is read-only"
    );
    // Best-effort adoption: daemon may not have auto-started for this error-path test.
    // adopt_running_pid returns None if no PID file was written; RuntimeDaemonCleanupGuard's
    // Drop sweep still attempts cleanup of any leaked daemon processes.
    let _adopted =
        daemon_cleanup.adopt_running_pid(&daemon_binary_path(), Duration::from_millis(250));

    // Restore permissions for cleanup
    fs::set_permissions(&inboxes_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// ============================================================================
// Category 8: Message Deduplication Under Concurrency
// ============================================================================

#[test]
#[serial]
fn test_no_duplicate_message_ids_under_concurrent_sends() {
    // Still serialized: RuntimeDaemonCleanupGuard sweeps the per-binary daemon
    // registry on entry/drop and must not race with live daemon-backed tests.
    // Verify that concurrent sends to same inbox produce unique message IDs
    use std::sync::{Arc, Barrier};

    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");
    #[cfg(unix)]
    let mut daemon_cleanup = RuntimeDaemonCleanupGuard::new(&temp_dir);

    let num_threads = 4;
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut handles = Vec::new();

    for thread_id in 0..num_threads {
        let barrier = Arc::clone(&barrier);
        let temp_path = temp_dir.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            barrier.wait();
            let mut cmd = cargo::cargo_bin_cmd!("atm");
            set_home_env_path(&mut cmd, &temp_path);
            cmd.env("ATM_TEAM", "test-team");
            cmd.env("ATM_DAEMON_AUTOSTART", "0");
            cmd.env("ATM_IDENTITY", format!("thread-{thread_id}"));
            cmd.arg("send")
                .arg("agent-a")
                .arg(format!("Concurrent msg from thread {thread_id}"));
            cmd.assert().success();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }
    // Best-effort adoption: daemon may not have auto-started for these file-based concurrent sends.
    // adopt_running_pid returns None if no PID file was written; RuntimeDaemonCleanupGuard's
    // Drop sweep still attempts cleanup of any leaked daemon processes.
    #[cfg(unix)]
    let _adopted =
        daemon_cleanup.adopt_running_pid(&daemon_binary_path(), Duration::from_millis(50));

    // Check inbox for duplicates
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();

    let msg_ids: Vec<&str> = messages
        .iter()
        .filter_map(|m| m["message_id"].as_str())
        .collect();
    let unique_ids: std::collections::HashSet<&str> = msg_ids.iter().copied().collect();

    assert_eq!(
        msg_ids.len(),
        unique_ids.len(),
        "All message IDs should be unique, found {} messages with {} unique IDs",
        msg_ids.len(),
        unique_ids.len()
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn test_runtime_daemon_cleanup_guard_adopts_pid_written_after_creation() {
    let temp_dir = TempDir::new().unwrap();
    let mut daemon_cleanup = RuntimeDaemonCleanupGuard::new(&temp_dir);
    let daemon_dir = temp_dir.path().join("runtime-home/.atm/daemon");
    fs::create_dir_all(&daemon_dir).unwrap();

    let launcher = temp_dir.path().join("late-pid-launcher.sh");
    fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nset -eu\nmkdir -p \"{}\"\n(sleep 30) &\nbgpid=$!\nprintf '%s\\n' \"$bgpid\" > \"{}\"\nexit 0\n",
            daemon_dir.display(),
            daemon_dir.join("atm-daemon.pid").display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&launcher).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&launcher, perms).unwrap();
    }

    let status = Command::new(&launcher)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());

    let adopted_pid = daemon_cleanup
        .adopt_running_pid(&launcher, Duration::from_secs(1))
        .expect("expected cleanup guard to adopt late pid-file daemon");
    drop(daemon_cleanup);
    wait_for_pid_exit(adopted_pid as i32, Duration::from_secs(2));
}

// ============================================================================
// Category 9: Inbox Round-Trip Preservation
// ============================================================================

#[test]
fn test_unknown_fields_preserved_through_send() {
    // Inbox with unknown fields should preserve them after a new send
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    // Pre-populate inbox with message containing unknown fields
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");
    let initial = r#"[{
        "from": "old-sender",
        "text": "Existing message",
        "timestamp": "2026-02-11T10:00:00Z",
        "read": false,
        "futureField": "preserve this",
        "anotherUnknown": {"nested": true}
    }]"#;
    fs::write(&inbox_path, initial).unwrap();

    // Send a new message
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "team-lead")
        .arg("send")
        .arg("agent-a")
        .arg("New message")
        .assert()
        .success();

    // Verify unknown fields are preserved in original message
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert_eq!(messages.len(), 2);

    // Find the original message
    let original = messages
        .iter()
        .find(|m| m["from"] == "old-sender")
        .expect("Original message should exist");
    assert_eq!(original["futureField"], "preserve this");
    assert_eq!(original["anotherUnknown"]["nested"], true);
}

// ============================================================================
// Category 10: Edge Case - Team Config Scenarios
// ============================================================================

#[test]
fn test_inbox_command_with_no_messages_anywhere() {
    // Inbox command when all inboxes are empty or nonexistent
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
        .arg("inbox")
        .arg("--no-since-last-seen")
        .assert()
        .success();
}

#[test]
fn test_members_command_shows_correct_labels() {
    // Verify no-daemon context renders Unknown (liveness cannot be confirmed).
    let temp_dir = TempDir::new().unwrap();
    let team_dir = temp_dir.path().join(".claude/teams/test-team");
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    let config = serde_json::json!({
        "name": "test-team",
        "description": "Label test",
        "createdAt": 1739284800000i64,
        "leadAgentId": "lead@test-team",
        "leadSessionId": "test",
        "members": [
            {
                "agentId": "online@test-team",
                "name": "online-agent",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%1",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": true
            },
            {
                "agentId": "offline@test-team",
                "name": "offline-agent",
                "agentType": "general-purpose",
                "model": "claude-sonnet-4-5-20250929",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%2",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": false
            }
        ]
    });
    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .arg("members")
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Unknown"), "Should show Unknown label");
    assert!(!stdout.contains("Online"), "Should not show Online label");
    assert!(!stdout.contains("Offline"), "Should not show Offline label");
}

#[test]
fn test_status_command_shows_correct_labels() {
    // Verify no-daemon context renders Unknown (liveness cannot be confirmed).
    let temp_dir = TempDir::new().unwrap();
    let team_dir = temp_dir.path().join(".claude/teams/test-team");
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    let config = serde_json::json!({
        "name": "test-team",
        "description": "Status label test",
        "createdAt": 1739284800000i64,
        "leadAgentId": "lead@test-team",
        "leadSessionId": "test",
        "members": [
            {
                "agentId": "online@test-team",
                "name": "online-agent",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%1",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": true
            },
            {
                "agentId": "offline@test-team",
                "name": "offline-agent",
                "agentType": "general-purpose",
                "model": "claude-sonnet-4-5-20250929",
                "joinedAt": 1739284800000i64,
                "tmuxPaneId": "%2",
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": false
            }
        ]
    });
    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    let output = cmd
        .env("ATM_TEAM", "test-team")
        .arg("status")
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Unknown"), "Should show Unknown label");
    assert!(!stdout.contains("Online"), "Should not show Online label");
    assert!(!stdout.contains("Offline"), "Should not show Offline label");
}

#[test]
fn test_doctor_status_members_consistent_unknown_when_daemon_unreachable() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = temp_dir.path().join(".claude/teams/test-team");
    fs::create_dir_all(team_dir.join("inboxes")).unwrap();

    // `isActive` is an activity hint only and must never be treated as liveness.
    let config = serde_json::json!({
        "name": "test-team",
        "description": "Canonical consistency test",
        "createdAt": 1739284800000i64,
        "leadAgentId": "lead@test-team",
        "leadSessionId": "test",
        "members": [
            {
                "agentId": "online@test-team",
                "name": "online-agent",
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1739284800000i64,
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": true
            },
            {
                "agentId": "offline@test-team",
                "name": "offline-agent",
                "agentType": "general-purpose",
                "model": "claude-sonnet-4-5-20250929",
                "joinedAt": 1739284800000i64,
                "cwd": temp_dir.path().to_str().unwrap(),
                "subscriptions": [],
                "isActive": false
            }
        ]
    });
    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    let mut members_cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut members_cmd, &temp_dir);
    let members_output = members_cmd
        .env("ATM_TEAM", "test-team")
        .arg("members")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let members_json: serde_json::Value = serde_json::from_slice(&members_output).unwrap();

    let mut status_cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut status_cmd, &temp_dir);
    let status_output = status_cmd
        .env("ATM_TEAM", "test-team")
        .arg("status")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status_json: serde_json::Value = serde_json::from_slice(&status_output).unwrap();

    let members_liveness: std::collections::HashMap<String, serde_json::Value> = members_json
        .get("members")
        .and_then(|v| v.as_array())
        .unwrap()
        .iter()
        .map(|m| {
            (
                m.get("name").and_then(|v| v.as_str()).unwrap().to_string(),
                m.get("liveness")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            )
        })
        .collect();

    let status_liveness: std::collections::HashMap<String, serde_json::Value> = status_json
        .get("members")
        .and_then(|v| v.as_array())
        .unwrap()
        .iter()
        .map(|m| {
            (
                m.get("name").and_then(|v| v.as_str()).unwrap().to_string(),
                m.get("liveness")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            )
        })
        .collect();

    assert_eq!(
        members_liveness, status_liveness,
        "members/status should render the same daemon-derived liveness"
    );
    assert_eq!(
        members_liveness.get("online-agent"),
        Some(&serde_json::Value::Null),
        "daemon unavailable should map to Unknown/null, not Online"
    );
    assert_eq!(
        members_liveness.get("offline-agent"),
        Some(&serde_json::Value::Null),
        "isActive=false must not be interpreted as Offline/dead"
    );

    let mut doctor_cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut doctor_cmd, &temp_dir);
    let doctor_output = doctor_cmd
        .env("ATM_TEAM", "test-team")
        .arg("doctor")
        .arg("--team")
        .arg("test-team")
        .output()
        .unwrap();
    assert_eq!(
        doctor_output.status.code(),
        Some(2),
        "doctor should exit non-zero when critical findings exist"
    );
    let doctor_stdout = String::from_utf8(doctor_output.stdout).unwrap();

    assert!(doctor_stdout.contains("offline-agent"));
    assert!(doctor_stdout.contains("online-agent"));
    assert!(doctor_stdout.contains("Unknown"));
    assert!(doctor_stdout.contains("DAEMON_NOT_RUNNING"));
    assert!(
        doctor_stdout.contains("atm-daemon"),
        "doctor output should include actionable daemon-start recommendation"
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn test_dead_pid_stale_lock_starts_daemon_cleanly() {
    // Still serialized: restarted daemon PIDs are tracked in the shared test
    // registry until AP replaces that global file with a per-test-safe owner.
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let config_home = temp_dir.path().join("config-home");
    mirror_team_config_to_home(&temp_dir, "test-team", &config_home);
    let dead_pid = 999_991_u32;
    assert!(!pid_alive(dead_pid as i32), "fixture pid should be dead");

    let daemon_dir = temp_dir.path().join("runtime-home/.atm/daemon");
    fs::create_dir_all(&daemon_dir).unwrap();
    fs::write(daemon_dir.join("atm-daemon.pid"), format!("{dead_pid}\n")).unwrap();
    fs::write(
        daemon_dir.join("status.json"),
        serde_json::json!({
            "pid": dead_pid,
            "version": env!("CARGO_PKG_VERSION"),
        })
        .to_string(),
    )
    .unwrap();

    let home_scope = fs::canonicalize(temp_dir.path())
        .unwrap()
        .to_string_lossy()
        .to_string();
    write_lock_metadata(
        &temp_dir,
        dead_pid,
        home_scope,
        daemon_binary_path().to_string_lossy().to_string(),
    );
    let lock_path = temp_dir.path().join("runtime-home/.atm/daemon/daemon.lock");
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    fs::write(&lock_path, "stale").unwrap();

    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    let runtime_home = temp_dir.path().join("runtime-home");
    fs::create_dir_all(&runtime_home).unwrap();
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", &runtime_home)
        .env("HOME", &config_home)
        .env("ATM_DAEMON_AUTOSTART", "1")
        .env("ATM_TEAM", "test-team")
        .env("ATM_IDENTITY", "team-lead")
        .env_remove("ATM_CONFIG")
        .env_remove("CLAUDE_SESSION_ID")
        .current_dir(&workdir);
    cmd.arg("status")
        .arg("--team")
        .arg("test-team")
        .arg("--json")
        .assert()
        .success();

    let new_pid = wait_for_daemon_pid_change(&temp_dir, dead_pid, Duration::from_secs(5));
    assert!(new_pid > 1);
    let _restarted_daemon = DaemonProcessGuard::adopt_registered_pid(
        new_pid,
        &daemon_binary_path(),
        &temp_dir.path().join("runtime-home"),
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn test_identity_mismatch_socket_is_detected_and_restarted() {
    // Still serialized: ensure_daemon_running reads ATM_HOME / ATM_DAEMON_BIN
    // / ATM_DAEMON_AUTOSTART from process-global env and the restarted PID is
    // tracked in the shared daemon test registry.
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");

    let mut daemon_guard = DaemonProcessGuard::spawn(&temp_dir, "test-team");
    daemon_guard.wait_ready(&temp_dir);
    let old_pid = daemon_guard.pid();

    write_lock_metadata(
        &temp_dir,
        old_pid,
        std::env::temp_dir()
            .join("atm-mismatch-home")
            .to_string_lossy()
            .to_string(),
        daemon_binary_path().to_string_lossy().to_string(),
    );

    let daemon_bin = daemon_binary_path();
    let _atm_home = EnvGuard::set("ATM_HOME", temp_dir.path().join("runtime-home"));
    let _atm_daemon_bin = EnvGuard::set("ATM_DAEMON_BIN", &daemon_bin);
    let _atm_daemon_autostart = EnvGuard::set("ATM_DAEMON_AUTOSTART", "1");
    let ensure_result = agent_team_mail_core::daemon_client::ensure_daemon_running();
    ensure_result.expect("ensure_daemon_running should restart on identity mismatch");

    let new_pid = wait_for_daemon_pid_change(&temp_dir, old_pid, Duration::from_secs(8));
    assert!(new_pid > 1);
    let _restarted_daemon =
        DaemonProcessGuard::adopt_registered_pid(new_pid, &daemon_binary_path(), temp_dir.path());
}
