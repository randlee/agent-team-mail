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

/// Emit a single structured event to the shared sink.
///
/// This function is intentionally fail-open: any error is swallowed.
pub fn emit_event_best_effort(mut fields: EventFields) {
    if fields.level.is_empty() || fields.source.is_empty() || fields.action.is_empty() {
        return;
    }

    let cfg = EventLogConfig::from_env();

    if fields.session_id.is_none() {
        fields.session_id = std::env::var("CLAUDE_SESSION_ID").ok();
    }
    if fields.session_id.is_none() {
        fields.session_id = Some("unknown".to_string());
    }

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
        if let Some(v) = fields.session_id {
            obj.insert("sid".to_string(), Value::from(v));
        }
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
        if let Some(v) = maybe_message_field(&cfg, fields.message_text.as_deref()) {
            obj.insert("msg".to_string(), Value::from(v));
        }

        let line = Value::Object(obj).to_string();
        let mut file = OpenOptions::new().create(true).append(true).open(&cfg.path)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    })();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::{NamedTempFile, TempDir};

    #[test]
    #[serial]
    fn test_emit_event_writes_header_and_event() {
        let tmp = TempDir::new().unwrap();
        let log_path = tmp.path().join("events.jsonl");
        unsafe {
            std::env::set_var("ATM_LOG_FILE", &log_path);
            std::env::set_var("ATM_LOG_MSG", "none");
            std::env::set_var("CLAUDE_SESSION_ID", "sess-123");
        }

        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm",
            action: "send",
            team: Some("atm-dev".to_string()),
            agent_id: Some("arch-ctm".to_string()),
            agent_name: Some("arch-ctm".to_string()),
            target: Some("team-lead".to_string()),
            result: Some("ok".to_string()),
            ..Default::default()
        });

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
    #[serial]
    fn test_rotate_if_needed_renames_file() {
        let tmp = TempDir::new().unwrap();
        let log_path = tmp.path().join("events.jsonl");
        fs::write(&log_path, b"1234567890").unwrap();
        rotate_if_needed(&log_path, 5, 5).unwrap();
        assert!(!log_path.exists());
        assert!(tmp.path().join("events.jsonl.1").exists());
    }

    #[test]
    #[serial]
    fn test_message_verbosity_truncated_and_full() {
        let tmp = TempDir::new().unwrap();
        let trunc_path = tmp.path().join("trunc.jsonl");
        unsafe {
            std::env::set_var("ATM_LOG_FILE", &trunc_path);
            std::env::set_var("ATM_LOG_MSG", "truncated");
            std::env::set_var("ATM_LOG_TRUNC_CHARS", "4");
        }
        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm",
            action: "send",
            message_text: Some("abcdef".to_string()),
            ..Default::default()
        });
        let lines: Vec<String> = fs::read_to_string(&trunc_path)
            .unwrap()
            .lines()
            .map(|s| s.to_string())
            .collect();
        let trunc_event: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(trunc_event["msg"], "abcd");

        let full_path = tmp.path().join("full.jsonl");
        unsafe {
            std::env::set_var("ATM_LOG_FILE", &full_path);
            std::env::set_var("ATM_LOG_MSG", "full");
        }
        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm",
            action: "send",
            message_text: Some("abcdef".to_string()),
            ..Default::default()
        });
        let lines: Vec<String> = fs::read_to_string(&full_path)
            .unwrap()
            .lines()
            .map(|s| s.to_string())
            .collect();
        let full_event: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(full_event["msg"], "abcdef");
    }

    #[test]
    #[serial]
    fn test_fail_open_when_path_unwritable() {
        let file = NamedTempFile::new().unwrap();
        unsafe {
            std::env::set_var("ATM_LOG_FILE", file.path());
        }
        // Should not panic even though parent path cannot be created under file
        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm",
            action: "send",
            ..Default::default()
        });
    }

    #[test]
    #[serial]
    fn test_from_env_uses_atm_home_events_file() {
        let tmp = TempDir::new().unwrap();
        unsafe {
            std::env::remove_var("ATM_LOG_FILE");
            std::env::remove_var("ATM_LOG_PATH");
            std::env::set_var("ATM_HOME", tmp.path());
        }
        let cfg = EventLogConfig::from_env();
        assert_eq!(cfg.path, tmp.path().join("events.jsonl"));
    }
}
