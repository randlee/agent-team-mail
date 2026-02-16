//! Deduplication and sync state tracking for bridge plugin
//!
//! Tracks which messages have been synced to avoid re-transferring them.
//! Persists state to disk to survive daemon restarts.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use tokio::fs;
use uuid::Uuid;

use atm_core::schema::InboxMessage;

/// Maximum number of message_ids to keep in dedup cache
const MAX_SYNCED_MESSAGE_IDS: usize = 10_000;

/// Bounded FIFO cache for synced message IDs
///
/// Maintains a bounded set of recently synced message IDs with FIFO eviction.
/// Note: This is NOT a true LRU cache - it does not update recency on re-insertion.
/// Messages are evicted in insertion order (oldest first).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FifoMessageIdCache {
    /// Set of synced message IDs for fast lookup
    #[serde(default)]
    ids: HashSet<String>,

    /// Queue of message IDs in insertion order (oldest first)
    #[serde(default)]
    queue: VecDeque<String>,

    /// Maximum cache size
    #[serde(default = "default_max_size")]
    max_size: usize,
}

fn default_max_size() -> usize {
    MAX_SYNCED_MESSAGE_IDS
}

impl FifoMessageIdCache {
    /// Create a new FIFO cache with default size
    pub fn new() -> Self {
        Self {
            ids: HashSet::new(),
            queue: VecDeque::new(),
            max_size: MAX_SYNCED_MESSAGE_IDS,
        }
    }

    /// Check if a message_id has been synced
    pub fn contains(&self, message_id: &str) -> bool {
        self.ids.contains(message_id)
    }

    /// Insert a message_id, evicting oldest if necessary
    pub fn insert(&mut self, message_id: String) {
        // If already present, don't duplicate in queue
        if self.ids.contains(&message_id) {
            return;
        }

        // Add to set and queue
        self.ids.insert(message_id.clone());
        self.queue.push_back(message_id);

        // Evict oldest if over capacity
        while self.queue.len() > self.max_size {
            if let Some(oldest) = self.queue.pop_front() {
                self.ids.remove(&oldest);
            }
        }
    }

    /// Get current cache size
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

impl Default for FifoMessageIdCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Sync state for bridge plugin
///
/// Tracks cursors (last synced index) and synced message_ids to prevent
/// re-transferring already-synced messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncState {
    /// Per-file cursors: maps file path to index of last synced message
    ///
    /// Key is relative path (e.g., "inboxes/agent-1.json")
    /// Value is the index of the last message we synced from that file
    #[serde(default)]
    pub per_file_cursors: HashMap<PathBuf, usize>,

    /// Bounded FIFO cache of synced message_ids
    ///
    /// Used for deduplication across multiple sync cycles.
    /// Bounded to MAX_SYNCED_MESSAGE_IDS entries with FIFO eviction (oldest first).
    #[serde(default)]
    pub synced_message_ids: FifoMessageIdCache,
}

impl SyncState {
    /// Create a new empty sync state
    pub fn new() -> Self {
        Self {
            per_file_cursors: HashMap::new(),
            synced_message_ids: FifoMessageIdCache::new(),
        }
    }

    /// Load sync state from disk
    ///
    /// Returns a new empty state if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns error if the file exists but cannot be read or parsed
    pub async fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }

        let content = fs::read(path)
            .await
            .context("Failed to read sync state file")?;

        let state: SyncState = serde_json::from_slice(&content)
            .context("Failed to parse sync state file")?;

        Ok(state)
    }

    /// Save sync state to disk
    ///
    /// Uses atomic write pattern: write to temp file, then rename.
    ///
    /// # Errors
    ///
    /// Returns error if write or rename fails
    pub async fn save(&self, path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .context("Failed to create state directory")?;
        }

        // Serialize state
        let content = serde_json::to_vec_pretty(self)
            .context("Failed to serialize sync state")?;

        // Write to temp file
        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, &content)
            .await
            .context("Failed to write sync state temp file")?;

        // Atomic rename
        fs::rename(&temp_path, path)
            .await
            .context("Failed to rename sync state file")?;

        Ok(())
    }

    /// Get the cursor (last synced index) for a file
    ///
    /// Returns 0 if no cursor exists for this file (never synced before).
    pub fn get_cursor(&self, file_path: &Path) -> usize {
        self.per_file_cursors.get(file_path).copied().unwrap_or(0)
    }

    /// Update the cursor for a file
    pub fn set_cursor(&mut self, file_path: PathBuf, index: usize) {
        self.per_file_cursors.insert(file_path, index);
    }

    /// Check if a message_id has already been synced
    pub fn is_synced(&self, message_id: &str) -> bool {
        self.synced_message_ids.contains(message_id)
    }

    /// Mark a message_id as synced
    pub fn mark_synced(&mut self, message_id: String) {
        self.synced_message_ids.insert(message_id);
    }

    /// Get the number of synced message IDs in cache
    pub fn synced_count(&self) -> usize {
        self.synced_message_ids.len()
    }
}

impl Default for SyncState {
    fn default() -> Self {
        Self::new()
    }
}

/// Assign message_id to messages that don't have one
///
/// This ensures all messages have a unique identifier for deduplication.
/// Messages that already have a message_id are not modified.
pub fn assign_message_ids(messages: &mut [InboxMessage]) {
    for msg in messages {
        if msg.message_id.is_none() {
            msg.message_id = Some(Uuid::new_v4().to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[test]
    fn test_sync_state_new() {
        let state = SyncState::new();
        assert!(state.per_file_cursors.is_empty());
        assert_eq!(state.synced_count(), 0);
    }

    #[test]
    fn test_sync_state_get_cursor_default() {
        let state = SyncState::new();
        let cursor = state.get_cursor(Path::new("inboxes/agent-1.json"));
        assert_eq!(cursor, 0);
    }

    #[test]
    fn test_sync_state_set_cursor() {
        let mut state = SyncState::new();
        let path = PathBuf::from("inboxes/agent-1.json");

        state.set_cursor(path.clone(), 5);
        assert_eq!(state.get_cursor(&path), 5);

        state.set_cursor(path.clone(), 10);
        assert_eq!(state.get_cursor(&path), 10);
    }

    #[test]
    fn test_sync_state_is_synced() {
        let mut state = SyncState::new();

        assert!(!state.is_synced("msg-001"));

        state.mark_synced("msg-001".to_string());
        assert!(state.is_synced("msg-001"));
        assert!(!state.is_synced("msg-002"));
    }

    #[tokio::test]
    async fn test_sync_state_save_load_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let state_path = temp_dir.path().join(".bridge-state.json");

        // Create state with some data
        let mut state = SyncState::new();
        state.set_cursor(PathBuf::from("inboxes/agent-1.json"), 5);
        state.set_cursor(PathBuf::from("inboxes/agent-2.json"), 10);
        state.mark_synced("msg-001".to_string());
        state.mark_synced("msg-002".to_string());

        // Save
        state.save(&state_path).await.unwrap();
        assert!(state_path.exists());

        // Load
        let loaded = SyncState::load(&state_path).await.unwrap();
        assert_eq!(loaded.get_cursor(Path::new("inboxes/agent-1.json")), 5);
        assert_eq!(loaded.get_cursor(Path::new("inboxes/agent-2.json")), 10);
        assert!(loaded.is_synced("msg-001"));
        assert!(loaded.is_synced("msg-002"));
        assert!(!loaded.is_synced("msg-003"));
    }

    #[tokio::test]
    async fn test_sync_state_load_nonexistent_file() {
        let temp_dir = TempDir::new().unwrap();
        let state_path = temp_dir.path().join("nonexistent.json");

        // Load should succeed and return empty state
        let state = SyncState::load(&state_path).await.unwrap();
        assert!(state.per_file_cursors.is_empty());
        assert_eq!(state.synced_count(), 0);
    }

    #[tokio::test]
    async fn test_sync_state_save_creates_parent_dir() {
        let temp_dir = TempDir::new().unwrap();
        let nested_path = temp_dir.path().join("nested/dir/.bridge-state.json");

        let state = SyncState::new();
        state.save(&nested_path).await.unwrap();

        assert!(nested_path.exists());
    }

    #[test]
    fn test_assign_message_ids() {
        let mut messages = vec![
            InboxMessage {
                from: "user-a".to_string(),
                text: "Message 1".to_string(),
                timestamp: "2026-02-16T10:00:00Z".to_string(),
                read: false,
                summary: None,
                message_id: None,
                unknown_fields: HashMap::new(),
            },
            InboxMessage {
                from: "user-b".to_string(),
                text: "Message 2".to_string(),
                timestamp: "2026-02-16T10:05:00Z".to_string(),
                read: false,
                summary: None,
                message_id: Some("existing-id".to_string()),
                unknown_fields: HashMap::new(),
            },
            InboxMessage {
                from: "user-c".to_string(),
                text: "Message 3".to_string(),
                timestamp: "2026-02-16T10:10:00Z".to_string(),
                read: false,
                summary: None,
                message_id: None,
                unknown_fields: HashMap::new(),
            },
        ];

        assign_message_ids(&mut messages);

        // First message should get a new UUID
        assert!(messages[0].message_id.is_some());
        let id1 = messages[0].message_id.as_ref().unwrap();
        assert!(Uuid::parse_str(id1).is_ok());

        // Second message should keep its existing ID
        assert_eq!(messages[1].message_id.as_ref().unwrap(), "existing-id");

        // Third message should get a new UUID
        assert!(messages[2].message_id.is_some());
        let id3 = messages[2].message_id.as_ref().unwrap();
        assert!(Uuid::parse_str(id3).is_ok());

        // UUIDs should be different
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_assign_message_ids_empty_vec() {
        let mut messages: Vec<InboxMessage> = Vec::new();
        assign_message_ids(&mut messages);
        assert!(messages.is_empty());
    }

    #[test]
    fn test_sync_state_serialization() {
        let mut state = SyncState::new();
        state.set_cursor(PathBuf::from("inboxes/agent-1.json"), 5);
        state.mark_synced("msg-001".to_string());

        let json = serde_json::to_string(&state).unwrap();
        let deserialized: SyncState = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.get_cursor(Path::new("inboxes/agent-1.json")), 5);
        assert!(deserialized.is_synced("msg-001"));
    }

    #[test]
    fn test_lru_cache_basic() {
        let mut cache = FifoMessageIdCache::new();
        assert!(cache.is_empty());

        cache.insert("msg-001".to_string());
        assert_eq!(cache.len(), 1);
        assert!(cache.contains("msg-001"));
        assert!(!cache.contains("msg-002"));

        cache.insert("msg-002".to_string());
        assert_eq!(cache.len(), 2);
        assert!(cache.contains("msg-001"));
        assert!(cache.contains("msg-002"));
    }

    #[test]
    fn test_lru_cache_duplicate_insert() {
        let mut cache = FifoMessageIdCache::new();
        cache.insert("msg-001".to_string());
        cache.insert("msg-001".to_string());

        // Should only have 1 entry (no duplication)
        assert_eq!(cache.len(), 1);
        assert!(cache.contains("msg-001"));
    }

    #[test]
    fn test_lru_cache_eviction() {
        // Create a small cache for testing eviction
        let mut cache = FifoMessageIdCache {
            ids: HashSet::new(),
            queue: VecDeque::new(),
            max_size: 3,
        };

        // Insert 3 items
        cache.insert("msg-001".to_string());
        cache.insert("msg-002".to_string());
        cache.insert("msg-003".to_string());
        assert_eq!(cache.len(), 3);

        // Insert 4th item - should evict oldest (msg-001)
        cache.insert("msg-004".to_string());
        assert_eq!(cache.len(), 3);
        assert!(!cache.contains("msg-001")); // Evicted
        assert!(cache.contains("msg-002"));
        assert!(cache.contains("msg-003"));
        assert!(cache.contains("msg-004"));

        // Insert 5th item - should evict msg-002
        cache.insert("msg-005".to_string());
        assert_eq!(cache.len(), 3);
        assert!(!cache.contains("msg-002")); // Evicted
        assert!(cache.contains("msg-003"));
        assert!(cache.contains("msg-004"));
        assert!(cache.contains("msg-005"));
    }

    #[test]
    fn test_lru_cache_serialization() {
        let mut cache = FifoMessageIdCache::new();
        cache.insert("msg-001".to_string());
        cache.insert("msg-002".to_string());

        let json = serde_json::to_string(&cache).unwrap();
        let deserialized: FifoMessageIdCache = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.len(), 2);
        assert!(deserialized.contains("msg-001"));
        assert!(deserialized.contains("msg-002"));
    }

    #[test]
    fn test_lru_cache_default_max_size() {
        let cache = FifoMessageIdCache::new();
        assert_eq!(cache.max_size, MAX_SYNCED_MESSAGE_IDS);
    }
}
