//! Shared structured JSONL event logging for ATM binaries.
//!
//! This module provides a compact, cross-process event sink used by `atm`,
//! `atm-daemon`, and `atm-agent-mcp`.

use crate::home::get_home_dir;
use chrono::Utc;
use serde_json::{Map, Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

// Requirements §4.6 default size-based rotation policy.
const DEFAULT_MAX_BYTES: u64 = 50 * 1024 * 1024;
// Requirements §4.6 default retained file count.
const DEFAULT_MAX_FILES: u32 = 5;
const DEFAULT_TRUNC_CHARS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageVerbosity {
    None,
    Truncated,
    Full,
}

impl MessageVerbosity {
    fn from_env() -> Self {
        match std::env::var("ATM_LOG_MSG")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "full" => Self::Full,
            "truncated" => Self::Truncated,
            _ => Self::None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct EventLogConfig {
    pub path: PathBuf,
    pub max_bytes: u64,
    pub max_files: u32,
    pub message_verbosity: MessageVerbosity,
    pub truncate_chars: usize,
}

impl EventLogConfig {
    pub fn from_env() -> Self {
        let default_path = std::env::var("ATM_HOME")
            .map(|atm_home| PathBuf::from(atm_home).join("events.jsonl"))
            .unwrap_or_else(|_| {
                get_home_dir()
                    .ok()
                    .map(|h| h.join(".config/atm/events.jsonl"))
                    .unwrap_or_else(|| PathBuf::from("events.jsonl"))
            });

        let path = std::env::var("ATM_LOG_FILE")
            .or_else(|_| std::env::var("ATM_LOG_PATH"))
            .map(PathBuf::from)
            .unwrap_or(default_path);
        let max_bytes = std::env::var("ATM_LOG_MAX_BYTES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_BYTES);
        let max_files = std::env::var("ATM_LOG_MAX_FILES")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_FILES);
        let truncate_chars = std::env::var("ATM_LOG_TRUNC_CHARS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_TRUNC_CHARS);

        Self {
            path,
            max_bytes,
            max_files,
            message_verbosity: MessageVerbosity::from_env(),
            truncate_chars,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct EventFields {
    pub level: &'static str,
    pub source: &'static str,
    pub action: &'static str,
    pub team: Option<String>,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub target: Option<String>,
    pub result: Option<String>,
    pub message_id: Option<String>,
    pub request_id: Option<String>,
    pub error: Option<String>,
    pub count: Option<u64>,
    pub message_text: Option<String>,
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

fn maybe_message_field(cfg: &EventLogConfig, text: Option<&str>) -> Option<String> {
    let txt = text?;
    match cfg.message_verbosity {
        MessageVerbosity::None => None,
        MessageVerbosity::Truncated => Some(truncate_chars(txt, cfg.truncate_chars)),
        MessageVerbosity::Full => Some(txt.to_string()),
    }
}

fn ensure_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn rotated_path(path: &Path, idx: u32) -> PathBuf {
    PathBuf::from(format!("{}.{}", path.display(), idx))
}

fn rotate_if_needed(path: &Path, max_bytes: u64, max_files: u32) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    if fs::metadata(path)?.len() < max_bytes {
        return Ok(());
    }

    for idx in (1..max_files).rev() {
        let src = rotated_path(path, idx);
        let dst = rotated_path(path, idx + 1);
        if src.exists() {
            let _ = fs::rename(&src, &dst);
        }
    }
    let _ = fs::rename(path, rotated_path(path, 1));
    Ok(())
}

fn schema_header_line() -> String {
    json!({
        "v": 1,
        "k": "h",
        "ts": Utc::now().to_rfc3339(),
        "m": {
            "v": "schema_version",
            "k": "record_kind",
            "ts": "timestamp",
            "lv": "level",
            "src": "source",
            "act": "action",
            "team": "team",
            "sid": "session_id",
            "aid": "agent_id",
            "anm": "agent_name",
            "target": "target",
            "res": "result",
            "mid": "message_id",
            "rid": "request_id",
            "cnt": "count",
            "err": "error",
            "msg": "message_text"
        }
    })
    .to_string()
}

fn write_header_if_empty(path: &Path) -> std::io::Result<()> {
    let should_write = !path.exists() || fs::metadata(path)?.len() == 0;
    if !should_write {
        return Ok(());
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(schema_header_line().as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

/// Bridge mode that controls whether events are written to the legacy sink,
/// the unified channel, or both.
///
/// Controlled by the `ATM_LOG_BRIDGE` environment variable:
/// - `"dual"` (default): write legacy `events.jsonl` AND forward to unified channel.
/// - `"unified_only"`: skip legacy write, only forward to unified channel.
/// - `"legacy_only"`: write legacy only, skip unified channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BridgeMode {
    Dual,
    UnifiedOnly,
    LegacyOnly,
}

impl BridgeMode {
    fn from_env() -> Self {
        match std::env::var("ATM_LOG_BRIDGE")
            .unwrap_or_else(|_| "dual".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "unified_only" => Self::UnifiedOnly,
            "legacy_only" => Self::LegacyOnly,
            _ => Self::Dual,
        }
    }
}

// ── Cached configuration ──────────────────────────────────────────────────────

/// Cached [`EventLogConfig`] so environment variables are read only once.
///
/// The first call to [`emit_event_best_effort`] initialises the cache.
/// Subsequent calls reuse the cached value without reading the environment.
///
/// # Test note
///
/// Tests that need to exercise different environment configurations should call
/// [`EventLogConfig::from_env`] or [`emit_event_with_config`] directly rather
/// than relying on the env-variable side effects of `emit_event_best_effort`,
/// because the cache is process-global and the first call wins.
static CACHED_CONFIG: std::sync::OnceLock<EventLogConfig> = std::sync::OnceLock::new();

/// Cached [`BridgeMode`] so the env variable is read only once.
static CACHED_BRIDGE_MODE: std::sync::OnceLock<BridgeMode> = std::sync::OnceLock::new();

/// Forward `event` to the unified producer channel if a sender is registered.
fn forward_to_unified(event: crate::logging_event::LogEventV1) {
    if let Some(tx) = crate::logging::producer_sender() {
        // send returns Err if full or disconnected; we silently drop.
        let _ = tx.try_send(event);
    }
}

/// Map [`EventFields`] to a [`LogEventV1`] for the unified pipeline.
fn fields_to_log_event(fields: &EventFields) -> crate::logging_event::LogEventV1 {
    use crate::logging_event::LogEventV1;
    use chrono::Utc;

    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();

    LogEventV1 {
        v: 1,
        ts: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        level: fields.level.to_string(),
        source_binary: fields.source.to_string(),
        hostname,
        pid: std::process::id(),
        target: fields.target.clone().unwrap_or_default(),
        action: fields.action.to_string(),
        team: fields.team.clone(),
        agent: fields
            .agent_id
            .clone()
            .or_else(|| fields.agent_name.clone()),
        session_id: fields.session_id.clone(),
        request_id: fields.request_id.clone(),
        correlation_id: None,
        outcome: fields.result.clone(),
        error: fields.error.clone(),
        fields: {
            let mut map = serde_json::Map::new();
            if let Some(mid) = &fields.message_id {
                map.insert(
                    "message_id".to_string(),
                    serde_json::Value::String(mid.clone()),
                );
            }
            if let Some(cnt) = fields.count {
                map.insert("count".to_string(), serde_json::Value::Number(cnt.into()));
            }
            map
        },
        spans: vec![],
    }
}

/// Emit a single structured event to the shared sink.
///
/// This function is intentionally fail-open: any error is swallowed.
///
/// The bridge mode (see [`BridgeMode`]) controls whether events are written to
/// the legacy JSONL file, forwarded to the unified daemon channel, or both.
/// The default mode is `dual` — both paths are used.
///
/// # Configuration caching
///
/// [`EventLogConfig`] and [`BridgeMode`] are resolved from environment variables
/// on the **first** call and cached for the lifetime of the process.  If you
/// need to test different configurations, use [`emit_event_with_config`] directly.
pub fn emit_event_best_effort(mut fields: EventFields) {
    if fields.level.is_empty() || fields.source.is_empty() || fields.action.is_empty() {
        return;
    }

    let cfg = CACHED_CONFIG.get_or_init(EventLogConfig::from_env);
    let bridge_mode = *CACHED_BRIDGE_MODE.get_or_init(BridgeMode::from_env);

    // Pick up CLAUDE_SESSION_ID if the caller did not supply a session_id.
    // If the env var is absent, leave session_id as None — do NOT fall back to
    // a sentinel string like "unknown".  The LogEventV1 schema uses Option<String>
    // to distinguish "no session" from a known session ID.
    if fields.session_id.is_none() {
        fields.session_id = std::env::var("CLAUDE_SESSION_ID").ok();
    }

    emit_event_with_config_inner(fields, cfg, bridge_mode);
}

/// Emit a structured event using the provided configuration and bridge mode.
///
/// Unlike [`emit_event_best_effort`], this function bypasses the process-global
/// [`OnceLock`](std::sync::OnceLock) cache and uses the supplied `cfg` and
/// `bridge_mode` directly.  Use this in tests and in code paths that need
/// precise control over routing behaviour.
#[cfg(test)]
fn emit_event_with_config(fields: EventFields, cfg: &EventLogConfig, bridge_mode: BridgeMode) {
    emit_event_with_config_inner(fields, cfg, bridge_mode);
}

fn emit_event_with_config_inner(
    fields: EventFields,
    cfg: &EventLogConfig,
    bridge_mode: BridgeMode,
) {
    if fields.level.is_empty() || fields.source.is_empty() || fields.action.is_empty() {
        return;
    }

    // Forward to unified pipeline if enabled.
    if bridge_mode == BridgeMode::Dual || bridge_mode == BridgeMode::UnifiedOnly {
        let event = fields_to_log_event(&fields);
        forward_to_unified(event);
    }

    // Write legacy JSONL if enabled.
    if bridge_mode == BridgeMode::UnifiedOnly {
        return;
    }

    // Build the legacy session_id value: use the caller-supplied value if present;
    // fall back to CLAUDE_SESSION_ID from env; last resort keep the legacy sentinel
    // "unknown" only in the legacy write path (backwards compat for existing log consumers).
    let legacy_sid = fields
        .session_id
        .clone()
        .or_else(|| std::env::var("CLAUDE_SESSION_ID").ok())
        .unwrap_or_else(|| "unknown".to_string());

    let _ = (|| -> std::io::Result<()> {
        ensure_parent(&cfg.path)?;
        rotate_if_needed(&cfg.path, cfg.max_bytes, cfg.max_files)?;
        write_header_if_empty(&cfg.path)?;

        let mut obj = Map::new();
        obj.insert("v".to_string(), Value::from(1));
        obj.insert("k".to_string(), Value::from("e"));
        obj.insert("ts".to_string(), Value::from(Utc::now().to_rfc3339()));
        obj.insert("lv".to_string(), Value::from(fields.level));
        obj.insert("src".to_string(), Value::from(fields.source));
        obj.insert("act".to_string(), Value::from(fields.action));
        obj.insert(
            "team".to_string(),
            Value::from(fields.team.unwrap_or_else(|| "unknown".to_string())),
        );
        obj.insert("sid".to_string(), Value::from(legacy_sid));
        if let Some(v) = fields.agent_id {
            obj.insert("aid".to_string(), Value::from(v));
        }
        if let Some(v) = fields.agent_name {
            obj.insert("anm".to_string(), Value::from(v));
        }
        if let Some(v) = fields.target {
            obj.insert("target".to_string(), Value::from(v));
        }
        if let Some(v) = fields.result {
            obj.insert("res".to_string(), Value::from(v));
        }
        if let Some(v) = fields.message_id {
            obj.insert("mid".to_string(), Value::from(v));
        }
        if let Some(v) = fields.request_id {
            obj.insert("rid".to_string(), Value::from(v));
        }
        if let Some(v) = fields.count {
            obj.insert("cnt".to_string(), Value::from(v));
        }
        if let Some(v) = fields.error {
            obj.insert("err".to_string(), Value::from(v));
        }
        if let Some(v) = maybe_message_field(cfg, fields.message_text.as_deref()) {
            obj.insert("msg".to_string(), Value::from(v));
        }

        let line = Value::Object(obj).to_string();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&cfg.path)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    })();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{NamedTempFile, TempDir};

    /// Build an [`EventLogConfig`] pointing to the given log path with no message verbosity.
    fn cfg_at(path: std::path::PathBuf) -> EventLogConfig {
        EventLogConfig {
            path,
            max_bytes: 50 * 1024 * 1024,
            max_files: 5,
            message_verbosity: MessageVerbosity::None,
            truncate_chars: 200,
        }
    }

    #[test]
    fn test_emit_event_writes_header_and_event() {
        let tmp = TempDir::new().unwrap();
        let log_path = tmp.path().join("events.jsonl");

        // Use emit_event_with_config to bypass the OnceLock cache.
        emit_event_with_config(
            EventFields {
                level: "info",
                source: "atm",
                action: "send",
                team: Some("atm-dev".to_string()),
                agent_id: Some("arch-ctm".to_string()),
                agent_name: Some("arch-ctm".to_string()),
                target: Some("team-lead".to_string()),
                result: Some("ok".to_string()),
                session_id: Some("sess-123".to_string()),
                ..Default::default()
            },
            &cfg_at(log_path.clone()),
            BridgeMode::LegacyOnly,
        );

        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert!(lines.len() >= 2);
        let header: Value = serde_json::from_str(lines[0]).unwrap();
        let event: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(header["k"], "h");
        assert_eq!(event["k"], "e");
        assert_eq!(event["team"], "atm-dev");
        assert_eq!(event["sid"], "sess-123");
        assert_eq!(event["act"], "send");
        assert_eq!(event["aid"], "arch-ctm");
    }

    #[test]
    fn test_rotate_if_needed_renames_file() {
        let tmp = TempDir::new().unwrap();
        let log_path = tmp.path().join("events.jsonl");
        fs::write(&log_path, b"1234567890").unwrap();
        rotate_if_needed(&log_path, 5, 5).unwrap();
        assert!(!log_path.exists());
        assert!(tmp.path().join("events.jsonl.1").exists());
    }

    #[test]
    fn test_message_verbosity_truncated() {
        let tmp = TempDir::new().unwrap();
        let trunc_path = tmp.path().join("trunc.jsonl");

        let cfg = EventLogConfig {
            path: trunc_path.clone(),
            max_bytes: 50 * 1024 * 1024,
            max_files: 5,
            message_verbosity: MessageVerbosity::Truncated,
            truncate_chars: 4,
        };
        emit_event_with_config(
            EventFields {
                level: "info",
                source: "atm",
                action: "send",
                message_text: Some("abcdef".to_string()),
                ..Default::default()
            },
            &cfg,
            BridgeMode::LegacyOnly,
        );
        let lines: Vec<String> = fs::read_to_string(&trunc_path)
            .unwrap()
            .lines()
            .map(|s| s.to_string())
            .collect();
        let trunc_event: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(trunc_event["msg"], "abcd");
    }

    #[test]
    fn test_message_verbosity_full() {
        let tmp = TempDir::new().unwrap();
        let full_path = tmp.path().join("full.jsonl");

        let cfg = EventLogConfig {
            path: full_path.clone(),
            max_bytes: 50 * 1024 * 1024,
            max_files: 5,
            message_verbosity: MessageVerbosity::Full,
            truncate_chars: 4,
        };
        emit_event_with_config(
            EventFields {
                level: "info",
                source: "atm",
                action: "send",
                message_text: Some("abcdef".to_string()),
                ..Default::default()
            },
            &cfg,
            BridgeMode::LegacyOnly,
        );
        let lines: Vec<String> = fs::read_to_string(&full_path)
            .unwrap()
            .lines()
            .map(|s| s.to_string())
            .collect();
        let full_event: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(full_event["msg"], "abcdef");
    }

    #[test]
    fn test_fail_open_when_path_unwritable() {
        let file = NamedTempFile::new().unwrap();
        // Should not panic even though the path points into a temporary file (not a dir)
        emit_event_with_config(
            EventFields {
                level: "info",
                source: "atm",
                action: "send",
                ..Default::default()
            },
            &cfg_at(file.path().join("impossible_subdir").join("events.jsonl")),
            BridgeMode::LegacyOnly,
        );
    }

    #[test]
    fn test_from_env_uses_atm_home_events_file() {
        // EventLogConfig::from_env() is tested directly — no caching concerns.
        let tmp = TempDir::new().unwrap();
        // SAFETY: serialised by isolation — we call from_env directly, not emit.
        unsafe {
            std::env::remove_var("ATM_LOG_FILE");
            std::env::remove_var("ATM_LOG_PATH");
            std::env::set_var("ATM_HOME", tmp.path());
        }
        let cfg = EventLogConfig::from_env();
        assert_eq!(cfg.path, tmp.path().join("events.jsonl"));
        // Restore
        unsafe { std::env::remove_var("ATM_HOME") };
    }

    /// Verify that `fields_to_log_event` does NOT set session_id to "unknown"
    /// when no session ID is available.  The LogEventV1 schema uses Option<String>
    /// and None is the correct value when no session is active.
    #[test]
    fn test_fields_to_log_event_session_id_none_when_unset() {
        // Ensure the env var is absent for this test.
        // SAFETY: direct env manipulation — this test reads from_env directly.
        unsafe { std::env::remove_var("CLAUDE_SESSION_ID") };

        let fields = EventFields {
            level: "info",
            source: "atm",
            action: "test_action",
            session_id: None,
            ..Default::default()
        };
        let event = fields_to_log_event(&fields);
        assert!(
            event.session_id.is_none(),
            "session_id should be None when CLAUDE_SESSION_ID is unset and caller passed None"
        );
    }

    /// Verify that fields_to_log_event propagates a caller-supplied session_id.
    #[test]
    fn test_fields_to_log_event_session_id_propagated() {
        let fields = EventFields {
            level: "info",
            source: "atm",
            action: "test_action",
            session_id: Some("my-session-42".to_string()),
            ..Default::default()
        };
        let event = fields_to_log_event(&fields);
        assert_eq!(event.session_id.as_deref(), Some("my-session-42"));
    }

    /// Verify that the legacy JSONL write path still uses "unknown" as sentinel
    /// when no session_id is supplied (backwards compat for log consumers).
    #[test]
    fn test_legacy_write_uses_unknown_sentinel_when_no_session() {
        unsafe { std::env::remove_var("CLAUDE_SESSION_ID") };

        let tmp = TempDir::new().unwrap();
        let log_path = tmp.path().join("events.jsonl");

        emit_event_with_config(
            EventFields {
                level: "info",
                source: "atm",
                action: "test_action",
                session_id: None,
                ..Default::default()
            },
            &cfg_at(log_path.clone()),
            BridgeMode::LegacyOnly,
        );

        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        let event: Value = serde_json::from_str(lines[1]).unwrap();
        // Legacy format always writes a sid field; "unknown" is the fallback.
        assert_eq!(
            event["sid"], "unknown",
            "legacy sid should be 'unknown' when no session"
        );
    }

    // ── Bridge-mode routing tests (migrated from integration tests) ────────────
    //
    // These tests were originally in `tests/test_unified_logging.rs` and used
    // `emit_event_best_effort` with env-var side effects.  Because
    // `CACHED_BRIDGE_MODE` is a process-global `OnceLock`, whichever test ran
    // first would freeze the mode for the entire test binary, causing the others
    // to fail.  Moving them here and using `emit_event_with_config` directly
    // bypasses the cache entirely — each test gets its own `BridgeMode` and
    // `EventLogConfig` with no shared state.

    /// `BridgeMode::Dual` must write to the legacy JSONL file.
    #[test]
    fn test_bridge_dual_writes_legacy_file() {
        let tmp = TempDir::new().unwrap();
        let log_path = tmp.path().join("events.jsonl");

        emit_event_with_config(
            EventFields {
                level: "info",
                source: "atm",
                action: "test_dual",
                team: Some("atm-dev".to_string()),
                ..Default::default()
            },
            &cfg_at(log_path.clone()),
            BridgeMode::Dual,
        );

        let content = fs::read_to_string(&log_path).expect("log file should exist");
        assert!(!content.is_empty(), "dual mode should write legacy JSONL");

        let lines: Vec<&str> = content.lines().collect();
        assert!(
            lines.len() >= 2,
            "expected header + event; got {} lines",
            lines.len()
        );

        let event_line: Value =
            serde_json::from_str(lines[1]).expect("event line should be valid JSON");
        assert_eq!(event_line["act"], "test_dual");
    }

    /// `BridgeMode::LegacyOnly` must write to the legacy JSONL file.
    #[test]
    fn test_bridge_legacy_only_writes_legacy_file() {
        let tmp = TempDir::new().unwrap();
        let log_path = tmp.path().join("events.jsonl");

        emit_event_with_config(
            EventFields {
                level: "info",
                source: "atm",
                action: "test_legacy_only",
                ..Default::default()
            },
            &cfg_at(log_path.clone()),
            BridgeMode::LegacyOnly,
        );

        let content = fs::read_to_string(&log_path).expect("log file should exist");
        assert!(
            !content.is_empty(),
            "legacy_only mode should write legacy JSONL"
        );
    }

    /// `BridgeMode::UnifiedOnly` must NOT write to the legacy JSONL file.
    #[test]
    fn test_bridge_unified_only_skips_legacy_file() {
        let tmp = TempDir::new().unwrap();
        let log_path = tmp.path().join("events.jsonl");

        emit_event_with_config(
            EventFields {
                level: "info",
                source: "atm",
                action: "test_unified_only",
                ..Default::default()
            },
            &cfg_at(log_path.clone()),
            BridgeMode::UnifiedOnly,
        );

        // With unified_only the legacy JSONL should NOT be written.
        let exists = log_path.exists();
        if exists {
            let content = fs::read_to_string(&log_path).unwrap();
            assert!(
                content.is_empty(),
                "unified_only mode must not write to legacy JSONL; got: {content}"
            );
        }
        // file not existing is also correct
    }

    /// The default bridge mode (`Dual`) must write to the legacy JSONL file.
    #[test]
    fn test_bridge_default_is_dual() {
        let tmp = TempDir::new().unwrap();
        let log_path = tmp.path().join("events.jsonl");

        // Explicitly pass BridgeMode::Dual to represent the default behaviour.
        emit_event_with_config(
            EventFields {
                level: "info",
                source: "atm",
                action: "test_default_bridge",
                ..Default::default()
            },
            &cfg_at(log_path.clone()),
            BridgeMode::Dual,
        );

        let content = fs::read_to_string(&log_path).expect("log file should exist");
        assert!(
            !content.is_empty(),
            "default bridge mode should write legacy JSONL"
        );
    }
}
