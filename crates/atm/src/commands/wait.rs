//! File watching for blocking reads

use anyhow::Result;
use notify::{Config as NotifyConfig, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Result of waiting for a message
#[derive(Debug, PartialEq, Eq)]
pub enum WaitResult {
    /// New message arrived
    MessageReceived,
    /// Timeout expired
    Timeout,
}

/// Wait for a new message to arrive in the inbox
///
/// Uses OS-level file watching (inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW on Windows)
/// to detect changes to the inbox files. Falls back to polling every 2 seconds if file watching fails.
///
/// # Arguments
///
/// * `inbox_dir` - Path to the inboxes directory
/// * `agent_name` - Name of the agent to watch
/// * `timeout_secs` - Timeout in seconds
/// * `known_hostnames` - Optional list of known hostnames for bridge-synced messages
///
/// # Returns
///
/// `WaitResult::MessageReceived` if a new message arrives, `WaitResult::Timeout` if timeout expires
pub fn wait_for_message(
    inbox_dir: &Path,
    agent_name: &str,
    timeout_secs: u64,
    known_hostnames: Option<&Vec<String>>,
) -> Result<WaitResult> {
    // Build list of inbox files to watch
    let mut inbox_files = vec![format!("{agent_name}.json")];

    // Add per-origin inbox files
    if let Some(hostnames) = known_hostnames {
        for hostname in hostnames {
            inbox_files.push(format!("{agent_name}.{hostname}.json"));
        }
    }

    // Get initial message count
    let initial_count = count_messages(inbox_dir, agent_name, known_hostnames)?;

    // Try file watching first
    match try_file_watching(inbox_dir, agent_name, timeout_secs, initial_count, known_hostnames) {
        Ok(result) => Ok(result),
        Err(e) => {
            eprintln!("Warning: File watching failed ({e}), falling back to polling");
            // Fall back to polling
            polling_wait(inbox_dir, agent_name, timeout_secs, initial_count, known_hostnames)
        }
    }
}

/// Try file watching approach
fn try_file_watching(
    inbox_dir: &Path,
    agent_name: &str,
    timeout_secs: u64,
    initial_count: usize,
    known_hostnames: Option<&Vec<String>>,
) -> Result<WaitResult> {
    let (tx, rx) = channel();

    // Create watcher with debouncing
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(_event) = res {
                let _ = tx.send(());
            }
        },
        NotifyConfig::default().with_poll_interval(Duration::from_millis(500)),
    )?;

    // Watch the inboxes directory
    watcher.watch(inbox_dir, RecursiveMode::NonRecursive)?;

    let timeout = Duration::from_secs(timeout_secs);
    let start = Instant::now();

    loop {
        // Check if we've exceeded timeout
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return Ok(WaitResult::Timeout);
        }

        let remaining = timeout - elapsed;

        // Wait for file system event or timeout
        match rx.recv_timeout(remaining) {
            Ok(()) => {
                // File changed, re-count messages
                let current_count = count_messages(inbox_dir, agent_name, known_hostnames)?;
                if current_count > initial_count {
                    return Ok(WaitResult::MessageReceived);
                }
                // False alarm (e.g., temp file created), continue waiting
            }
            Err(RecvTimeoutError::Timeout) => {
                return Ok(WaitResult::Timeout);
            }
            Err(RecvTimeoutError::Disconnected) => {
                anyhow::bail!("File watcher disconnected unexpectedly");
            }
        }
    }
}

/// Polling fallback for NFS or unsupported filesystems
fn polling_wait(
    inbox_dir: &Path,
    agent_name: &str,
    timeout_secs: u64,
    initial_count: usize,
    known_hostnames: Option<&Vec<String>>,
) -> Result<WaitResult> {
    let timeout = Duration::from_secs(timeout_secs);
    let start = Instant::now();
    let poll_interval = Duration::from_secs(2);

    loop {
        // Check if we've exceeded timeout
        if start.elapsed() >= timeout {
            return Ok(WaitResult::Timeout);
        }

        // Check for new messages
        let current_count = count_messages(inbox_dir, agent_name, known_hostnames)?;
        if current_count > initial_count {
            return Ok(WaitResult::MessageReceived);
        }

        // Sleep for poll interval or remaining time, whichever is shorter
        let remaining = timeout.saturating_sub(start.elapsed());
        std::thread::sleep(std::cmp::min(poll_interval, remaining));
    }
}

/// Count total messages across all inbox files
fn count_messages(
    inbox_dir: &Path,
    agent_name: &str,
    known_hostnames: Option<&Vec<String>>,
) -> Result<usize> {
    let mut total = 0;

    // Count messages in local inbox
    let local_inbox = inbox_dir.join(format!("{agent_name}.json"));
    if local_inbox.exists() {
        if let Ok(content) = std::fs::read_to_string(&local_inbox) {
            if let Ok(messages) = serde_json::from_str::<Vec<serde_json::Value>>(&content) {
                total += messages.len();
            }
        }
    }

    // Count messages in origin inboxes
    if let Some(hostnames) = known_hostnames {
        for hostname in hostnames {
            let origin_inbox = inbox_dir.join(format!("{agent_name}.{hostname}.json"));
            if origin_inbox.exists() {
                if let Ok(content) = std::fs::read_to_string(&origin_inbox) {
                    if let Ok(messages) = serde_json::from_str::<Vec<serde_json::Value>>(&content) {
                        total += messages.len();
                    }
                }
            }
        }
    }

    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_count_messages_empty() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_dir = temp_dir.path().join("inboxes");
        fs::create_dir_all(&inbox_dir).unwrap();

        let count = count_messages(&inbox_dir, "test-agent", None).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_count_messages_single_inbox() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_dir = temp_dir.path().join("inboxes");
        fs::create_dir_all(&inbox_dir).unwrap();

        let inbox_file = inbox_dir.join("test-agent.json");
        let messages = serde_json::json!([
            {"from": "user1", "text": "msg1", "timestamp": "2026-01-01T00:00:00Z", "read": false},
            {"from": "user2", "text": "msg2", "timestamp": "2026-01-01T00:01:00Z", "read": false}
        ]);
        fs::write(&inbox_file, serde_json::to_string(&messages).unwrap()).unwrap();

        let count = count_messages(&inbox_dir, "test-agent", None).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_count_messages_with_origins() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_dir = temp_dir.path().join("inboxes");
        fs::create_dir_all(&inbox_dir).unwrap();

        // Local inbox with 2 messages
        let local_inbox = inbox_dir.join("test-agent.json");
        let local_messages = serde_json::json!([
            {"from": "user1", "text": "msg1", "timestamp": "2026-01-01T00:00:00Z", "read": false},
            {"from": "user2", "text": "msg2", "timestamp": "2026-01-01T00:01:00Z", "read": false}
        ]);
        fs::write(&local_inbox, serde_json::to_string(&local_messages).unwrap()).unwrap();

        // Origin inbox with 1 message
        let origin_inbox = inbox_dir.join("test-agent.remote.json");
        let origin_messages = serde_json::json!([
            {"from": "user3", "text": "msg3", "timestamp": "2026-01-01T00:02:00Z", "read": false}
        ]);
        fs::write(&origin_inbox, serde_json::to_string(&origin_messages).unwrap()).unwrap();

        let hostnames = vec!["remote".to_string()];
        let count = count_messages(&inbox_dir, "test-agent", Some(&hostnames)).unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_polling_wait_timeout() {
        let temp_dir = TempDir::new().unwrap();
        let inbox_dir = temp_dir.path().join("inboxes");
        fs::create_dir_all(&inbox_dir).unwrap();

        let result = polling_wait(&inbox_dir, "test-agent", 1, 0, None).unwrap();
        assert_eq!(result, WaitResult::Timeout);
    }

    #[test]
    fn test_polling_wait_message_received() {
        use std::thread;

        let temp_dir = TempDir::new().unwrap();
        let inbox_dir = temp_dir.path().join("inboxes");
        fs::create_dir_all(&inbox_dir).unwrap();

        let inbox_dir_clone = inbox_dir.clone();

        // Spawn thread to write message after 500ms
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            let messages = serde_json::json!([
                {"from": "user1", "text": "new message", "timestamp": "2026-01-01T00:00:00Z", "read": false}
            ]);
            fs::write(inbox_dir_clone.join("test-agent.json"), serde_json::to_string(&messages).unwrap()).unwrap();
        });

        let result = polling_wait(&inbox_dir, "test-agent", 5, 0, None).unwrap();
        assert_eq!(result, WaitResult::MessageReceived);
    }
}
