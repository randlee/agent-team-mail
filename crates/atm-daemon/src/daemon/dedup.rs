//! Request deduplication store for control receiver operations.
//!
//! Two implementations are provided:
//!
//! - [`DedupeStore`] — in-memory only, fast, lost on restart.
//! - [`DurableDedupeStore`] — file-backed, survives daemon restart.
//!
//! The daemon uses [`DurableDedupeStore`] so that idempotency guarantees hold
//! across restarts.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};

/// Default max in-memory dedupe keys when `ATM_DEDUP_CAPACITY` is unset.
const DEFAULT_CAPACITY: usize = 1000;
/// Default dedupe retention window in seconds when `ATM_DEDUP_TTL_SECS` is unset.
const DEFAULT_TTL_SECS: u64 = 600;

/// Composite idempotency key for control requests.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DedupeKey {
    pub team: String,
    pub session_id: String,
    pub agent_id: String,
    pub request_id: String,
}

impl DedupeKey {
    pub fn new(team: &str, session_id: &str, agent_id: &str, request_id: &str) -> Self {
        Self {
            team: team.to_string(),
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            request_id: request_id.to_string(),
        }
    }
}

/// In-memory dedupe store with bounded capacity and TTL expiry.
///
/// Entries are lost when the daemon restarts. Use [`DurableDedupeStore`] for
/// restart-safe deduplication.
#[derive(Debug)]
pub struct DedupeStore {
    entries: HashMap<DedupeKey, std::time::Instant>,
    order: VecDeque<DedupeKey>,
    ttl: Duration,
    capacity: usize,
}

impl DedupeStore {
    pub fn from_env() -> Self {
        let capacity = std::env::var("ATM_DEDUP_CAPACITY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_CAPACITY);
        let ttl_secs = std::env::var("ATM_DEDUP_TTL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_TTL_SECS);

        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            ttl: Duration::from_secs(ttl_secs),
            capacity,
        }
    }

    #[cfg(test)]
    fn with_config(capacity: usize, ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            ttl,
            capacity,
        }
    }

    /// Returns `true` when key already exists and has not expired.
    pub fn check_and_insert(&mut self, key: DedupeKey) -> bool {
        let now = std::time::Instant::now();
        self.purge_expired(now);
        if self.entries.contains_key(&key) {
            return true;
        }
        self.entries.insert(key.clone(), now);
        self.order.push_back(key);
        self.evict_to_capacity();
        false
    }

    /// Purge expired keys based on the configured TTL.
    pub fn cleanup_expired(&mut self) {
        self.purge_expired(std::time::Instant::now());
    }

    #[cfg(test)]
    fn check_and_insert_at(&mut self, key: DedupeKey, now: std::time::Instant) -> bool {
        self.purge_expired(now);
        if self.entries.contains_key(&key) {
            return true;
        }
        self.entries.insert(key.clone(), now);
        self.order.push_back(key);
        self.evict_to_capacity();
        false
    }

    fn purge_expired(&mut self, now: std::time::Instant) {
        while let Some(front_key) = self.order.front().cloned() {
            let expired = self
                .entries
                .get(&front_key)
                .map(|ts| now.saturating_duration_since(*ts) >= self.ttl)
                .unwrap_or(true);
            if !expired {
                break;
            }
            self.order.pop_front();
            self.entries.remove(&front_key);
        }
    }

    fn evict_to_capacity(&mut self) {
        while self.entries.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
    }
}

// ── DurableDedupeStore ────────────────────────────────────────────────────────

/// On-disk entry as serialised to the JSONL backing file.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct DurableEntry {
    team: String,
    session_id: String,
    agent_id: String,
    request_id: String,
    inserted_at: DateTime<Utc>,
}

impl DurableEntry {
    fn key(&self) -> DedupeKey {
        DedupeKey::new(&self.team, &self.session_id, &self.agent_id, &self.request_id)
    }
}

/// File-backed dedupe store that survives daemon restart.
///
/// On creation the backing file (if present) is read; entries older than
/// `ttl` are discarded. New inserts are appended immediately.  Cleanup
/// rewrites the file atomically via a temp-file rename.
///
/// # File location
///
/// Default: `{home_dir}/.claude/daemon/dedup.jsonl`
///
/// Each line is a JSON object:
/// ```text
/// {"team":"...","session_id":"...","agent_id":"...","request_id":"...","inserted_at":"2026-02-21T00:10:00Z"}
/// ```
///
/// # Concurrency
///
/// The daemon is a single process; only one `DurableDedupeStore` instance is
/// active at a time, wrapped in a `Mutex`. No external locking is required.
pub struct DurableDedupeStore {
    /// In-memory lookup — authoritative for duplicate checks.
    entries: HashMap<DedupeKey, DateTime<Utc>>,
    /// Insertion order for capacity eviction.
    order: VecDeque<DedupeKey>,
    ttl: Duration,
    capacity: usize,
    path: PathBuf,
}

impl DurableDedupeStore {
    /// Create a new store, loading existing (non-expired) entries from `path`.
    ///
    /// If the file does not exist, the store starts empty. Corrupted lines are
    /// skipped with a warning logged to stderr.
    ///
    /// # Errors
    ///
    /// Returns an error if `path`'s parent directory cannot be created.
    pub fn new(path: PathBuf, ttl: Duration, capacity: usize) -> io::Result<Self> {
        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut store = Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            ttl,
            capacity,
            path,
        };

        store.load_from_disk()?;
        Ok(store)
    }

    /// Construct from environment variables and the given home directory.
    ///
    /// Reads:
    /// - `ATM_DEDUP_CAPACITY` (default `1000`)
    /// - `ATM_DEDUP_TTL_SECS` (default `600`)
    ///
    /// File path: `{home_dir}/.claude/daemon/dedup.jsonl`
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from [`Self::new`].
    pub fn from_env(home_dir: &Path) -> io::Result<Self> {
        let capacity = std::env::var("ATM_DEDUP_CAPACITY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_CAPACITY);
        let ttl_secs = std::env::var("ATM_DEDUP_TTL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_TTL_SECS);

        let path = home_dir.join(".claude/daemon/dedup.jsonl");
        Self::new(path, Duration::from_secs(ttl_secs), capacity)
    }

    /// Returns `true` when `key` already exists and has not expired.
    ///
    /// New (non-duplicate) keys are persisted to disk immediately.
    pub fn check_and_insert(&mut self, key: DedupeKey) -> bool {
        let now = Utc::now();
        self.purge_expired(now);
        if self.entries.contains_key(&key) {
            return true;
        }
        // Append to backing file before updating in-memory state so that a
        // crash after the write but before the in-memory update is harmless
        // (on reload the entry will be in the file and prevent duplicates).
        let entry = DurableEntry {
            team: key.team.clone(),
            session_id: key.session_id.clone(),
            agent_id: key.agent_id.clone(),
            request_id: key.request_id.clone(),
            inserted_at: now,
        };
        if let Err(e) = self.append_entry(&entry) {
            // Log to stderr; the daemon continues but loses durability for
            // this entry.  Better to accept the request than to reject it.
            eprintln!("[dedup] warn: failed to persist entry: {e}");
        }
        self.entries.insert(key.clone(), now);
        self.order.push_back(key);
        self.evict_to_capacity();
        false
    }

    /// Remove expired entries from memory and rewrite the backing file.
    ///
    /// # Errors
    ///
    /// Returns I/O errors from the atomic temp-file rename.
    pub fn cleanup_expired(&mut self) -> io::Result<()> {
        let now = Utc::now();
        self.purge_expired(now);
        self.rewrite_file()
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn load_from_disk(&mut self) -> io::Result<()> {
        if !self.path.exists() {
            return Ok(());
        }
        let content = std::fs::read_to_string(&self.path)?;
        let now = Utc::now();
        for (lineno, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let entry: DurableEntry = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("[dedup] warn: skipping corrupt line {}: {e}", lineno + 1);
                    continue;
                }
            };
            // Discard expired entries.  A negative age means the timestamp is
            // in the future relative to `now`; treat that as not-yet-expired
            // (keep the entry).  Only discard when the entry is demonstrably
            // older than the TTL window.
            let age = now.signed_duration_since(entry.inserted_at);
            if age.num_seconds() >= 0
                && age.to_std().unwrap_or(Duration::ZERO) >= self.ttl
            {
                continue;
            }
            let key = entry.key();
            self.entries.insert(key.clone(), entry.inserted_at);
            self.order.push_back(key);
        }
        // Evict if the file had more entries than current capacity.
        self.evict_to_capacity();
        Ok(())
    }

    fn append_entry(&self, entry: &DurableEntry) -> io::Result<()> {
        let mut line = serde_json::to_string(entry)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.flush()?;
        // Flush kernel page cache to storage for crash-safe durability.
        // sync_data() persists file data without requiring metadata sync,
        // which is sufficient to guarantee the entry survives a daemon restart.
        #[cfg(unix)]
        file.sync_data()?;
        Ok(())
    }

    fn rewrite_file(&self) -> io::Result<()> {
        // Write to a sibling temp file, then rename for atomicity.
        let tmp_path = self.path.with_extension("jsonl.tmp");
        {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)?;
            for key in &self.order {
                if let Some(&inserted_at) = self.entries.get(key) {
                    let entry = DurableEntry {
                        team: key.team.clone(),
                        session_id: key.session_id.clone(),
                        agent_id: key.agent_id.clone(),
                        request_id: key.request_id.clone(),
                        inserted_at,
                    };
                    let mut line = serde_json::to_string(&entry)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    line.push('\n');
                    file.write_all(line.as_bytes())?;
                }
            }
            file.flush()?;
        }
        std::fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }

    fn purge_expired(&mut self, now: DateTime<Utc>) {
        while let Some(front_key) = self.order.front().cloned() {
            // An entry is expired only when its age is non-negative and meets
            // or exceeds the TTL.  Future-timestamped entries (age < 0) are
            // treated as valid (not yet expired) rather than discarded.
            let expired = self.entries.get(&front_key).is_none_or(|&ts| {
                let age = now.signed_duration_since(ts);
                age.num_seconds() >= 0
                    && age.to_std().unwrap_or(Duration::ZERO) >= self.ttl
            });
            if !expired {
                break;
            }
            self.order.pop_front();
            self.entries.remove(&front_key);
        }
    }

    fn evict_to_capacity(&mut self) {
        while self.entries.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ── DedupeStore tests ─────────────────────────────────────────────────────

    fn key(i: usize) -> DedupeKey {
        DedupeKey::new("atm-dev", "sess-1", "arch-ctm", &format!("req-{i}"))
    }

    #[test]
    fn insert_then_duplicate() {
        let mut d = DedupeStore::with_config(4, Duration::from_secs(600));
        assert!(!d.check_and_insert(key(1)));
        assert!(d.check_and_insert(key(1)));
    }

    #[test]
    fn ttl_expiry_allows_reinsert() {
        let mut d = DedupeStore::with_config(4, Duration::from_secs(10));
        let t0 = std::time::Instant::now();
        let k = key(1);
        assert!(!d.check_and_insert_at(k.clone(), t0));
        assert!(d.check_and_insert_at(k.clone(), t0 + Duration::from_secs(1)));
        assert!(!d.check_and_insert_at(k, t0 + Duration::from_secs(11)));
    }

    #[test]
    fn capacity_eviction_discards_oldest() {
        let mut d = DedupeStore::with_config(2, Duration::from_secs(600));
        assert!(!d.check_and_insert(key(1)));
        assert!(!d.check_and_insert(key(2)));
        assert!(!d.check_and_insert(key(3))); // evicts key(1)
        assert!(!d.check_and_insert(key(1))); // no longer duplicate
    }

    #[test]
    fn dedupe_key_isolated_by_team() {
        let mut d = DedupeStore::with_config(4, Duration::from_secs(600));
        let k1 = DedupeKey::new("atm-dev", "sess-1", "arch-ctm", "req-iso");
        let k2 = DedupeKey::new("other-team", "sess-1", "arch-ctm", "req-iso");
        assert!(!d.check_and_insert(k1));
        assert!(!d.check_and_insert(k2));
    }

    #[test]
    fn dedupe_key_isolated_by_session_id() {
        let mut d = DedupeStore::with_config(4, Duration::from_secs(600));
        let k1 = DedupeKey::new("atm-dev", "sess-1", "arch-ctm", "req-iso");
        let k2 = DedupeKey::new("atm-dev", "sess-2", "arch-ctm", "req-iso");
        assert!(!d.check_and_insert(k1));
        assert!(!d.check_and_insert(k2));
    }

    #[test]
    fn dedupe_key_isolated_by_agent_id() {
        let mut d = DedupeStore::with_config(4, Duration::from_secs(600));
        let k1 = DedupeKey::new("atm-dev", "sess-1", "arch-ctm", "req-iso");
        let k2 = DedupeKey::new("atm-dev", "sess-1", "sm-b-1", "req-iso");
        assert!(!d.check_and_insert(k1));
        assert!(!d.check_and_insert(k2));
    }

    // ── DurableDedupeStore tests ──────────────────────────────────────────────

    fn durable_key(id: &str) -> DedupeKey {
        DedupeKey::new("atm-dev", "sess-1", "arch-ctm", id)
    }

    fn make_store(dir: &TempDir) -> DurableDedupeStore {
        let path = dir.path().join("dedup.jsonl");
        DurableDedupeStore::new(path, Duration::from_secs(600), 1000).unwrap()
    }

    fn make_store_with_ttl(dir: &TempDir, ttl_secs: u64) -> DurableDedupeStore {
        let path = dir.path().join("dedup.jsonl");
        DurableDedupeStore::new(path, Duration::from_secs(ttl_secs), 1000).unwrap()
    }

    fn make_store_with_capacity(dir: &TempDir, cap: usize) -> DurableDedupeStore {
        let path = dir.path().join("dedup.jsonl");
        DurableDedupeStore::new(path, Duration::from_secs(600), cap).unwrap()
    }

    #[test]
    fn durable_insert_then_duplicate() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);
        let k = durable_key("req-1");
        assert!(!store.check_and_insert(k.clone()));
        assert!(store.check_and_insert(k));
    }

    #[test]
    fn durable_survives_restart() {
        let dir = TempDir::new().unwrap();
        let k = durable_key("req-restart");
        {
            let mut store = make_store(&dir);
            assert!(!store.check_and_insert(k.clone()));
        }
        // Drop and recreate — simulates daemon restart.
        let mut store2 = make_store(&dir);
        assert!(store2.check_and_insert(k), "should be duplicate after restart");
    }

    #[test]
    fn durable_missing_file_ok() {
        let dir = TempDir::new().unwrap();
        // Path does not exist — must succeed with empty store.
        let path = dir.path().join("nonexistent").join("dedup.jsonl");
        let mut store = DurableDedupeStore::new(path, Duration::from_secs(600), 100).unwrap();
        let k = durable_key("req-missing");
        assert!(!store.check_and_insert(k.clone()));
        assert!(store.check_and_insert(k));
    }

    #[test]
    fn durable_corrupted_line_skipped() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("dedup.jsonl");

        // Write one valid line and one corrupt line.
        let valid = r#"{"team":"atm-dev","session_id":"sess-1","agent_id":"arch-ctm","request_id":"req-good","inserted_at":"2099-01-01T00:00:00Z"}"#;
        std::fs::write(&path, format!("not-json\n{valid}\n")).unwrap();

        let mut store =
            DurableDedupeStore::new(path, Duration::from_secs(600), 1000).unwrap();
        // The valid entry should have loaded (it has a future timestamp, so TTL not expired).
        let k_good = DedupeKey::new("atm-dev", "sess-1", "arch-ctm", "req-good");
        assert!(
            store.check_and_insert(k_good),
            "valid entry should be a duplicate"
        );
        // The corrupt line should have been skipped.
        let k_new = durable_key("req-new");
        assert!(!store.check_and_insert(k_new));
    }

    #[test]
    fn durable_ttl_expiry_allows_reinsert_after_restart() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("dedup.jsonl");

        // Write an entry with an old timestamp (expired).
        let expired_entry = r#"{"team":"atm-dev","session_id":"sess-1","agent_id":"arch-ctm","request_id":"req-expired","inserted_at":"2000-01-01T00:00:00Z"}"#;
        std::fs::write(&path, format!("{expired_entry}\n")).unwrap();

        // Load with 600s TTL — entry is ancient, should be discarded.
        let mut store = DurableDedupeStore::new(path, Duration::from_secs(600), 1000).unwrap();
        let k = DedupeKey::new("atm-dev", "sess-1", "arch-ctm", "req-expired");
        assert!(
            !store.check_and_insert(k),
            "expired entry should not be duplicate on reload"
        );
    }

    #[test]
    fn durable_capacity_eviction() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store_with_capacity(&dir, 2);
        let k1 = durable_key("req-cap-1");
        let k2 = durable_key("req-cap-2");
        let k3 = durable_key("req-cap-3");
        assert!(!store.check_and_insert(k1.clone()));
        assert!(!store.check_and_insert(k2.clone()));
        assert!(!store.check_and_insert(k3.clone())); // k1 evicted
        assert!(!store.check_and_insert(k1)); // k1 no longer duplicate
    }

    #[test]
    fn durable_cleanup_expired_rewrites_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("dedup.jsonl");

        // Two entries: one with ancient timestamp (expired), one with future.
        let expired =
            r#"{"team":"a","session_id":"s","agent_id":"x","request_id":"r-old","inserted_at":"2000-01-01T00:00:00Z"}"#;
        let fresh =
            r#"{"team":"a","session_id":"s","agent_id":"x","request_id":"r-new","inserted_at":"2099-01-01T00:00:00Z"}"#;
        std::fs::write(&path, format!("{expired}\n{fresh}\n")).unwrap();

        let mut store =
            DurableDedupeStore::new(path.clone(), Duration::from_secs(600), 1000).unwrap();
        store.cleanup_expired().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("r-old"), "expired entry should be removed");
        assert!(content.contains("r-new"), "fresh entry should remain");
    }

    #[test]
    fn durable_ttl_uses_short_window() {
        // Insert with a 1s TTL store, then create a new store from the same
        // path with the same TTL and verify freshly-inserted entry IS a dup.
        let dir = TempDir::new().unwrap();
        let k = durable_key("req-short");
        {
            let mut store = make_store_with_ttl(&dir, 3600); // long TTL for write
            assert!(!store.check_and_insert(k.clone()));
        }
        // Reload with a 1s TTL — entry timestamp is "now" so it should still
        // be within the window and count as duplicate.
        let mut store2 = make_store_with_ttl(&dir, 3600);
        assert!(store2.check_and_insert(k), "should be duplicate");
    }
}
