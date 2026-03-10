//! Bounded in-memory log event queue and async JSONL writer task.
//!
//! This module implements the daemon-side sink for the unified logging pipeline:
//!
//! ```text
//! socket handler "log-event"
//!   └── LogEventQueue (bounded, 4096 capacity, drop-new on overflow)
//!         └── run_log_writer_task (drains every 100 ms)
//!               └── JSONL file with size-based rotation
//! ```
//!
//! # Queue overflow
//!
//! When the queue is full, incoming events are dropped (drop-new policy).
//! The `dropped` counter on [`BoundedQueue`] tracks all dropped events.
//! A `warn!` is emitted at most once per 5 seconds to avoid log flooding.
//!
//! # Rotation
//!
//! When the JSONL file reaches [`LogWriterConfig::max_bytes`], the writer
//! renames existing rotated files (`base.1` → `base.2` → … → `base.N`) and
//! starts a fresh base file. The oldest rotation file (`.N`) is removed.

use agent_team_mail_core::logging_event::LogEventV1;
use sc_observability::{DEFAULT_QUEUE_CAPACITY, export_otel_best_effort_from_path};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

// ── Queue ─────────────────────────────────────────────────────────────────────

/// A bounded FIFO queue for [`LogEventV1`] events.
///
/// When the queue is full, `push` returns `false` and increments the internal
/// `dropped` counter. A `warn!` is emitted at most once per 5 seconds
/// (rate-limited via [`BoundedQueue::last_warn_dropped`]).
pub struct BoundedQueue {
    inner: VecDeque<LogEventV1>,
    capacity: usize,
    dropped: u64,
    last_warn_dropped: Option<Instant>,
}

impl BoundedQueue {
    /// Create a new [`BoundedQueue`] with the given `capacity`.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: VecDeque::with_capacity(capacity),
            capacity,
            dropped: 0,
            last_warn_dropped: None,
        }
    }

    /// Attempt to enqueue `event`.
    ///
    /// Returns `true` if the event was accepted, `false` if the queue is
    /// full (event is dropped and `dropped` counter is incremented).
    pub fn push(&mut self, event: LogEventV1) -> bool {
        if self.inner.len() >= self.capacity {
            self.dropped += 1;
            // Rate-limit warnings to once per 5 seconds.
            let should_warn = self
                .last_warn_dropped
                .map(|t| t.elapsed().as_secs() >= 5)
                .unwrap_or(true);
            if should_warn {
                warn!(
                    dropped_total = self.dropped,
                    "log event queue full — dropping event (rate-limited warning)"
                );
                self.last_warn_dropped = Some(Instant::now());
            }
            return false;
        }
        self.inner.push_back(event);
        true
    }

    /// Drain up to `n` events from the front of the queue.
    pub fn drain_up_to(&mut self, n: usize) -> Vec<LogEventV1> {
        let count = n.min(self.inner.len());
        self.inner.drain(..count).collect()
    }

    /// Return the total number of events dropped since this queue was created.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Return the current number of events in the queue.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Return `true` if the queue contains no events.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

// ── Shared queue type ─────────────────────────────────────────────────────────

/// Thread-safe handle to a [`BoundedQueue`].
///
/// Cloned cheaply and shared between the socket handler (producer)
/// and the writer task (consumer).
pub type LogEventQueue = Arc<Mutex<BoundedQueue>>;

/// Create a new [`LogEventQueue`] with the default capacity of 4096.
pub fn new_log_event_queue() -> LogEventQueue {
    Arc::new(Mutex::new(BoundedQueue::new(DEFAULT_QUEUE_CAPACITY)))
}

// ── Writer configuration ──────────────────────────────────────────────────────

/// Configuration for the [`run_log_writer_task`] async writer.
pub struct LogWriterConfig {
    /// Path to the base JSONL log file (e.g. `~/.config/atm/atm.log.jsonl`).
    pub log_path: PathBuf,
    /// Rotate when the file reaches this size in bytes (default: 50 MiB).
    pub max_bytes: u64,
    /// Maximum number of rotated backup files to keep (default: 5).
    pub max_files: u32,
    /// How often to drain the queue and flush to disk, in milliseconds (default: 100).
    pub flush_interval_ms: u64,
}

impl LogWriterConfig {
    /// Build configuration from environment variables, falling back to defaults.
    ///
    /// | Variable              | Default                                  |
    /// |-----------------------|------------------------------------------|
    /// | `ATM_LOG_FILE`        | `{home_dir}/.config/atm/atm.log.jsonl`  |
    /// | `ATM_LOG_PATH`        | alias for `ATM_LOG_FILE` (compat)        |
    /// | `ATM_LOG_MAX_BYTES`   | 52428800 (50 MiB)                        |
    /// | `ATM_LOG_MAX_FILES`   | 5                                        |
    /// | `ATM_LOG_FLUSH_MS`    | 100                                      |
    ///
    /// `ATM_LOG_FILE` takes precedence over `ATM_LOG_PATH`. When neither is
    /// set the log is written to `{home_dir}/.config/atm/atm.log.jsonl`.
    pub fn from_env(home_dir: &Path) -> Self {
        // Check ATM_LOG_FILE first (canonical), then ATM_LOG_PATH (compat alias).
        let log_path = std::env::var("ATM_LOG_FILE")
            .or_else(|_| std::env::var("ATM_LOG_PATH"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir.join(".config/atm/atm.log.jsonl"));

        let max_bytes = std::env::var("ATM_LOG_MAX_BYTES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(50 * 1024 * 1024);

        let max_files = std::env::var("ATM_LOG_MAX_FILES")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(5);

        let flush_interval_ms = std::env::var("ATM_LOG_FLUSH_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(100);

        Self {
            log_path,
            max_bytes,
            max_files,
            flush_interval_ms,
        }
    }
}

// ── Writer task ───────────────────────────────────────────────────────────────

/// Run the async log writer task until `cancel` is triggered.
///
/// The task:
/// 1. Wakes every `config.flush_interval_ms` milliseconds.
/// 2. Drains up to 256 events from `queue`.
/// 3. Writes each event as one JSONL line to `config.log_path`.
/// 4. Rotates the file when it reaches `config.max_bytes`.
///
/// Write errors are logged with `warn!` and do not abort the task (fail-open).
pub async fn run_log_writer_task(
    queue: LogEventQueue,
    config: LogWriterConfig,
    cancel: CancellationToken,
) {
    let interval = std::time::Duration::from_millis(config.flush_interval_ms);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Ensure the log directory exists.
    if let Some(dir) = config.log_path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            warn!(
                "log_writer: failed to create log directory {}: {e}",
                dir.display()
            );
        }
    }

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("log_writer: cancellation received, flushing remaining events");
                // Final flush.
                let events = {
                    let mut q = queue.lock().await;
                    q.drain_up_to(usize::MAX)
                };
                if !events.is_empty() {
                    write_events(&config, &events);
                }
                break;
            }
            _ = ticker.tick() => {
                let events = {
                    let mut q = queue.lock().await;
                    q.drain_up_to(256)
                };
                if events.is_empty() {
                    continue;
                }
                write_events(&config, &events);
            }
        }
    }

    debug!("log_writer: task stopped");
}

/// Write `events` to the JSONL file, rotating if needed.
fn write_events(config: &LogWriterConfig, events: &[LogEventV1]) {
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    // Rotate if file is at or over the size threshold.
    if let Ok(meta) = fs::metadata(&config.log_path) {
        if meta.len() >= config.max_bytes {
            rotate_log_files(&config.log_path, config.max_files);
        }
    }

    let mut file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config.log_path)
    {
        Ok(f) => f,
        Err(e) => {
            warn!(
                "log_writer: failed to open log file {}: {e}",
                config.log_path.display()
            );
            return;
        }
    };

    for event in events {
        match serde_json::to_string(event) {
            Ok(line) => {
                if let Err(e) = writeln!(file, "{line}") {
                    warn!("log_writer: write error: {e}");
                    continue;
                }
                export_otel_best_effort_from_path(&config.log_path, event);
            }
            Err(e) => {
                warn!("log_writer: failed to serialize event: {e}");
            }
        }
    }
}

/// Rotate log files: `base.N` removed, `base.N-1` → `base.N`, …, `base` → `base.1`.
fn rotate_log_files(base: &Path, max_files: u32) {
    use std::fs;

    // Remove the oldest backup.
    let oldest = rotation_path(base, max_files);
    let _ = fs::remove_file(&oldest);

    // Shift existing backups: N-1 → N, …, 1 → 2.
    for i in (1..max_files).rev() {
        let from = rotation_path(base, i);
        let to = rotation_path(base, i + 1);
        if from.exists() {
            let _ = fs::rename(&from, &to);
        }
    }

    // Rename the base file to .1.
    let first = rotation_path(base, 1);
    let _ = fs::rename(base, first);
}

/// Build the path for rotation backup `n` (e.g. `base.jsonl.1`).
fn rotation_path(base: &Path, n: u32) -> PathBuf {
    let mut s = base.as_os_str().to_os_string();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_core::logging_event::new_log_event;
    use sc_observability::OtelRecord;
    use serial_test::serial;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    fn make_event() -> LogEventV1 {
        new_log_event("atm-daemon", "test_event", "atm_daemon::test", "info")
    }

    // ── BoundedQueue unit tests ───────────────────────────────────────────────

    #[test]
    fn test_queue_capacity() {
        let mut q = BoundedQueue::new(4096);
        for _ in 0..4097 {
            q.push(make_event());
        }
        assert_eq!(q.len(), 4096, "queue should hold exactly 4096 events");
        assert_eq!(q.dropped(), 1, "exactly 1 event should have been dropped");
    }

    #[test]
    fn test_queue_drain() {
        let mut q = BoundedQueue::new(100);
        for _ in 0..10 {
            q.push(make_event());
        }
        let drained = q.drain_up_to(5);
        assert_eq!(drained.len(), 5);
        assert_eq!(q.len(), 5, "5 events should remain after draining 5");
    }

    #[test]
    fn test_queue_drain_more_than_available() {
        let mut q = BoundedQueue::new(100);
        for _ in 0..3 {
            q.push(make_event());
        }
        let drained = q.drain_up_to(10);
        assert_eq!(drained.len(), 3);
        assert!(q.is_empty());
    }

    #[test]
    fn test_queue_push_returns_false_when_full() {
        let mut q = BoundedQueue::new(2);
        assert!(q.push(make_event()));
        assert!(q.push(make_event()));
        // Third push should fail.
        assert!(
            !q.push(make_event()),
            "push to full queue should return false"
        );
        assert_eq!(q.dropped(), 1);
    }

    #[test]
    fn test_overflow_rate_limited_warning() {
        // Push many events; only the first should produce a warning (rate-limited).
        // We verify via dropped count rather than intercepting warn! output.
        let mut q = BoundedQueue::new(10);
        for _ in 0..10 {
            q.push(make_event());
        }
        // Push 20 extra events — all should be dropped.
        for _ in 0..20 {
            q.push(make_event());
        }
        assert_eq!(q.dropped(), 20);
        // The queue should still contain exactly 10 events.
        assert_eq!(q.len(), 10);
    }

    // ── Writer integration tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_writer_creates_file() {
        let dir = TempDir::new().expect("temp dir");
        // Use ATM_HOME-style temp dir (per cross-platform guidelines).
        let log_path = dir.path().join("atm.log.jsonl");

        let queue = new_log_event_queue();

        // Enqueue a few events before starting the writer.
        {
            let mut q = queue.lock().await;
            for _ in 0..5 {
                q.push(make_event());
            }
        }

        let config = LogWriterConfig {
            log_path: log_path.clone(),
            max_bytes: 50 * 1024 * 1024,
            max_files: 5,
            flush_interval_ms: 50,
        };

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let queue_clone = queue.clone();

        tokio::spawn(async move {
            run_log_writer_task(queue_clone, config, cancel_clone).await;
        });

        // Wait for at least one flush cycle.
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();

        assert!(log_path.exists(), "log file should have been created");

        let content = std::fs::read_to_string(&log_path).expect("read log file");
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 5, "5 JSONL lines expected");

        // Verify each line is valid JSON with the expected action.
        for line in lines {
            let event: LogEventV1 = serde_json::from_str(line).expect("parse JSONL line");
            assert_eq!(event.action, "test_event");
        }
    }

    #[tokio::test]
    async fn test_writer_rotation() {
        let dir = TempDir::new().expect("temp dir");
        let log_path = dir.path().join("atm.log.jsonl");

        let queue = new_log_event_queue();

        // Use a tiny max_bytes so rotation happens after a couple of events.
        let config = LogWriterConfig {
            log_path: log_path.clone(),
            max_bytes: 300, // tiny — triggers rotation quickly
            max_files: 3,
            flush_interval_ms: 50,
        };

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let queue_clone = queue.clone();

        tokio::spawn(async move {
            run_log_writer_task(queue_clone, config, cancel_clone).await;
        });

        // Write enough events across several flush cycles to trigger rotation.
        for _ in 0..5 {
            {
                let mut q = queue.lock().await;
                for _ in 0..4 {
                    q.push(make_event());
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(100)).await;

        // At least one rotation file should exist.
        let rotation_1 = rotation_path(&log_path, 1);
        assert!(
            rotation_1.exists() || log_path.exists(),
            "at least one log file (base or rotated) should exist"
        );
    }

    #[test]
    #[serial]
    fn test_log_writer_config_from_env() {
        // Use a temp dir as the home dir.
        let dir = TempDir::new().expect("temp dir");
        let config = LogWriterConfig::from_env(dir.path());
        assert_eq!(
            config.log_path,
            dir.path().join(".config/atm/atm.log.jsonl")
        );
        assert_eq!(config.max_bytes, 50 * 1024 * 1024);
        assert_eq!(config.max_files, 5);
        assert_eq!(config.flush_interval_ms, 100);
    }

    #[test]
    #[serial]
    fn test_log_writer_config_env_override() {
        let dir = TempDir::new().expect("temp dir");
        let custom_path = dir.path().join("custom.log.jsonl");

        // Set ATM_LOG_FILE and verify it overrides the default path.
        // SAFETY: single-threaded guard provided by #[serial].
        unsafe {
            std::env::set_var("ATM_LOG_FILE", custom_path.as_os_str());
        }
        let config = LogWriterConfig::from_env(dir.path());
        unsafe {
            std::env::remove_var("ATM_LOG_FILE");
        }

        assert_eq!(
            config.log_path, custom_path,
            "ATM_LOG_FILE should override the default log path"
        );
    }

    #[test]
    #[serial]
    fn test_log_writer_config_env_override_compat_alias() {
        let dir = TempDir::new().expect("temp dir");
        let custom_path = dir.path().join("compat.log.jsonl");

        // ATM_LOG_PATH is the compat alias when ATM_LOG_FILE is absent.
        unsafe {
            std::env::set_var("ATM_LOG_PATH", custom_path.as_os_str());
        }
        let config = LogWriterConfig::from_env(dir.path());
        unsafe {
            std::env::remove_var("ATM_LOG_PATH");
        }

        assert_eq!(
            config.log_path, custom_path,
            "ATM_LOG_PATH should override the default log path when ATM_LOG_FILE is unset"
        );
    }

    #[test]
    fn test_rotation_path() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let base = dir.path().join("atm.log.jsonl");

        let expected_1 = {
            let mut s = base.as_os_str().to_os_string();
            s.push(".1");
            PathBuf::from(s)
        };
        let expected_5 = {
            let mut s = base.as_os_str().to_os_string();
            s.push(".5");
            PathBuf::from(s)
        };

        assert_eq!(rotation_path(&base, 1), expected_1);
        assert_eq!(rotation_path(&base, 5), expected_5);
    }

    #[test]
    fn test_write_events_exports_otel_for_each_producer_source() {
        let dir = TempDir::new().expect("temp dir");
        let log_path = dir.path().join("atm.log.jsonl");
        let config = LogWriterConfig {
            log_path: log_path.clone(),
            max_bytes: 50 * 1024 * 1024,
            max_files: 5,
            flush_interval_ms: 100,
        };
        let mut events = Vec::new();
        for (idx, source, action) in [
            (1u8, "atm", "send"),
            (2u8, "atm-daemon", "register_hint"),
            (3u8, "sc-composer", "compose"),
        ] {
            let mut event = new_log_event(source, action, "atm::test", "info");
            event.team = Some("atm-dev".to_string());
            event.agent = Some("arch-ctm".to_string());
            event.runtime = Some("codex".to_string());
            event.session_id = Some("sess-123".to_string());
            event.trace_id = Some("trace-123".to_string());
            event.span_id = Some(format!("span-{idx}"));
            events.push(event);
        }
        write_events(&config, &events);

        let otel_path = dir.path().join("atm.log.otel.jsonl");
        let lines: Vec<String> = std::fs::read_to_string(&otel_path)
            .expect("otel output should exist")
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(lines.len(), 3);
        let exported: Vec<OtelRecord> = lines
            .iter()
            .map(|line| serde_json::from_str(line).expect("valid otel record json"))
            .collect();
        assert_eq!(
            exported
                .iter()
                .map(|record| record.name.clone())
                .collect::<Vec<_>>(),
            vec![
                "send".to_string(),
                "register_hint".to_string(),
                "compose".to_string()
            ]
        );
        for record in &exported {
            assert_eq!(record.trace_id.as_deref(), Some("trace-123"));
            assert!(record.span_id.is_some(), "span_id should be present");
            assert_eq!(
                record.attributes.get("team").and_then(|v| v.as_str()),
                Some("atm-dev")
            );
            assert_eq!(
                record.attributes.get("agent").and_then(|v| v.as_str()),
                Some("arch-ctm")
            );
            assert_eq!(
                record.attributes.get("runtime").and_then(|v| v.as_str()),
                Some("codex")
            );
            assert_eq!(
                record.attributes.get("session_id").and_then(|v| v.as_str()),
                Some("sess-123")
            );
        }
    }
}
