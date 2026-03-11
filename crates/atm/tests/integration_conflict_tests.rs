//! Conflict & Edge Case Integration Tests
//!
//! Tests for concurrent writes, lock contention, spool drain cycles,
//! malformed JSON recovery, large inbox performance, missing files,
//! and permission-denied scenarios.

use assert_cmd::cargo;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tempfile::TempDir;
#[path = "support/daemon_process_guard.rs"]
mod daemon_process_guard;
#[path = "support/daemon_test_registry.rs"]
mod daemon_test_registry;
use daemon_process_guard::DaemonProcessGuard;

/// Helper to set home directory for cross-platform test compatibility.
/// Uses `ATM_HOME` which is checked first by `get_home_dir()`, avoiding
/// platform-specific differences in how `dirs::home_dir()` resolves.
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    // Use a subdirectory as CWD to avoid:
    // 1. .atm.toml config leak from the repo root
    // 2. auto-identity CWD matching against team member CWD (temp_dir root)
    let workdir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&workdir).ok();
    cmd.env("ATM_HOME", temp_dir.path())
        // Prevent opportunistic daemon autostart from changing expected
        // offline/online label behavior in deterministic integration tests.
        .env("ATM_DAEMON_AUTOSTART", "0")
        .env_remove("ATM_TEAM")
        .env_remove("ATM_IDENTITY")
        .env_remove("ATM_CONFIG")
        .env_remove("CLAUDE_SESSION_ID")
        .current_dir(&workdir);
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
fn daemon_pid_path(temp_dir: &TempDir) -> PathBuf {
    temp_dir.path().join(".atm/daemon/atm-daemon.pid")
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
fn daemon_binary_path() -> PathBuf {
    let mut candidate = PathBuf::from(cargo::cargo_bin!("atm"));
    candidate.set_file_name("atm-daemon");
    candidate
}

#[cfg(unix)]
fn write_lock_metadata(temp_dir: &TempDir, pid: u32, home_scope: String, executable_path: String) {
    let metadata_path = temp_dir.path().join(".atm/daemon/daemon.lock.meta.json");
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

#[cfg(unix)]
fn cleanup_pid(pid: u32) {
    send_signal(pid as i32, 15);
    for _ in 0..20 {
        if !pid_alive(pid as i32) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    send_signal(pid as i32, 9);
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: signal 0 checks process existence.
    unsafe { kill(pid, 0) == 0 }
}

#[cfg(unix)]
fn send_signal(pid: i32, sig: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: best-effort test cleanup path.
    let _ = unsafe { kill(pid, sig) };
}

// ============================================================================
// Category 1: Concurrent Write Tests
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn test_concurrent_sends_no_data_loss() {
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
        let workdir_path = workdir.clone();
        let atm_bin_path = atm_bin.to_path_buf();
        senders.spawn(async move {
            for msg_id in 0..messages_per_sender {
                let mut attempts = 0u8;
                loop {
                    attempts += 1;
                    let mut cmd = tokio::process::Command::new(&atm_bin_path);
                    cmd.env("ATM_HOME", &temp_path)
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
    let deadline = Instant::now() + Duration::from_secs(10);
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
    let delivery_deadline = Instant::now() + Duration::from_secs(15);
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
fn test_concurrent_cli_and_direct_write_no_loss() {
    // Simulate CLI and Claude Code writing simultaneously
    use std::sync::{Arc, Barrier};

    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

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
        cmd.env("ATM_HOME", &temp_path1);
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
        cmd.env("ATM_HOME", &temp_path);
        cmd.env("ATM_TEAM", "test-team");
        cmd.env("ATM_IDENTITY", "claude-code");
        cmd.arg("send").arg("agent-a").arg("Claude Code message");
        cmd.assert().success();
    });

    handle1.join().unwrap();
    handle2.join().unwrap();

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
#[serial_test::serial]
fn test_spool_drain_delivery_cycle() {
    // This test uses the library API directly since spool_drain is not yet
    // exposed via CLI command.
    use agent_team_mail_core::io::inbox::inbox_append;
    use agent_team_mail_core::io::lock::acquire_lock;
    use agent_team_mail_core::schema::InboxMessage;
    use std::collections::HashMap;

    let temp_dir = TempDir::new().unwrap();
    // Use ATM_HOME to redirect spool dir — works cross-platform (dirs::config_dir()
    // ignores HOME/USERPROFILE on Windows)
    let prev_atm_home = std::env::var("ATM_HOME").ok();
    unsafe {
        std::env::set_var("ATM_HOME", temp_dir.path());
    }
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
    let status = agent_team_mail_core::io::spool::spool_drain(&teams_dir).unwrap();
    assert!(
        status.delivered >= 1,
        "At least our spooled message should be delivered, got: {}",
        status.delivered
    );

    // Step 5: Verify message is in inbox
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert!(
        messages.iter().any(|m| m["text"] == "Spooled message"),
        "Delivered message should be in inbox"
    );

    // Restore environment
    unsafe {
        match prev_atm_home {
            Some(val) => std::env::set_var("ATM_HOME", val),
            None => std::env::remove_var("ATM_HOME"),
        }
    }
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
fn test_permission_denied_inboxes_dir() {
    // Make inboxes directory read-only, then try to send to new inbox
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

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

    // Restore permissions for cleanup
    fs::set_permissions(&inboxes_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

// ============================================================================
// Category 8: Message Deduplication Under Concurrency
// ============================================================================

#[test]
fn test_no_duplicate_message_ids_under_concurrent_sends() {
    // Verify that concurrent sends to same inbox produce unique message IDs
    use std::sync::{Arc, Barrier};

    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let num_threads = 4;
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut handles = Vec::new();

    for thread_id in 0..num_threads {
        let barrier = Arc::clone(&barrier);
        let temp_path = temp_dir.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            barrier.wait();
            let mut cmd = cargo::cargo_bin_cmd!("atm");
            cmd.env("ATM_HOME", &temp_path);
            cmd.env("ATM_TEAM", "test-team");
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
#[serial_test::serial]
fn test_dead_pid_stale_lock_starts_daemon_cleanly() {
    let temp_dir = TempDir::new().unwrap();
    setup_test_team(&temp_dir, "test-team");
    let dead_pid = 999_991_u32;
    assert!(!pid_alive(dead_pid as i32), "fixture pid should be dead");

    let daemon_dir = temp_dir.path().join(".atm/daemon");
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
    let lock_path = temp_dir.path().join(".atm/daemon/daemon.lock");
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    fs::write(&lock_path, "stale").unwrap();

    let workdir = temp_dir.path().join("workdir");
    fs::create_dir_all(&workdir).unwrap();
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    cmd.env("ATM_HOME", temp_dir.path())
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
    daemon_test_registry::register_test_daemon(new_pid, &daemon_binary_path());
    cleanup_pid(new_pid);
    daemon_test_registry::unregister_test_daemon(new_pid);
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn test_identity_mismatch_socket_is_detected_and_restarted() {
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

    let old_home = std::env::var("ATM_HOME").ok();
    let old_daemon_bin = std::env::var("ATM_DAEMON_BIN").ok();
    let old_autostart = std::env::var("ATM_DAEMON_AUTOSTART").ok();
    unsafe {
        std::env::set_var("ATM_HOME", temp_dir.path());
        std::env::set_var("ATM_DAEMON_BIN", daemon_binary_path());
        std::env::set_var("ATM_DAEMON_AUTOSTART", "1");
    }
    let ensure_result = agent_team_mail_core::daemon_client::ensure_daemon_running();
    unsafe {
        match old_home {
            Some(v) => std::env::set_var("ATM_HOME", v),
            None => std::env::remove_var("ATM_HOME"),
        }
        match old_daemon_bin {
            Some(v) => std::env::set_var("ATM_DAEMON_BIN", v),
            None => std::env::remove_var("ATM_DAEMON_BIN"),
        }
        match old_autostart {
            Some(v) => std::env::set_var("ATM_DAEMON_AUTOSTART", v),
            None => std::env::remove_var("ATM_DAEMON_AUTOSTART"),
        }
    }
    ensure_result.expect("ensure_daemon_running should restart on identity mismatch");

    let new_pid = wait_for_daemon_pid_change(&temp_dir, old_pid, Duration::from_secs(8));
    assert!(new_pid > 1);
    daemon_test_registry::register_test_daemon(new_pid, &daemon_binary_path());
    cleanup_pid(new_pid);
    daemon_test_registry::unregister_test_daemon(new_pid);
}
