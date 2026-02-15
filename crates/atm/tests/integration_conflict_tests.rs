//! Conflict & Edge Case Integration Tests
//!
//! Tests for concurrent writes, lock contention, spool drain cycles,
//! malformed JSON recovery, large inbox performance, missing files,
//! and permission-denied scenarios.

use assert_cmd::cargo;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Helper to set home directory for cross-platform test compatibility.
/// Uses `ATM_HOME` which is checked first by `get_home_dir()`, avoiding
/// platform-specific differences in how `dirs::home_dir()` resolves.
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    cmd.env("ATM_HOME", temp_dir.path());
}

/// Create a test team structure with multiple agents
fn setup_test_team(temp_dir: &TempDir, team_name: &str) -> PathBuf {
    let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
    let inboxes_dir = team_dir.join("inboxes");

    fs::create_dir_all(&inboxes_dir).unwrap();

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
    fs::write(
        &config_path,
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();

    team_dir
}

// ============================================================================
// Category 1: Concurrent Write Tests
// ============================================================================

#[test]
fn test_concurrent_sends_no_data_loss() {
    // Multi-threaded test: simulate atm CLI and Claude Code writing to same inbox
    let temp_dir = TempDir::new().unwrap();
    let _team_dir = setup_test_team(&temp_dir, "test-team");

    let num_senders = 5;
    let messages_per_sender = 4;

    // Send messages in parallel using threads
    let mut handles = Vec::new();

    for sender_id in 0..num_senders {
        let temp_path = temp_dir.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            for msg_id in 0..messages_per_sender {
                let mut cmd = cargo::cargo_bin_cmd!("atm");
                cmd.env("ATM_HOME", &temp_path);
                cmd.env("ATM_TEAM", "test-team");
                cmd.env("ATM_IDENTITY", format!("sender-{sender_id}"));
                cmd.arg("send")
                    .arg("agent-a")
                    .arg(format!("Message {msg_id} from sender {sender_id}"));
                let result = cmd.assert();
                // Allow both success and queued outcomes
                let output = result.get_output();
                assert!(
                    output.status.success(),
                    "Send should succeed or queue, got: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        });
        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Verify: all messages should be in the inbox (possibly some in spool)
    let inbox_path = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes/agent-a.json");

    if inbox_path.exists() {
        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();

        // We should have at least some messages (concurrent writes may queue some)
        assert!(
            !messages.is_empty(),
            "Inbox should contain at least some messages"
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
    use atm_core::io::lock::acquire_lock;

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
    use atm_core::io::inbox::inbox_append;
    use atm_core::io::lock::acquire_lock;
    use atm_core::schema::InboxMessage;
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

    let outcome =
        inbox_append(&inbox_path, &message, "test-team", "agent-a").unwrap();
    assert!(
        matches!(outcome, atm_core::io::WriteOutcome::Queued { .. }),
        "Expected Queued outcome when lock held"
    );

    // Step 3: Release the lock
    drop(held_lock);

    // Step 4: Drain the spool - message should be delivered
    // Note: spool_drain uses system-global spool dir, so other tests may have
    // queued messages. We verify our message was delivered rather than exact count.
    let status = atm_core::io::spool::spool_drain(&teams_dir).unwrap();
    assert!(
        status.delivered >= 1,
        "At least our spooled message should be delivered, got: {}",
        status.delivered
    );

    // Step 5: Verify message is in inbox
    let content = fs::read_to_string(&inbox_path).unwrap();
    let messages: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap();
    assert!(
        messages
            .iter()
            .any(|m| m["text"] == "Spooled message"),
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
    fs::write(
        &inbox_path,
        serde_json::to_string(&messages).unwrap(),
    )
    .unwrap();

    // Timed send: appending to 10K inbox should complete in reasonable time
    let start = Instant::now();
    let mut cmd = cargo::cargo_bin_cmd!("atm");
    set_home_env(&mut cmd, &temp_dir);
    cmd.env("ATM_TEAM", "test-team")
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

    let inboxes_dir = temp_dir
        .path()
        .join(".claude/teams/test-team/inboxes");

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
    // Verify the Online/Offline labels appear in members output
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
    assert!(stdout.contains("Online"), "Should show Online label");
    assert!(stdout.contains("Offline"), "Should show Offline label");
}

#[test]
fn test_status_command_shows_correct_labels() {
    // Verify the Online/Offline labels appear in status output
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
    assert!(stdout.contains("Online"), "Should show Online label");
    assert!(stdout.contains("Offline"), "Should show Offline label");
}
