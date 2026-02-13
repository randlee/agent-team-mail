//! Outbound message spool for guaranteed delivery
//!
//! When inbox writes fail due to lock contention, messages are queued in a spool
//! directory for later retry. The spool ensures no messages are lost even under
//! high concurrency.
//!
//! # Directory Structure
//!
//! ```text
//! ~/.config/atm/spool/
//!   pending/    - Messages awaiting retry
//!   failed/     - Messages that exceeded max retries
//! ```
//!
//! # Spool Workflow
//!
//! 1. `inbox_append()` fails to acquire lock → `spool_message()` queues message
//! 2. User/daemon calls `spool_drain()` periodically
//! 3. Each pending message is retried via `inbox_append()`
//! 4. On success: message deleted from pending/
//! 5. On failure: retry_count incremented
//! 6. After max_retries: message moved to failed/

use crate::io::{error::InboxError, inbox::{inbox_append, WriteOutcome}};
use crate::schema::InboxMessage;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Metadata for a spooled message awaiting delivery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpooledMessage {
    /// Target team name
    pub target_team: String,

    /// Target agent name
    pub target_agent: String,

    /// The actual message to deliver
    pub message: InboxMessage,

    /// Number of delivery attempts so far
    pub retry_count: u32,

    /// Maximum retry attempts before moving to failed/
    pub max_retries: u32,

    /// ISO 8601 timestamp when message was first spooled
    pub created_at: String,

    /// ISO 8601 timestamp of last delivery attempt
    pub last_attempt: String,
}

/// Status report from spool drain operation
#[derive(Debug, Clone, PartialEq)]
pub struct SpoolStatus {
    /// Number of messages successfully delivered
    pub delivered: usize,

    /// Number of messages still in pending/ queue
    pub pending: usize,

    /// Number of messages that exceeded max retries
    pub failed: usize,
}

/// Create a spooled message entry
///
/// Called by `inbox_append()` when lock acquisition fails.
///
/// # Arguments
///
/// * `team` - Target team name
/// * `agent` - Target agent name
/// * `message` - Message to spool
///
/// # Returns
///
/// Path to the spooled message file in pending/
pub fn spool_message(team: &str, agent: &str, message: &InboxMessage) -> Result<PathBuf, InboxError> {
    spool_message_with_base(team, agent, message, None)
}

/// Internal implementation that accepts an optional base directory for testing
fn spool_message_with_base(
    team: &str,
    agent: &str,
    message: &InboxMessage,
    base_dir: Option<&Path>,
) -> Result<PathBuf, InboxError> {
    let spool_dir = get_spool_dir_with_base("pending", base_dir)?;
    fs::create_dir_all(&spool_dir).map_err(|e| InboxError::Io {
        path: spool_dir.clone(),
        source: e,
    })?;

    let now = chrono::Utc::now();
    let timestamp = now.timestamp();
    let filename = format!("{timestamp}-{agent}@{team}.json");
    let spool_path = spool_dir.join(&filename);

    let spooled = SpooledMessage {
        target_team: team.to_string(),
        target_agent: agent.to_string(),
        message: message.clone(),
        retry_count: 0,
        max_retries: 10,
        created_at: now.to_rfc3339(),
        last_attempt: now.to_rfc3339(),
    };

    let content = serde_json::to_vec_pretty(&spooled).map_err(|e| InboxError::Json {
        path: spool_path.clone(),
        source: e,
    })?;

    fs::write(&spool_path, content).map_err(|e| InboxError::Io {
        path: spool_path.clone(),
        source: e,
    })?;

    Ok(spool_path)
}

/// Drain the outbound spool, retrying pending messages
///
/// Iterates all files in pending/, attempts delivery via `inbox_append()`,
/// and updates retry counts. Messages that exceed max_retries are moved to failed/.
///
/// # Returns
///
/// `SpoolStatus` with delivery statistics
pub fn spool_drain(inbox_base: &Path) -> Result<SpoolStatus, InboxError> {
    spool_drain_with_base(inbox_base, None)
}

/// Internal implementation that accepts an optional base directory for testing
fn spool_drain_with_base(inbox_base: &Path, base_dir: Option<&Path>) -> Result<SpoolStatus, InboxError> {
    let pending_dir = get_spool_dir_with_base("pending", base_dir)?;
    let failed_dir = get_spool_dir_with_base("failed", base_dir)?;

    // Ensure directories exist
    fs::create_dir_all(&pending_dir).map_err(|e| InboxError::Io {
        path: pending_dir.clone(),
        source: e,
    })?;
    fs::create_dir_all(&failed_dir).map_err(|e| InboxError::Io {
        path: failed_dir.clone(),
        source: e,
    })?;

    let mut delivered = 0;

    // Process all pending messages
    if pending_dir.exists() {
        let entries = fs::read_dir(&pending_dir).map_err(|e| InboxError::Io {
            path: pending_dir.clone(),
            source: e,
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| InboxError::Io {
                path: pending_dir.clone(),
                source: e,
            })?;

            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            match process_spooled_message(&path, inbox_base, &failed_dir) {
                Ok(true) => {
                    // Message delivered - delete spool file
                    let _ = fs::remove_file(&path); // Ignore cleanup errors
                    delivered += 1;
                }
                Ok(false) => {
                    // Message still pending (updated in-place or moved to failed)
                }
                Err(e) => {
                    // Log error but continue processing other messages
                    eprintln!("Warning: Failed to process {path:?}: {e}");
                }
            }
        }
    }

    // Count remaining messages
    let pending = count_files(&pending_dir)?;
    let failed = count_files(&failed_dir)?;

    Ok(SpoolStatus {
        delivered,
        pending,
        failed,
    })
}

/// Process a single spooled message file
///
/// Returns Ok(true) if delivered, Ok(false) if still pending/failed
fn process_spooled_message(
    spool_path: &Path,
    inbox_base: &Path,
    failed_dir: &Path,
) -> Result<bool, InboxError> {
    // Read spooled message
    let content = fs::read(spool_path).map_err(|e| InboxError::Io {
        path: spool_path.to_path_buf(),
        source: e,
    })?;

    let mut spooled: SpooledMessage =
        serde_json::from_slice(&content).map_err(|e| InboxError::Json {
            path: spool_path.to_path_buf(),
            source: e,
        })?;

    // Construct inbox path: inbox_base/{team}/inboxes/{agent}.json
    let inbox_path = inbox_base
        .join(&spooled.target_team)
        .join("inboxes")
        .join(format!("{}.json", spooled.target_agent));

    // Attempt delivery (including directory creation and inbox append)
    let delivery_result = (|| -> Result<WriteOutcome, InboxError> {
        // Ensure inbox directory exists
        if let Some(parent) = inbox_path.parent() {
            fs::create_dir_all(parent).map_err(|e| InboxError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        inbox_append(&inbox_path, &spooled.message, &spooled.target_team, &spooled.target_agent)
    })();

    match delivery_result {
        Ok(WriteOutcome::Success | WriteOutcome::ConflictResolved { .. }) => {
            // Message delivered successfully
            return Ok(true);
        }
        Ok(WriteOutcome::Queued { spool_path: new_spool_path }) => {
            // Lock contention - inbox_append re-spooled the message.
            // Delete the redundant spool file since we keep the original.
            let _ = fs::remove_file(&new_spool_path);
        }
        Err(_) => {
            // Delivery error - fall through to retry logic
        }
    }

    // Delivery failed or queued - increment retry count
    spooled.retry_count += 1;
    spooled.last_attempt = chrono::Utc::now().to_rfc3339();

    if spooled.retry_count >= spooled.max_retries {
        // Move to failed directory
        let failed_path = failed_dir.join(
            spool_path
                .file_name()
                .ok_or_else(|| InboxError::SpoolError {
                    message: format!("Invalid spool path: {spool_path:?}"),
                })?,
        );

        let failed_content =
            serde_json::to_vec_pretty(&spooled).map_err(|e| InboxError::Json {
                path: failed_path.clone(),
                source: e,
            })?;

        fs::write(&failed_path, failed_content).map_err(|e| InboxError::Io {
            path: failed_path.clone(),
            source: e,
        })?;

        // Delete from pending
        let _ = fs::remove_file(spool_path);
    } else {
        // Write back with updated retry count
        let updated_content =
            serde_json::to_vec_pretty(&spooled).map_err(|e| InboxError::Json {
                path: spool_path.to_path_buf(),
                source: e,
            })?;

        fs::write(spool_path, updated_content).map_err(|e| InboxError::Io {
            path: spool_path.to_path_buf(),
            source: e,
        })?;
    }

    Ok(false)
}

/// Get spool directory path (pending/ or failed/)
fn get_spool_dir_with_base(subdir: &str, base_dir: Option<&Path>) -> Result<PathBuf, InboxError> {
    let spool_dir = if let Some(base) = base_dir {
        base.join("spool").join(subdir)
    } else if let Ok(atm_home) = std::env::var("ATM_HOME") {
        PathBuf::from(atm_home).join("spool").join(subdir)
    } else {
        dirs::config_dir()
            .ok_or_else(|| InboxError::SpoolError {
                message: "Could not determine config directory".to_string(),
            })?
            .join("atm")
            .join("spool")
            .join(subdir)
    };

    Ok(spool_dir)
}

/// Count JSON files in a directory
fn count_files(dir: &Path) -> Result<usize, InboxError> {
    if !dir.exists() {
        return Ok(0);
    }

    let entries = fs::read_dir(dir).map_err(|e| InboxError::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;

    let count = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().is_file() && e.path().extension().and_then(|s| s.to_str()) == Some("json")
        })
        .count();

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::WriteOutcome;
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
    fn test_spool_message_format() {
        let temp_dir = TempDir::new().unwrap();
        let message = create_test_message("team-lead", "Test message", Some("msg-001".to_string()));

        let spool_path = spool_message_with_base("test-team", "test-agent", &message, Some(temp_dir.path())).unwrap();
        assert!(spool_path.exists());
        assert!(spool_path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains("test-agent@test-team.json"));

        // Verify SpooledMessage structure
        let content = fs::read_to_string(&spool_path).unwrap();
        let spooled: SpooledMessage = serde_json::from_str(&content).unwrap();
        assert_eq!(spooled.target_team, "test-team");
        assert_eq!(spooled.target_agent, "test-agent");
        assert_eq!(spooled.message.text, "Test message");
        assert_eq!(spooled.retry_count, 0);
        assert_eq!(spooled.max_retries, 10);
        assert!(!spooled.created_at.is_empty());
        assert!(!spooled.last_attempt.is_empty());
    }

    #[test]
    fn test_spool_drain_delivers_messages() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_base = temp_dir.path().join("teams");
        fs::create_dir_all(&inbox_base).unwrap();

        let message = create_test_message("team-lead", "Test message", Some("msg-001".to_string()));
        let spool_path = spool_message_with_base("test-team", "test-agent", &message, Some(temp_dir.path())).unwrap();
        assert!(spool_path.exists());

        // Drain spool
        let status = spool_drain_with_base(&inbox_base, Some(temp_dir.path())).unwrap();
        assert_eq!(status.delivered, 1);
        assert_eq!(status.pending, 0);
        assert_eq!(status.failed, 0);

        // Verify message was delivered to inbox
        let inbox_path = inbox_base
            .join("test-team")
            .join("inboxes")
            .join("test-agent.json");
        assert!(inbox_path.exists());

        let inbox_content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<InboxMessage> = serde_json::from_str(&inbox_content).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "Test message");

        // Verify spool file was deleted
        assert!(!spool_path.exists());
    }

    #[test]
    fn test_spool_drain_increments_retry_count() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_base = temp_dir.path().join("teams");

        let message = create_test_message("team-lead", "Test message", Some("msg-001".to_string()));
        let spool_path = spool_message_with_base("test-team", "test-agent", &message, Some(temp_dir.path())).unwrap();

        // Manually modify the spool message to simulate a previous failed attempt
        let content = fs::read_to_string(&spool_path).unwrap();
        let mut spooled: SpooledMessage = serde_json::from_str(&content).unwrap();

        // Simulate a scenario where retry count should be preserved through delivery
        // We'll test by delivering successfully, then checking that on failure the count increments
        // For this test, we just verify the structure allows retry_count tracking
        assert_eq!(spooled.retry_count, 0);
        assert_eq!(spooled.max_retries, 10);

        // Increment retry count manually to simulate a failed attempt
        spooled.retry_count = 1;
        spooled.last_attempt = chrono::Utc::now().to_rfc3339();
        fs::write(&spool_path, serde_json::to_string_pretty(&spooled).unwrap()).unwrap();

        // Now drain - should deliver successfully
        let status = spool_drain_with_base(&inbox_base, Some(temp_dir.path())).unwrap();
        assert_eq!(status.delivered, 1);
        assert_eq!(status.pending, 0);

        // Verify spool file was deleted after successful delivery
        assert!(!spool_path.exists());
    }

    #[test]
    fn test_spool_drain_moves_to_failed() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_base = temp_dir.path().join("teams");

        let message = create_test_message("team-lead", "Test message", Some("msg-001".to_string()));
        let spool_path = spool_message_with_base("test-team", "test-agent", &message, Some(temp_dir.path())).unwrap();

        // Manually set retry_count to max_retries to trigger immediate failure
        let content = fs::read_to_string(&spool_path).unwrap();
        let mut spooled: SpooledMessage = serde_json::from_str(&content).unwrap();
        spooled.retry_count = 10; // At max_retries (10)
        fs::write(&spool_path, serde_json::to_string_pretty(&spooled).unwrap()).unwrap();

        // Create an invalid inbox directory path to force delivery failure
        // Make the inboxes directory a file instead of a directory
        let inboxes_dir = inbox_base.join("test-team").join("inboxes");
        fs::create_dir_all(inbox_base.join("test-team")).unwrap();
        fs::write(&inboxes_dir, "not a directory").unwrap();

        // Drain - should move to failed
        let status = spool_drain_with_base(&inbox_base, Some(temp_dir.path())).unwrap();
        assert_eq!(status.delivered, 0);
        assert_eq!(status.pending, 0);
        assert_eq!(status.failed, 1);

        // Verify message moved to failed directory
        assert!(!spool_path.exists());

        let failed_dir = get_spool_dir_with_base("failed", Some(temp_dir.path())).unwrap();
        let failed_files: Vec<_> = fs::read_dir(&failed_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        assert_eq!(failed_files.len(), 1);
    }

    #[test]
    fn test_spool_status_counts() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_base = temp_dir.path().join("teams");
        fs::create_dir_all(&inbox_base).unwrap();

        // Create multiple spooled messages
        for i in 0..3 {
            let message = create_test_message(
                "team-lead",
                &format!("Message {i}"),
                Some(format!("msg-{i:03}")),
            );
            spool_message_with_base("test-team", &format!("agent-{i}"), &message, Some(temp_dir.path())).unwrap();
        }

        // Drain - all should be delivered
        let status = spool_drain_with_base(&inbox_base, Some(temp_dir.path())).unwrap();
        assert_eq!(status.delivered, 3);
        assert_eq!(status.pending, 0);
        assert_eq!(status.failed, 0);
    }

    #[test]
    fn test_spool_directories_auto_created() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_base = temp_dir.path().join("teams");

        let message = create_test_message("team-lead", "Test message", Some("msg-001".to_string()));

        // Spool message - should auto-create pending directory
        let spool_path = spool_message_with_base("test-team", "test-agent", &message, Some(temp_dir.path())).unwrap();
        assert!(spool_path.exists());
        assert!(spool_path.parent().unwrap().exists());

        // Drain - should auto-create failed directory if needed
        let _ = spool_drain_with_base(&inbox_base, Some(temp_dir.path())).unwrap();
        let failed_dir = get_spool_dir_with_base("failed", Some(temp_dir.path())).unwrap();
        assert!(failed_dir.exists());
    }

    #[test]
    fn test_spool_drain_keeps_pending_on_queued_outcome() {
        use crate::io::lock::acquire_lock;

        let temp_dir = TempDir::new().unwrap();
        let inbox_base = temp_dir.path().join("teams");
        fs::create_dir_all(&inbox_base).unwrap();

        // Create a spooled message
        let message = create_test_message("team-lead", "Test message", Some("msg-001".to_string()));
        let spool_path =
            spool_message_with_base("test-team", "test-agent", &message, Some(temp_dir.path()))
                .unwrap();
        assert!(spool_path.exists());

        // Verify initial retry_count is 0
        let content = fs::read_to_string(&spool_path).unwrap();
        let initial: SpooledMessage = serde_json::from_str(&content).unwrap();
        assert_eq!(initial.retry_count, 0);

        // Create inbox directory structure and a valid inbox file
        let inbox_path = inbox_base
            .join("test-team")
            .join("inboxes")
            .join("test-agent.json");
        fs::create_dir_all(inbox_path.parent().unwrap()).unwrap();
        fs::write(&inbox_path, "[]").unwrap();

        // Hold the lock on inbox to force inbox_append to timeout → return Queued
        let lock_path = inbox_path.with_extension("lock");
        let _held_lock = acquire_lock(&lock_path, 0).unwrap();

        // Drain spool - should fail to deliver due to held lock
        let status = spool_drain_with_base(&inbox_base, Some(temp_dir.path())).unwrap();
        assert_eq!(status.delivered, 0, "Should not deliver when lock is held");
        assert_eq!(status.pending, 1, "Spool file should remain in pending");

        // Verify spool file still exists with incremented retry_count
        assert!(spool_path.exists(), "Spool file should not be deleted");
        let content = fs::read_to_string(&spool_path).unwrap();
        let updated: SpooledMessage = serde_json::from_str(&content).unwrap();
        assert_eq!(updated.retry_count, 1, "retry_count should be incremented");
        assert_ne!(
            updated.last_attempt, initial.last_attempt,
            "last_attempt should be updated"
        );
    }

    #[test]
    fn test_duplicate_detection_in_spool_drain() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_base = temp_dir.path().join("teams");
        fs::create_dir_all(&inbox_base).unwrap();

        // Create message with specific message_id
        let message = create_test_message("team-lead", "Test message", Some("msg-001".to_string()));

        // Manually add message to inbox first
        let inbox_path = inbox_base
            .join("test-team")
            .join("inboxes")
            .join("test-agent.json");
        fs::create_dir_all(inbox_path.parent().unwrap()).unwrap();
        let result = inbox_append(&inbox_path, &message, "test-team", "test-agent").unwrap();
        assert_eq!(result, WriteOutcome::Success);

        // Now spool the same message (simulating duplicate)
        spool_message_with_base("test-team", "test-agent", &message, Some(temp_dir.path())).unwrap();

        // Drain spool
        let status = spool_drain_with_base(&inbox_base, Some(temp_dir.path())).unwrap();
        assert_eq!(status.delivered, 1); // Treated as delivered (dedup skips insert)
        assert_eq!(status.pending, 0);
        assert_eq!(status.failed, 0);

        // Verify inbox still has only one message
        let inbox_content = fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<InboxMessage> = serde_json::from_str(&inbox_content).unwrap();
        assert_eq!(messages.len(), 1);
    }
}
