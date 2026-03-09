//! Shared observability primitives for ATM ecosystem tools.
//!
//! AH.1 scope:
//! - `Logger` + `emit()` with JSONL rotation
//! - `LogConfig` with environment-driven defaults
//! - spool write/merge semantics
//! - socket error-code constants for the `log-event` contract

use agent_team_mail_core::logging_event::{LogEventV1, ValidationError};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

pub const DEFAULT_QUEUE_CAPACITY: usize = 4096;
pub const DEFAULT_MAX_EVENT_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_BYTES: u64 = 50 * 1024 * 1024;
pub const DEFAULT_MAX_FILES: u32 = 5;
pub const DEFAULT_RETENTION_DAYS: u32 = 7;

pub const SOCKET_ERROR_VERSION_MISMATCH: &str = "VERSION_MISMATCH";
pub const SOCKET_ERROR_INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
pub const SOCKET_ERROR_INTERNAL_ERROR: &str = "INTERNAL_ERROR";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl FromStr for LogLevel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogConfig {
    pub log_path: PathBuf,
    pub spool_dir: PathBuf,
    pub level: LogLevel,
    pub message_preview_enabled: bool,
    pub max_bytes: u64,
    pub max_files: u32,
    pub retention_days: u32,
    pub queue_capacity: usize,
    pub max_event_bytes: usize,
}

impl LogConfig {
    pub fn from_home(home_dir: &Path) -> Self {
        let log_path = std::env::var("ATM_LOG_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir.join(".config/atm/atm.log.jsonl"));

        let spool_dir = home_dir.join(".config/atm/log-spool");
        let level = std::env::var("ATM_LOG")
            .ok()
            .and_then(|v| LogLevel::from_str(&v).ok())
            .unwrap_or(LogLevel::Info);
        let message_preview_enabled = std::env::var("ATM_LOG_MSG")
            .ok()
            .map(|v| v.trim() == "1")
            .unwrap_or(false);
        let max_bytes = std::env::var("ATM_LOG_MAX_BYTES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MAX_BYTES);
        let max_files = std::env::var("ATM_LOG_MAX_FILES")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(DEFAULT_MAX_FILES);
        let retention_days = std::env::var("ATM_LOG_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|days| *days > 0)
            .unwrap_or(DEFAULT_RETENTION_DAYS);

        Self {
            log_path,
            spool_dir,
            level,
            message_preview_enabled,
            max_bytes,
            max_files,
            retention_days,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        }
    }
}

#[derive(Debug, Error)]
pub enum LoggerError {
    #[error("event validation failed: {0}")]
    Validation(#[from] ValidationError),
    #[error("failed to serialize log event: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("event exceeds configured size guard: {size} > {max}")]
    EventTooLarge { size: usize, max: usize },
}

#[derive(Debug, Clone)]
pub struct Logger {
    config: LogConfig,
}

/// Apply canonical redaction rules to a logging event.
pub fn redact_event(event: &mut LogEventV1) {
    event.redact();
}

impl Logger {
    pub fn new(config: LogConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &LogConfig {
        &self.config
    }

    /// Validate, redact, and append an event to the canonical JSONL log.
    ///
    /// # Errors
    ///
    /// Returns an error when validation fails, serialization fails, the event
    /// exceeds `max_event_bytes`, or filesystem writes fail.
    pub fn emit(&self, event: &LogEventV1) -> Result<(), LoggerError> {
        let line = self.prepare_line(event)?;
        self.append_line_to_canonical(&line)?;
        Ok(())
    }

    /// Convenience helper for tools that only need action/outcome + fields.
    ///
    /// This builds a [`LogEventV1`] with the configured log level and emits it
    /// through the same validation/redaction/path pipeline as [`Self::emit`].
    pub fn emit_action(
        &self,
        source_binary: &str,
        target: &str,
        action: &str,
        outcome: Option<&str>,
        fields: serde_json::Value,
    ) -> Result<(), LoggerError> {
        let mut event = LogEventV1::builder(source_binary, action, target)
            .level(self.config.level.as_str())
            .build();
        event.outcome = outcome.map(ToOwned::to_owned);
        event.fields = value_to_map(fields);
        self.emit(&event)
    }
    /// Write one event to a per-source spool file for deferred fan-in merge.
    ///
    /// # Errors
    ///
    /// Returns an error when validation/serialization fails, the event exceeds
    /// `max_event_bytes`, or spool file creation/appending fails.
    pub fn write_to_spool(
        &self,
        event: &LogEventV1,
        unix_millis: u128,
    ) -> Result<PathBuf, LoggerError> {
        let line = self.prepare_line(event)?;
        fs::create_dir_all(&self.config.spool_dir)?;

        let name = spool_file_name(&event.source_binary, event.pid, unix_millis);
        let path = self.config.spool_dir.join(name);
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(file, "{line}")?;
        Ok(path)
    }

    /// Merge spool fragments into the canonical log in deterministic order.
    ///
    /// Supports crash-recovery of stale `.claiming` files from interrupted
    /// prior merges.
    ///
    /// # Errors
    ///
    /// Returns an error when reading the spool directory or writing to the
    /// canonical log fails.
    pub fn merge_spool(&self) -> Result<u64, LoggerError> {
        if !self.config.spool_dir.exists() {
            return Ok(0);
        }

        let mut spool_files: Vec<PathBuf> = fs::read_dir(&self.config.spool_dir)?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| {
                path.is_file()
                    && path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext == "jsonl" || ext == "claiming")
                        .unwrap_or(false)
            })
            .collect();
        spool_files.sort();

        let mut claimed_files: Vec<PathBuf> = Vec::new();
        let mut events: Vec<(LogEventV1, String)> = Vec::new();

        for path in spool_files {
            let claiming = if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "claiming")
            {
                path.clone()
            } else {
                let claiming = path.with_extension("claiming");
                if fs::rename(&path, &claiming).is_err() {
                    continue;
                }
                claiming
            };
            let ordering_key = claiming
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();

            let content = match fs::read_to_string(&claiming) {
                Ok(content) => content,
                Err(_) => {
                    let _ = fs::remove_file(&claiming);
                    continue;
                }
            };
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(event) = serde_json::from_str::<LogEventV1>(trimmed) {
                    events.push((event, ordering_key.clone()));
                }
            }
            claimed_files.push(claiming);
        }

        events.sort_by(|(a, file_a), (b, file_b)| a.ts.cmp(&b.ts).then(file_a.cmp(file_b)));

        let mut merged = 0_u64;
        for (event, _) in events {
            let line = serde_json::to_string(&event)?;
            if line.len() > self.config.max_event_bytes {
                continue;
            }
            self.append_line_to_canonical(&line)?;
            merged += 1;
        }

        for claimed in claimed_files {
            let _ = fs::remove_file(claimed);
        }

        Ok(merged)
    }

    fn prepare_line(&self, event: &LogEventV1) -> Result<String, LoggerError> {
        let mut event = event.clone();
        event.validate()?;
        redact_event(&mut event);
        let line = serde_json::to_string(&event)?;
        let size = line.len();
        if size > self.config.max_event_bytes {
            return Err(LoggerError::EventTooLarge {
                size,
                max: self.config.max_event_bytes,
            });
        }
        Ok(line)
    }

    fn append_line_to_canonical(&self, line: &str) -> Result<(), LoggerError> {
        if let Some(parent) = self.config.log_path.parent() {
            fs::create_dir_all(parent)?;
        }

        self.rotate_if_needed()?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.log_path)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    fn rotate_if_needed(&self) -> Result<(), LoggerError> {
        let current_size = fs::metadata(&self.config.log_path)
            .map(|m| m.len())
            .unwrap_or(0);
        if current_size < self.config.max_bytes {
            return Ok(());
        }
        rotate_log_files(&self.config.log_path, self.config.max_files)?;
        Ok(())
    }
}

pub fn spool_file_name(source_binary: &str, pid: u32, unix_millis: u128) -> String {
    let sanitized = sanitize_source_binary(source_binary);
    format!("{}-{}-{}.jsonl", sanitized, pid, unix_millis)
}

fn sanitize_source_binary(source_binary: &str) -> String {
    let mut out = String::with_capacity(source_binary.len());
    for ch in source_binary.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

fn rotate_log_files(base: &Path, max_files: u32) -> Result<(), LoggerError> {
    if max_files == 0 {
        let _ = fs::remove_file(base);
        return Ok(());
    }

    let oldest = rotation_path(base, max_files);
    let _ = fs::remove_file(&oldest);

    for idx in (1..max_files).rev() {
        let from = rotation_path(base, idx);
        let to = rotation_path(base, idx + 1);
        if from.exists() {
            let _ = fs::rename(&from, &to);
        }
    }

    if base.exists() {
        let first = rotation_path(base, 1);
        fs::rename(base, first)?;
    }
    Ok(())
}

fn rotation_path(base: &Path, n: u32) -> PathBuf {
    let mut os = base.as_os_str().to_os_string();
    os.push(format!(".{n}"));
    PathBuf::from(os)
}

fn value_to_map(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("value".to_string(), other);
            map
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_core::logging_event::new_log_event;
    use serial_test::serial;
    use tempfile::TempDir;

    fn make_event(ts: &str) -> LogEventV1 {
        let mut event = new_log_event("atm", "test_action", "atm::test", "info");
        event.ts = ts.to_string();
        event
    }

    #[test]
    #[serial]
    fn config_defaults_and_env_overrides() {
        let tmp = TempDir::new().expect("temp dir");
        let custom_log = tmp.path().join("custom-atm.log");
        let home_root = tmp.path().join("home-root");
        // SAFETY: test-scoped env mutation.
        unsafe {
            std::env::set_var("ATM_LOG", "debug");
            std::env::set_var("ATM_LOG_MSG", "1");
            std::env::set_var("ATM_LOG_FILE", &custom_log);
            std::env::set_var("ATM_LOG_MAX_BYTES", "1024");
            std::env::set_var("ATM_LOG_MAX_FILES", "7");
            std::env::set_var("ATM_LOG_RETENTION_DAYS", "9");
        }
        let cfg = LogConfig::from_home(&home_root);
        assert_eq!(cfg.level, LogLevel::Debug);
        assert!(cfg.message_preview_enabled);
        assert_eq!(cfg.log_path, custom_log);
        assert_eq!(cfg.max_bytes, 1024);
        assert_eq!(cfg.max_files, 7);
        assert_eq!(cfg.retention_days, 9);
        assert_eq!(cfg.queue_capacity, DEFAULT_QUEUE_CAPACITY);
        assert_eq!(cfg.max_event_bytes, DEFAULT_MAX_EVENT_BYTES);
        // SAFETY: cleanup after test.
        unsafe {
            std::env::remove_var("ATM_LOG");
            std::env::remove_var("ATM_LOG_MSG");
            std::env::remove_var("ATM_LOG_FILE");
            std::env::remove_var("ATM_LOG_MAX_BYTES");
            std::env::remove_var("ATM_LOG_MAX_FILES");
            std::env::remove_var("ATM_LOG_RETENTION_DAYS");
        }
    }

    #[test]
    fn spool_filename_format_matches_contract() {
        let name = spool_file_name("atm-daemon", 44201, 123456789);
        assert_eq!(name, "atm-daemon-44201-123456789.jsonl");
    }

    #[test]
    fn spool_filename_sanitizes_windows_unsafe_chars() {
        let name = spool_file_name(r"atm\daemon:core?*", 44201, 123456789);
        assert_eq!(name, "atm_daemon_core__-44201-123456789.jsonl");
    }

    #[test]
    fn emit_rotates_file() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: 1,
            max_files: 2,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };
        let logger = Logger::new(cfg);

        let ev1 = make_event("2026-03-09T00:00:01Z");
        logger.emit(&ev1).expect("first emit");
        let ev2 = make_event("2026-03-09T00:00:02Z");
        logger.emit(&ev2).expect("second emit");

        assert!(logger.config.log_path.exists());
        assert!(rotation_path(&logger.config.log_path, 1).exists());
    }

    #[test]
    fn emit_rejects_event_larger_than_configured_guard() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: 256,
        };
        let logger = Logger::new(cfg);

        let mut event = make_event("2026-03-09T00:00:01Z");
        event.fields.insert(
            "blob".to_string(),
            serde_json::Value::String("x".repeat(2048)),
        );
        let err = logger.emit(&event).expect_err("expected size guard error");
        assert!(matches!(err, LoggerError::EventTooLarge { .. }));
    }

    #[test]
    fn merge_spool_sorts_by_timestamp_and_deletes_claimed_files() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };
        let logger = Logger::new(cfg.clone());

        let ev_late = make_event("2026-03-09T00:00:05Z");
        let ev_early = make_event("2026-03-09T00:00:01Z");
        logger
            .write_to_spool(&ev_late, 2000)
            .expect("write late spool");
        logger
            .write_to_spool(&ev_early, 1000)
            .expect("write early spool");

        let merged = logger.merge_spool().expect("merge spool");
        assert_eq!(merged, 2);

        let lines: Vec<String> = fs::read_to_string(&cfg.log_path)
            .expect("read canonical log")
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(lines.len(), 2);
        let parsed0: LogEventV1 = serde_json::from_str(&lines[0]).expect("line 0 parse");
        let parsed1: LogEventV1 = serde_json::from_str(&lines[1]).expect("line 1 parse");
        assert_eq!(parsed0.ts, "2026-03-09T00:00:01Z");
        assert_eq!(parsed1.ts, "2026-03-09T00:00:05Z");

        let leftover: Vec<_> = fs::read_dir(&cfg.spool_dir)
            .expect("spool dir")
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            leftover.is_empty(),
            "spool files should be deleted after merge"
        );
    }

    #[test]
    fn merge_spool_recovers_stale_claiming_files() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };
        fs::create_dir_all(&cfg.spool_dir).expect("create spool dir");
        let stale_claiming = cfg.spool_dir.join("atm-44201-1000.claiming");
        let ev = make_event("2026-03-09T00:00:01Z");
        fs::write(
            &stale_claiming,
            format!("{}\n", serde_json::to_string(&ev).expect("serialize")),
        )
        .expect("write stale claiming");

        let logger = Logger::new(cfg.clone());
        let merged = logger.merge_spool().expect("merge spool");
        assert_eq!(merged, 1);
        assert!(!stale_claiming.exists());

        let log_content = fs::read_to_string(&cfg.log_path).expect("read log");
        let lines: Vec<_> = log_content.lines().collect();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn write_to_spool_creates_dir_and_appends() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };
        let logger = Logger::new(cfg);
        let ev = make_event("2026-03-09T00:00:01Z");
        let path1 = logger.write_to_spool(&ev, 1000).expect("spool write 1");
        let path2 = logger.write_to_spool(&ev, 1000).expect("spool write 2");
        assert_eq!(path1, path2);
        let spool_content = fs::read_to_string(path1).expect("read spool");
        let lines: Vec<_> = spool_content.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn rotate_log_files_max_files_zero_removes_base() {
        let tmp = TempDir::new().expect("temp dir");
        let base = tmp.path().join("atm.log.jsonl");
        fs::write(&base, "line\n").expect("write base");
        rotate_log_files(&base, 0).expect("rotate");
        assert!(!base.exists());
    }

    #[test]
    fn rotate_log_files_evicts_oldest_when_limit_reached() {
        let tmp = TempDir::new().expect("temp dir");
        let base = tmp.path().join("atm.log.jsonl");
        fs::write(&base, "base\n").expect("write base");
        fs::write(rotation_path(&base, 1), "one\n").expect("write .1");
        fs::write(rotation_path(&base, 2), "two\n").expect("write .2");

        rotate_log_files(&base, 2).expect("rotate");

        assert_eq!(
            fs::read_to_string(rotation_path(&base, 1)).expect("read .1"),
            "base\n"
        );
        assert_eq!(
            fs::read_to_string(rotation_path(&base, 2)).expect("read .2"),
            "one\n"
        );
        assert!(!rotation_path(&base, 3).exists());
    }

    #[test]
    fn socket_error_codes_match_contract() {
        assert_eq!(SOCKET_ERROR_VERSION_MISMATCH, "VERSION_MISMATCH");
        assert_eq!(SOCKET_ERROR_INVALID_PAYLOAD, "INVALID_PAYLOAD");
        assert_eq!(SOCKET_ERROR_INTERNAL_ERROR, "INTERNAL_ERROR");
    }

    #[test]
    fn emit_action_writes_schema_compatible_event() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };
        let logger = Logger::new(cfg.clone());

        logger
            .emit_action(
                "sc-compose",
                "sc_compose::cli",
                "command_end",
                Some("success"),
                serde_json::json!({"code": 0}),
            )
            .expect("emit action");

        let lines: Vec<_> = fs::read_to_string(&cfg.log_path)
            .expect("read log")
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(lines.len(), 1);
        let parsed: LogEventV1 = serde_json::from_str(&lines[0]).expect("parse event");
        assert_eq!(parsed.source_binary, "sc-compose");
        assert_eq!(parsed.action, "command_end");
        assert_eq!(parsed.outcome.as_deref(), Some("success"));
        assert_eq!(parsed.fields.get("code").and_then(|v| v.as_u64()), Some(0));
    }
}
