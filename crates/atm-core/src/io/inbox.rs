//! Inbox file operations with atomic writes and conflict detection

use crate::io::{atomic::atomic_swap, error::InboxError, hash::compute_hash, lock::acquire_lock};
use crate::schema::InboxMessage;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Outcome of an inbox write operation
#[derive(Debug, Clone, PartialEq)]
pub enum WriteOutcome {
    /// Clean write with no conflicts detected
    Success,

    /// Concurrent write detected and merged automatically
    ConflictResolved { merged_messages: usize },

    /// Could not write immediately, message queued for later delivery
    Queued { spool_path: PathBuf },
}

/// Atomically append a message to an inbox with conflict detection
///
/// This implements the atomic write strategy with lock, hash, swap, and
/// conflict merge. If the lock cannot be acquired, the message is spooled
/// for later delivery.
///
/// # Arguments
///
/// * `inbox_path` - Full path to inbox.json file
/// * `message` - Message to append
/// * `team` - Target team name (for spooling)
/// * `agent` - Target agent name (for spooling)
///
/// # Returns
///
/// * `Success` - Message written cleanly
/// * `ConflictResolved` - Concurrent write detected and merged
/// * `Queued` - Lock timeout, message spooled for retry
///
/// # Errors
///
/// Returns `InboxError` for I/O errors, JSON parse errors, or merge failures.
pub fn inbox_append(
    inbox_path: &Path,
    message: &InboxMessage,
    team: &str,
    agent: &str,
) -> Result<WriteOutcome, InboxError> {
    let msg_clone = message.clone();
    match atomic_write_with_conflict_check(inbox_path, |messages| {
        // Deduplication check
        if let Some(ref msg_id) = msg_clone.message_id
            && messages
                .iter()
                .any(|m| m.message_id.as_ref() == Some(msg_id))
        {
            return false;
        }
        messages.push(msg_clone);
        true
    }) {
        Ok(outcome) => Ok(outcome),
        Err(InboxError::LockTimeout { .. }) => {
            // Could not acquire lock - spool for later delivery
            let spool_path = crate::io::spool::spool_message(team, agent, message)?;
            Ok(WriteOutcome::Queued { spool_path })
        }
        Err(e) => Err(e),
    }
}

/// Atomically update messages in an inbox using a closure
///
/// Acquires the inbox lock, reads current messages, applies the update
/// closure, and writes back atomically with conflict detection.
///
/// # Arguments
///
/// * `inbox_path` - Full path to inbox.json file
/// * `team` - Target team name (reserved for future use)
/// * `agent` - Target agent name (reserved for future use)
/// * `update_fn` - Closure that modifies the message vector in place
///
/// # Errors
///
/// Returns `InboxError` for I/O errors, JSON parse errors, lock timeout,
/// or merge failures.
pub fn inbox_update<F>(
    inbox_path: &Path,
    _team: &str,
    _agent: &str,
    update_fn: F,
) -> Result<(), InboxError>
where
    F: FnOnce(&mut Vec<InboxMessage>),
{
    atomic_write_with_conflict_check(inbox_path, |messages| {
        update_fn(messages);
        true
    })?;
    Ok(())
}

/// Shared atomic write logic for inbox operations
///
/// Acquires lock, reads current file, applies modification via closure,
/// writes atomically with conflict detection and merge.
///
/// The `modify_fn` closure receives the current messages and returns `true`
/// if modifications were made (triggering a write), or `false` to skip
/// the write (e.g., duplicate detection).
fn atomic_write_with_conflict_check<F>(
    inbox_path: &Path,
    modify_fn: F,
) -> Result<WriteOutcome, InboxError>
where
    F: FnOnce(&mut Vec<InboxMessage>) -> bool,
{
    let lock_path = inbox_path.with_extension("lock");
    let tmp_path = inbox_path.with_extension("tmp");

    // Step 1: Acquire lock with retry
    let _lock = acquire_lock(&lock_path, 5)?;

    // Step 2: Read current inbox and compute hash
    let (mut messages, original_hash) = if inbox_path.exists() {
        let content = fs::read(inbox_path).map_err(|e| InboxError::Io {
            path: inbox_path.to_path_buf(),
            source: e,
        })?;
        let hash = compute_hash(&content);
        let msgs: Vec<InboxMessage> =
            serde_json::from_slice(&content).map_err(|e| InboxError::Json {
                path: inbox_path.to_path_buf(),
                source: e,
            })?;
        (msgs, hash)
    } else {
        // New inbox file
        (Vec::new(), compute_hash(b"[]"))
    };

    // Step 3: Apply modification
    if !modify_fn(&mut messages) {
        // No changes needed (e.g., duplicate message)
        return Ok(WriteOutcome::Success);
    }

    // Step 4: Write to tmp file with fsync
    let new_content =
        serde_json::to_vec_pretty(&messages).map_err(|e| InboxError::Json {
            path: tmp_path.clone(),
            source: e,
        })?;

    {
        let mut tmp_file = fs::File::create(&tmp_path).map_err(|e| InboxError::Io {
            path: tmp_path.clone(),
            source: e,
        })?;

        tmp_file
            .write_all(&new_content)
            .map_err(|e| InboxError::Io {
                path: tmp_path.clone(),
                source: e,
            })?;

        tmp_file.sync_all().map_err(|e| InboxError::Io {
            path: tmp_path.clone(),
            source: e,
        })?;
    }

    // Step 5: Atomic swap
    if !inbox_path.exists() {
        // First time creating inbox - just rename
        fs::rename(&tmp_path, inbox_path).map_err(|e| InboxError::Io {
            path: inbox_path.to_path_buf(),
            source: e,
        })?;
        return Ok(WriteOutcome::Success);
    }

    atomic_swap(inbox_path, &tmp_path)?;

    // Step 6: Check for concurrent writes
    let displaced_content = fs::read(&tmp_path).map_err(|e| InboxError::Io {
        path: tmp_path.clone(),
        source: e,
    })?;
    let displaced_hash = compute_hash(&displaced_content);

    let outcome = if displaced_hash != original_hash {
        // Step 7: Conflict detected - merge and re-swap
        let displaced_messages: Vec<InboxMessage> =
            serde_json::from_slice(&displaced_content).map_err(|e| InboxError::Json {
                path: tmp_path.clone(),
                source: e,
            })?;

        // Merge: add messages from displaced that aren't in our version
        let merged = merge_messages(&messages, &displaced_messages);
        let merge_count = merged.len() - messages.len();

        // Write merged version back
        let merged_content =
            serde_json::to_vec_pretty(&merged).map_err(|e| InboxError::Json {
                path: tmp_path.clone(),
                source: e,
            })?;

        fs::write(&tmp_path, &merged_content).map_err(|e| InboxError::Io {
            path: tmp_path.clone(),
            source: e,
        })?;

        // Re-swap
        atomic_swap(inbox_path, &tmp_path)?;

        WriteOutcome::ConflictResolved {
            merged_messages: merge_count,
        }
    } else {
        WriteOutcome::Success
    };

    // Step 8: Lock released automatically on drop
    // Step 9: Delete tmp file
    let _ = fs::remove_file(&tmp_path); // Ignore errors on cleanup

    Ok(outcome)
}

/// Merge two message arrays, preserving order and deduplicating by message_id
fn merge_messages(
    our_messages: &[InboxMessage],
    their_messages: &[InboxMessage],
) -> Vec<InboxMessage> {
    let mut merged = our_messages.to_vec();
    let our_ids: std::collections::HashSet<_> = our_messages
        .iter()
        .filter_map(|m| m.message_id.as_ref())
        .collect();

    // Add messages from their version that we don't have
    for msg in their_messages {
        let already_present = if let Some(ref msg_id) = msg.message_id {
            our_ids.contains(msg_id)
        } else {
            // No message_id - check by content (less reliable)
            our_messages
                .iter()
                .any(|m| m.from == msg.from && m.text == msg.text && m.timestamp == msg.timestamp)
        };

        if !already_present {
            merged.push(msg.clone());
        }
    }

    // Sort by timestamp to maintain chronological order
    merged.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    merged
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn create_test_message(from: &str, text: &str, message_id: Option<String>) -> InboxMessage {
        InboxMessage {
            from: from.to_string(),
            text: text.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: None,
            message_id,
            unknown_fields: HashMap::new(),
        }
    }

    #[test]
    fn test_inbox_append_new_file() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_path = temp_dir.path().join("agent.json");

        let message = create_test_message("team-lead", "Test message", Some("msg-001".to_string()));

        let outcome = inbox_append(&inbox_path, &message, "test-team", "test-agent").unwrap();
        assert_eq!(outcome, WriteOutcome::Success);

        // Verify file was created and contains message
        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<InboxMessage> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].from, "team-lead");
        assert_eq!(messages[0].text, "Test message");
    }

    #[test]
    fn test_inbox_append_existing_file() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_path = temp_dir.path().join("agent.json");

        // Create initial message
        let msg1 = create_test_message("team-lead", "Message 1", Some("msg-001".to_string()));
        inbox_append(&inbox_path, &msg1, "test-team", "test-agent").unwrap();

        // Append second message
        let msg2 = create_test_message("ci-agent", "Message 2", Some("msg-002".to_string()));
        let outcome = inbox_append(&inbox_path, &msg2, "test-team", "test-agent").unwrap();
        assert_eq!(outcome, WriteOutcome::Success);

        // Verify both messages present
        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<InboxMessage> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text, "Message 1");
        assert_eq!(messages[1].text, "Message 2");
    }

    #[test]
    fn test_inbox_append_deduplication() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_path = temp_dir.path().join("agent.json");

        let message = create_test_message("team-lead", "Test message", Some("msg-001".to_string()));

        // First append
        inbox_append(&inbox_path, &message, "test-team", "test-agent").unwrap();

        // Second append with same message_id - should be deduplicated
        let outcome = inbox_append(&inbox_path, &message, "test-team", "test-agent").unwrap();
        assert_eq!(outcome, WriteOutcome::Success);

        // Verify only one message present
        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<InboxMessage> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn test_merge_messages_no_duplicates() {
        let msg1 = create_test_message("team-lead", "Message 1", Some("msg-001".to_string()));
        let msg2 = create_test_message("ci-agent", "Message 2", Some("msg-002".to_string()));
        let msg3 = create_test_message("qa-agent", "Message 3", Some("msg-003".to_string()));

        let our_messages = vec![msg1.clone(), msg2.clone()];
        let their_messages = vec![msg1.clone(), msg3.clone()];

        let merged = merge_messages(&our_messages, &their_messages);

        assert_eq!(merged.len(), 3);
        assert!(merged.iter().any(|m| m.message_id == Some("msg-001".to_string())));
        assert!(merged.iter().any(|m| m.message_id == Some("msg-002".to_string())));
        assert!(merged.iter().any(|m| m.message_id == Some("msg-003".to_string())));
    }

    #[test]
    fn test_merge_messages_preserves_order() {
        let mut msg1 = create_test_message("team-lead", "Message 1", Some("msg-001".to_string()));
        msg1.timestamp = "2026-02-11T10:00:00Z".to_string();

        let mut msg2 = create_test_message("ci-agent", "Message 2", Some("msg-002".to_string()));
        msg2.timestamp = "2026-02-11T11:00:00Z".to_string();

        let mut msg3 = create_test_message("qa-agent", "Message 3", Some("msg-003".to_string()));
        msg3.timestamp = "2026-02-11T10:30:00Z".to_string();

        let our_messages = vec![msg1.clone(), msg2.clone()];
        let their_messages = vec![msg3.clone()];

        let merged = merge_messages(&our_messages, &their_messages);

        // Should be sorted by timestamp: msg1, msg3, msg2
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].timestamp, "2026-02-11T10:00:00Z");
        assert_eq!(merged[1].timestamp, "2026-02-11T10:30:00Z");
        assert_eq!(merged[2].timestamp, "2026-02-11T11:00:00Z");
    }

    #[test]
    fn test_merge_messages_without_message_id() {
        let mut msg1 = create_test_message("team-lead", "Unique message", None);
        msg1.timestamp = "2026-02-11T10:00:00Z".to_string();

        let mut msg2 = create_test_message("team-lead", "Unique message", None);
        msg2.timestamp = "2026-02-11T10:00:00Z".to_string(); // Exact same timestamp

        let our_messages = vec![msg1.clone()];
        let their_messages = vec![msg2.clone()];

        let merged = merge_messages(&our_messages, &their_messages);

        // Should deduplicate by content (from, text, timestamp match)
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn test_inbox_append_preserves_unknown_fields() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_path = temp_dir.path().join("agent.json");

        // Create inbox with unknown fields
        let json = r#"[{
            "from": "team-lead",
            "text": "Existing message",
            "timestamp": "2026-02-11T10:00:00Z",
            "read": false,
            "unknownField": "should be preserved",
            "futureFeature": {"nested": "data"}
        }]"#;
        fs::write(&inbox_path, json).unwrap();

        // Append new message
        let new_message = create_test_message("ci-agent", "New message", Some("msg-002".to_string()));
        inbox_append(&inbox_path, &new_message, "test-team", "test-agent").unwrap();

        // Verify unknown fields preserved
        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<InboxMessage> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages[0].unknown_fields.contains_key("unknownField"));
        assert!(messages[0].unknown_fields.contains_key("futureFeature"));
    }

    #[test]
    fn test_inbox_update_marks_read() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_path = temp_dir.path().join("agent.json");

        // Seed inbox with unread messages
        let msg1 = create_test_message("user-a", "Message 1", Some("msg-001".to_string()));
        let msg2 = create_test_message("user-b", "Message 2", Some("msg-002".to_string()));
        inbox_append(&inbox_path, &msg1, "test-team", "test-agent").unwrap();
        inbox_append(&inbox_path, &msg2, "test-team", "test-agent").unwrap();

        // Mark all as read via inbox_update
        inbox_update(&inbox_path, "test-team", "test-agent", |messages| {
            for msg in messages.iter_mut() {
                msg.read = true;
            }
        })
        .unwrap();

        // Verify all marked as read
        let content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<InboxMessage> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages[0].read);
        assert!(messages[1].read);
    }

    #[test]
    fn test_inbox_update_concurrent_writes() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let temp_dir = TempDir::new().unwrap();
        let inbox_path = temp_dir.path().join("agent.json");

        // Seed inbox with messages to update
        let msg1 = create_test_message("user-a", "Message 1", Some("msg-001".to_string()));
        let msg2 = create_test_message("user-b", "Message 2", Some("msg-002".to_string()));
        inbox_append(&inbox_path, &msg1, "test-team", "test-agent").unwrap();
        inbox_append(&inbox_path, &msg2, "test-team", "test-agent").unwrap();

        let inbox_path = Arc::new(inbox_path);
        let barrier = Arc::new(Barrier::new(2));

        // Thread 1: Mark messages as read via inbox_update
        let path1 = Arc::clone(&inbox_path);
        let barrier1 = Arc::clone(&barrier);
        let handle1 = thread::spawn(move || {
            barrier1.wait();
            inbox_update(&path1, "test-team", "test-agent", |messages| {
                for msg in messages.iter_mut() {
                    msg.read = true;
                }
            })
            .unwrap();
        });

        // Thread 2: Append a new message via inbox_append
        let path2 = Arc::clone(&inbox_path);
        let barrier2 = Arc::clone(&barrier);
        let handle2 = thread::spawn(move || {
            barrier2.wait();
            let msg3 = create_test_message("user-c", "Message 3", Some("msg-003".to_string()));
            inbox_append(&path2, &msg3, "test-team", "test-agent").unwrap();
        });

        handle1.join().unwrap();
        handle2.join().unwrap();

        // Verify: all messages present (no data loss)
        let content = fs::read_to_string(&*inbox_path).unwrap();
        let messages: Vec<InboxMessage> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages.len(), 3, "No messages should be lost");
        assert!(
            messages.iter().any(|m| m.message_id == Some("msg-001".to_string())),
            "msg-001 should be present"
        );
        assert!(
            messages.iter().any(|m| m.message_id == Some("msg-002".to_string())),
            "msg-002 should be present"
        );
        assert!(
            messages.iter().any(|m| m.message_id == Some("msg-003".to_string())),
            "msg-003 should be present"
        );
    }
}
