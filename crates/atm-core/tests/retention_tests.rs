//! Integration tests for retention policy implementation

use agent_team_mail_core::config::{CleanupStrategy, RetentionConfig};
use agent_team_mail_core::retention::apply_retention;
use agent_team_mail_core::schema::InboxMessage;
use chrono::{Duration, Utc};
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;

/// Helper to create a test message with a specific timestamp
fn create_test_message(
    from: &str,
    text: &str,
    days_ago: i64,
    message_id: Option<String>,
) -> InboxMessage {
    let timestamp = (Utc::now() - Duration::days(days_ago)).to_rfc3339();
    InboxMessage {
        from: from.to_string(),
        text: text.to_string(),
        timestamp,
        read: false,
        summary: None,
        message_id,
        unknown_fields: HashMap::new(),
    }
}

/// Helper to write messages to an inbox file
fn write_inbox(inbox_path: &std::path::Path, messages: &[InboxMessage]) {
    let json = serde_json::to_string_pretty(messages).unwrap();
    fs::write(inbox_path, json).unwrap();
}

/// Helper to read messages from an inbox file
fn read_inbox(inbox_path: &std::path::Path) -> Vec<InboxMessage> {
    let content = fs::read_to_string(inbox_path).unwrap();
    serde_json::from_str(&content).unwrap()
}

#[test]
fn test_retention_by_max_age() {
    let temp_dir = TempDir::new().unwrap();
    let inbox_path = temp_dir.path().join("agent.json");

    // Create messages: 3 old (10 days), 2 recent (3 days)
    let messages = vec![
        create_test_message("user1", "Old message 1", 10, Some("msg-001".to_string())),
        create_test_message("user2", "Old message 2", 10, Some("msg-002".to_string())),
        create_test_message("user3", "Old message 3", 10, Some("msg-003".to_string())),
        create_test_message("user4", "Recent message 1", 3, Some("msg-004".to_string())),
        create_test_message("user5", "Recent message 2", 3, Some("msg-005".to_string())),
    ];

    write_inbox(&inbox_path, &messages);

    // Apply retention: max_age = 7 days, delete strategy
    let policy = RetentionConfig {
        max_age: Some("7d".to_string()),
        max_count: None,
        strategy: CleanupStrategy::Delete,
        archive_dir: None,
    };

    let result = apply_retention(&inbox_path, "test-team", "test-agent", &policy, false).unwrap();

    // Verify result counts
    assert_eq!(result.kept, 2, "Should keep 2 recent messages");
    assert_eq!(result.removed, 3, "Should remove 3 old messages");
    assert_eq!(result.archived, 0, "No archiving with delete strategy");

    // Verify inbox file contains only recent messages
    let remaining = read_inbox(&inbox_path);
    assert_eq!(remaining.len(), 2);
    assert!(remaining.iter().any(|m| m.message_id == Some("msg-004".to_string())));
    assert!(remaining.iter().any(|m| m.message_id == Some("msg-005".to_string())));
}

#[test]
fn test_retention_by_max_count() {
    let temp_dir = TempDir::new().unwrap();
    let inbox_path = temp_dir.path().join("agent.json");

    // Create 10 messages, all recent
    let messages: Vec<InboxMessage> = (1..=10)
        .map(|i| create_test_message(&format!("user{i}"), &format!("Message {i}"), 1, Some(format!("msg-{i:03}"))))
        .collect();

    write_inbox(&inbox_path, &messages);

    // Apply retention: max_count = 5, delete strategy
    let policy = RetentionConfig {
        max_age: None,
        max_count: Some(5),
        strategy: CleanupStrategy::Delete,
        archive_dir: None,
    };

    let result = apply_retention(&inbox_path, "test-team", "test-agent", &policy, false).unwrap();

    // Verify result counts
    assert_eq!(result.kept, 5, "Should keep 5 messages");
    assert_eq!(result.removed, 5, "Should remove 5 messages");
    assert_eq!(result.archived, 0, "No archiving with delete strategy");

    // Verify inbox file contains only 5 messages
    let remaining = read_inbox(&inbox_path);
    assert_eq!(remaining.len(), 5);
}

#[test]
fn test_retention_combined_age_and_count() {
    let temp_dir = TempDir::new().unwrap();
    let inbox_path = temp_dir.path().join("agent.json");

    // Create messages: 5 old (10 days), 5 recent (3 days)
    let mut messages = Vec::new();
    for i in 1..=5 {
        messages.push(create_test_message(&format!("old-user{i}"), &format!("Old {i}"), 10, Some(format!("msg-old-{i:03}"))));
    }
    for i in 1..=5 {
        messages.push(create_test_message(&format!("new-user{i}"), &format!("Recent {i}"), 3, Some(format!("msg-new-{i:03}"))));
    }

    write_inbox(&inbox_path, &messages);

    // Apply retention: max_age = 7 days AND max_count = 3, delete strategy
    let policy = RetentionConfig {
        max_age: Some("7d".to_string()),
        max_count: Some(3),
        strategy: CleanupStrategy::Delete,
        archive_dir: None,
    };

    let result = apply_retention(&inbox_path, "test-team", "test-agent", &policy, false).unwrap();

    // All old messages removed by age (5), then count limit applies to recent messages (keep 3, remove 2)
    // Total: keep 3, remove 7
    assert_eq!(result.kept, 3, "Should keep 3 messages (count limit)");
    assert_eq!(result.removed, 7, "Should remove 7 messages (5 by age, 2 by count)");

    // Verify inbox file
    let remaining = read_inbox(&inbox_path);
    assert_eq!(remaining.len(), 3);
    // All remaining should be recent messages
    for msg in &remaining {
        assert!(msg.message_id.as_ref().unwrap().starts_with("msg-new-"));
    }
}

#[test]
fn test_archive_strategy() {
    let temp_dir = TempDir::new().unwrap();
    let inbox_path = temp_dir.path().join("agent.json");
    let archive_dir = temp_dir.path().join("archives");

    // Create messages: 3 old (10 days), 2 recent (3 days)
    let messages = vec![
        create_test_message("user1", "Old message 1", 10, Some("msg-001".to_string())),
        create_test_message("user2", "Old message 2", 10, Some("msg-002".to_string())),
        create_test_message("user3", "Old message 3", 10, Some("msg-003".to_string())),
        create_test_message("user4", "Recent message 1", 3, Some("msg-004".to_string())),
        create_test_message("user5", "Recent message 2", 3, Some("msg-005".to_string())),
    ];

    write_inbox(&inbox_path, &messages);

    // Apply retention: max_age = 7 days, archive strategy
    let policy = RetentionConfig {
        max_age: Some("7d".to_string()),
        max_count: None,
        strategy: CleanupStrategy::Archive,
        archive_dir: Some(archive_dir.to_str().unwrap().to_string()),
    };

    let result = apply_retention(&inbox_path, "test-team", "test-agent", &policy, false).unwrap();

    // Verify result counts
    assert_eq!(result.kept, 2, "Should keep 2 recent messages");
    assert_eq!(result.removed, 3, "Should remove 3 old messages");
    assert_eq!(result.archived, 3, "Should archive 3 messages");

    // Verify inbox file contains only recent messages
    let remaining = read_inbox(&inbox_path);
    assert_eq!(remaining.len(), 2);

    // Verify archive file exists and contains removed messages
    let archive_team_dir = archive_dir.join("test-team").join("test-agent");
    assert!(archive_team_dir.exists(), "Archive directory should exist");

    let archive_files: Vec<_> = fs::read_dir(&archive_team_dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(archive_files.len(), 1, "Should have one archive file");

    let archive_file = &archive_files[0].path();
    let archived_messages: Vec<InboxMessage> = serde_json::from_str(&fs::read_to_string(archive_file).unwrap()).unwrap();
    assert_eq!(archived_messages.len(), 3, "Archive should contain 3 messages");

    // Verify archived messages are the old ones
    for msg in &archived_messages {
        assert!(msg.message_id.as_ref().unwrap().starts_with("msg-00"));
    }
}

#[test]
fn test_delete_strategy() {
    let temp_dir = TempDir::new().unwrap();
    let inbox_path = temp_dir.path().join("agent.json");
    let archive_dir = temp_dir.path().join("archives");

    // Create messages: 3 old (10 days), 2 recent (3 days)
    let messages = vec![
        create_test_message("user1", "Old message 1", 10, Some("msg-001".to_string())),
        create_test_message("user2", "Old message 2", 10, Some("msg-002".to_string())),
        create_test_message("user3", "Old message 3", 10, Some("msg-003".to_string())),
        create_test_message("user4", "Recent message 1", 3, Some("msg-004".to_string())),
        create_test_message("user5", "Recent message 2", 3, Some("msg-005".to_string())),
    ];

    write_inbox(&inbox_path, &messages);

    // Apply retention: max_age = 7 days, delete strategy (explicit)
    let policy = RetentionConfig {
        max_age: Some("7d".to_string()),
        max_count: None,
        strategy: CleanupStrategy::Delete,
        archive_dir: Some(archive_dir.to_str().unwrap().to_string()),
    };

    let result = apply_retention(&inbox_path, "test-team", "test-agent", &policy, false).unwrap();

    // Verify result counts
    assert_eq!(result.kept, 2, "Should keep 2 recent messages");
    assert_eq!(result.removed, 3, "Should remove 3 old messages");
    assert_eq!(result.archived, 0, "No archiving with delete strategy");

    // Verify no archive directory created (delete strategy)
    assert!(!archive_dir.exists(), "Archive directory should not exist with delete strategy");
}

#[test]
fn test_dry_run() {
    let temp_dir = TempDir::new().unwrap();
    let inbox_path = temp_dir.path().join("agent.json");

    // Create messages: 3 old (10 days), 2 recent (3 days)
    let messages = vec![
        create_test_message("user1", "Old message 1", 10, Some("msg-001".to_string())),
        create_test_message("user2", "Old message 2", 10, Some("msg-002".to_string())),
        create_test_message("user3", "Old message 3", 10, Some("msg-003".to_string())),
        create_test_message("user4", "Recent message 1", 3, Some("msg-004".to_string())),
        create_test_message("user5", "Recent message 2", 3, Some("msg-005".to_string())),
    ];

    write_inbox(&inbox_path, &messages);

    // Apply retention: max_age = 7 days, delete strategy, DRY RUN
    let policy = RetentionConfig {
        max_age: Some("7d".to_string()),
        max_count: None,
        strategy: CleanupStrategy::Delete,
        archive_dir: None,
    };

    let result = apply_retention(&inbox_path, "test-team", "test-agent", &policy, true).unwrap();

    // Verify result counts reflect what would happen
    assert_eq!(result.kept, 2, "Should report 2 messages would be kept");
    assert_eq!(result.removed, 3, "Should report 3 messages would be removed");
    assert_eq!(result.archived, 0, "No archiving with delete strategy");

    // Verify inbox file is UNCHANGED
    let remaining = read_inbox(&inbox_path);
    assert_eq!(remaining.len(), 5, "Dry run should not modify inbox");
}

#[test]
fn test_empty_inbox() {
    let temp_dir = TempDir::new().unwrap();
    let inbox_path = temp_dir.path().join("agent.json");

    // Create empty inbox
    write_inbox(&inbox_path, &[]);

    // Apply retention
    let policy = RetentionConfig {
        max_age: Some("7d".to_string()),
        max_count: None,
        strategy: CleanupStrategy::Delete,
        archive_dir: None,
    };

    let result = apply_retention(&inbox_path, "test-team", "test-agent", &policy, false).unwrap();

    // Verify result counts
    assert_eq!(result.kept, 0);
    assert_eq!(result.removed, 0);
    assert_eq!(result.archived, 0);

    // Verify inbox still exists and is empty
    let remaining = read_inbox(&inbox_path);
    assert_eq!(remaining.len(), 0);
}

#[test]
fn test_nonexistent_inbox() {
    let temp_dir = TempDir::new().unwrap();
    let inbox_path = temp_dir.path().join("nonexistent.json");

    // Apply retention to non-existent inbox
    let policy = RetentionConfig {
        max_age: Some("7d".to_string()),
        max_count: None,
        strategy: CleanupStrategy::Delete,
        archive_dir: None,
    };

    let result = apply_retention(&inbox_path, "test-team", "test-agent", &policy, false).unwrap();

    // Verify result counts
    assert_eq!(result.kept, 0);
    assert_eq!(result.removed, 0);
    assert_eq!(result.archived, 0);
}

#[test]
fn test_no_data_loss_within_policy() {
    let temp_dir = TempDir::new().unwrap();
    let inbox_path = temp_dir.path().join("agent.json");

    // Create messages all within retention policy
    let messages = vec![
        create_test_message("user1", "Recent message 1", 1, Some("msg-001".to_string())),
        create_test_message("user2", "Recent message 2", 2, Some("msg-002".to_string())),
        create_test_message("user3", "Recent message 3", 3, Some("msg-003".to_string())),
    ];

    write_inbox(&inbox_path, &messages);

    // Apply retention: max_age = 7 days
    let policy = RetentionConfig {
        max_age: Some("7d".to_string()),
        max_count: None,
        strategy: CleanupStrategy::Delete,
        archive_dir: None,
    };

    let result = apply_retention(&inbox_path, "test-team", "test-agent", &policy, false).unwrap();

    // Verify NO messages removed
    assert_eq!(result.kept, 3, "All messages should be kept");
    assert_eq!(result.removed, 0, "No messages should be removed");
    assert_eq!(result.archived, 0);

    // Verify inbox unchanged
    let remaining = read_inbox(&inbox_path);
    assert_eq!(remaining.len(), 3);
}

#[test]
fn test_retention_hours_duration() {
    let temp_dir = TempDir::new().unwrap();
    let inbox_path = temp_dir.path().join("agent.json");

    // Create messages: one from 48 hours ago, one from 12 hours ago
    let timestamp_48h = (Utc::now() - Duration::hours(48)).to_rfc3339();
    let timestamp_12h = (Utc::now() - Duration::hours(12)).to_rfc3339();

    let messages = vec![
        InboxMessage {
            from: "user1".to_string(),
            text: "Old message".to_string(),
            timestamp: timestamp_48h,
            read: false,
            summary: None,
            message_id: Some("msg-001".to_string()),
            unknown_fields: HashMap::new(),
        },
        InboxMessage {
            from: "user2".to_string(),
            text: "Recent message".to_string(),
            timestamp: timestamp_12h,
            read: false,
            summary: None,
            message_id: Some("msg-002".to_string()),
            unknown_fields: HashMap::new(),
        },
    ];

    write_inbox(&inbox_path, &messages);

    // Apply retention: max_age = 24 hours
    let policy = RetentionConfig {
        max_age: Some("24h".to_string()),
        max_count: None,
        strategy: CleanupStrategy::Delete,
        archive_dir: None,
    };

    let result = apply_retention(&inbox_path, "test-team", "test-agent", &policy, false).unwrap();

    // Verify result counts
    assert_eq!(result.kept, 1, "Should keep 1 recent message (< 24h)");
    assert_eq!(result.removed, 1, "Should remove 1 old message (> 24h)");

    // Verify inbox file
    let remaining = read_inbox(&inbox_path);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].message_id, Some("msg-002".to_string()));
}

#[test]
fn test_no_retention_policy_keeps_all() {
    let temp_dir = TempDir::new().unwrap();
    let inbox_path = temp_dir.path().join("agent.json");

    // Create messages
    let messages = vec![
        create_test_message("user1", "Message 1", 100, Some("msg-001".to_string())),
        create_test_message("user2", "Message 2", 50, Some("msg-002".to_string())),
        create_test_message("user3", "Message 3", 1, Some("msg-003".to_string())),
    ];

    write_inbox(&inbox_path, &messages);

    // Apply retention with NO policy (both None)
    let policy = RetentionConfig {
        max_age: None,
        max_count: None,
        strategy: CleanupStrategy::Delete,
        archive_dir: None,
    };

    let result = apply_retention(&inbox_path, "test-team", "test-agent", &policy, false).unwrap();

    // Verify all messages kept
    assert_eq!(result.kept, 3, "Should keep all messages when no policy set");
    assert_eq!(result.removed, 0);
    assert_eq!(result.archived, 0);
}
