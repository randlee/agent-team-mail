//! Structured log event types for the unified daemon-fan-in logging architecture.
//!
//! This module defines [`LogEventV1`] — the canonical wire format for all
//! structured log events emitted by any `atm` binary. All binaries send events
//! to the `atm-daemon` via the `"log-event"` socket command; the daemon is the
//! sole JSONL writer.
//!
//! # Architecture
//!
//! ```text
//! atm / atm-tui / atm-agent-mcp
//!   └── write_to_spool()     (best-effort when daemon unavailable)
//!   └── daemon socket "log-event" command
//!         └── atm-daemon receives → validates → redacts → enqueues
//!               └── log_writer task drains → JSONL file(s)
//! ```
//!
//! # Schema versioning
//!
//! [`LogEventV1::v`] is always `1`. Future breaking changes bump to `LogEventV2`,
//! keeping this module stable for as long as the daemon supports it.
//!
//! # Spool
//!
//! When the daemon is unavailable, callers may use [`write_to_spool`] as a
//! best-effort fallback. Spool files are written to
//! `{home_dir}/.config/atm/log-spool/{source_binary}-{pid}-{millis}.jsonl`.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

// ── Validation error ──────────────────────────────────────────────────────────

/// Errors returned by [`LogEventV1::validate`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    /// A required field is empty or absent.
    #[error("required field '{field}' is empty")]
    RequiredFieldEmpty { field: &'static str },

    /// The `level` field contains an unrecognized value.
    #[error("invalid level '{value}'; expected one of: trace, debug, info, warn, error")]
    InvalidLevel { value: String },

    /// The schema version field `v` is not `1`.
    #[error("unsupported schema version {version}; expected 1")]
    UnsupportedVersion { version: u8 },

    /// The serialized event exceeds the 64 KiB size limit.
    #[error("event exceeds maximum serialized size of 65536 bytes ({size} bytes)")]
    EventTooLarge { size: usize },
}

/// Maximum allowed serialized size of a [`LogEventV1`] in bytes (64 KiB).
pub const MAX_EVENT_BYTES: usize = 64 * 1024;

// ── SpanRefV1 ─────────────────────────────────────────────────────────────────

/// A tracing span reference captured at the time a [`LogEventV1`] is emitted.
///
/// Spans are snapshots of the active tracing span chain; they are not live
/// handles. The `name` field contains the span name and `fields` contains the
/// span's recorded key-value pairs at the time of capture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpanRefV1 {
    /// Span name (e.g. `"daemon_dispatch"`).
    pub name: String,
    /// Span fields recorded at capture time.
    #[serde(default)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

// ── LogEventV1 ───────────────────────────────────────────────────────────────

/// Canonical structured log event for the unified atm logging pipeline.
///
/// All `atm` binaries emit this type via the daemon's `"log-event"` socket
/// command. The daemon validates, redacts, and writes events to a JSONL file.
///
/// # Schema version
///
/// The `v` field is always `1`. A future breaking change will introduce
/// `LogEventV2` rather than modifying this struct.
///
/// # Serde
///
/// All `Option` fields are omitted from JSON when `None`
/// (`#[serde(skip_serializing_if = "Option::is_none")]`).
/// `fields` and `spans` default to empty when absent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogEventV1 {
    /// Schema version — always `1`.
    pub v: u8,

    /// RFC 3339 UTC timestamp (e.g. `"2026-02-22T10:30:00Z"`).
    pub ts: String,

    /// Log level: `trace`, `debug`, `info`, `warn`, or `error`.
    pub level: String,

    /// Name of the binary that emitted this event.
    ///
    /// One of: `atm`, `atm-daemon`, `atm-tui`, `atm-agent-mcp`.
    pub source_binary: String,

    /// Hostname of the machine where the event was emitted.
    pub hostname: String,

    /// Process ID of the emitting binary.
    pub pid: u32,

    /// Tracing target — typically the Rust module path (e.g. `"atm_daemon::daemon::socket"`).
    pub target: String,

    /// Stable, machine-readable event name (e.g. `"daemon_start"`, `"send_message"`).
    ///
    /// Consumers should treat this as an opaque identifier; do not parse its structure.
    pub action: String,

    /// ATM team name this event is associated with, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,

    /// Agent identity that emitted this event, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,

    /// Claude session ID, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Per-request correlation ID, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,

    /// Cross-request correlation ID for tracing a logical operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,

    /// Outcome of the action: `ok`, `err`, `timeout`, `dropped`, etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,

    /// Human-readable error description when `outcome` is `err` or similar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Arbitrary additional key-value pairs for this event.
    #[serde(default)]
    pub fields: serde_json::Map<String, serde_json::Value>,

    /// Active tracing span chain at the time the event was emitted.
    #[serde(default)]
    pub spans: Vec<SpanRefV1>,
}

impl LogEventV1 {
    /// Create a [`LogEventV1Builder`] for constructing a new event.
    ///
    /// `source_binary`, `action`, and `target` are required; the builder
    /// auto-fills `v`, `ts`, `hostname`, and `pid`.
    ///
    /// # Examples
    ///
    /// ```
    /// use agent_team_mail_core::logging_event::LogEventV1;
    ///
    /// let event = LogEventV1::builder("atm", "send_message", "atm::send")
    ///     .level("info")
    ///     .build();
    ///
    /// assert_eq!(event.v, 1);
    /// assert_eq!(event.source_binary, "atm");
    /// assert_eq!(event.action, "send_message");
    /// ```
    pub fn builder(
        source_binary: impl Into<String>,
        action: impl Into<String>,
        target: impl Into<String>,
    ) -> LogEventV1Builder {
        LogEventV1Builder::new(source_binary.into(), action.into(), target.into())
    }

    /// Validate the event, returning a [`ValidationError`] if any constraint is violated.
    ///
    /// # Checked constraints
    ///
    /// - `v` must be `1`
    /// - `ts`, `level`, `source_binary`, `hostname`, `target`, `action` must be non-empty
    /// - `level` must be one of: `trace`, `debug`, `info`, `warn`, `error` (case-insensitive)
    /// - The serialized JSON representation must not exceed [`MAX_EVENT_BYTES`] (64 KiB)
    ///
    /// # Errors
    ///
    /// Returns the first [`ValidationError`] encountered.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.v != 1 {
            return Err(ValidationError::UnsupportedVersion { version: self.v });
        }

        let required = [
            ("ts", self.ts.as_str()),
            ("level", self.level.as_str()),
            ("source_binary", self.source_binary.as_str()),
            ("hostname", self.hostname.as_str()),
            ("target", self.target.as_str()),
            ("action", self.action.as_str()),
        ];
        for (field, value) in required {
            if value.is_empty() {
                return Err(ValidationError::RequiredFieldEmpty { field });
            }
        }

        let level_lower = self.level.to_lowercase();
        if !matches!(
            level_lower.as_str(),
            "trace" | "debug" | "info" | "warn" | "error"
        ) {
            return Err(ValidationError::InvalidLevel {
                value: self.level.clone(),
            });
        }

        // Size guard: serialize and check byte count.
        let serialized = serde_json::to_string(self)
            .unwrap_or_default();
        let size = serialized.len();
        if size > MAX_EVENT_BYTES {
            return Err(ValidationError::EventTooLarge { size });
        }

        Ok(())
    }

    /// Redact sensitive values in `fields` and all `spans[*].fields`.
    ///
    /// Denylist keys (case-insensitive): `password`, `secret`, `token`, `api_key`, `auth`.
    /// String values matching `^[Bb]earer\s+\S+` are also redacted regardless of key.
    ///
    /// Matching values are replaced with the string `"[REDACTED]"`.
    pub fn redact(&mut self) {
        redact_map(&mut self.fields);
        for span in &mut self.spans {
            redact_map(&mut span.fields);
        }
    }
}

// ── Redaction helpers ─────────────────────────────────────────────────────────

/// Denylist key names (matched case-insensitively).
const DENYLIST_KEYS: &[&str] = &["password", "secret", "token", "api_key", "auth"];

fn is_denylist_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    DENYLIST_KEYS.iter().any(|&k| lower == k)
}

fn is_bearer_token(value: &str) -> bool {
    // Matches "Bearer <token>" or "bearer <token>" at the start of the string.
    if let Some(rest) = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")) {
        !rest.trim().is_empty()
    } else {
        false
    }
}

fn redact_map(map: &mut serde_json::Map<String, serde_json::Value>) {
    for (key, value) in map.iter_mut() {
        let should_redact = is_denylist_key(key)
            || value
                .as_str()
                .map(is_bearer_token)
                .unwrap_or(false);
        if should_redact {
            *value = serde_json::Value::String("[REDACTED]".to_string());
        }
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Builder for [`LogEventV1`].
///
/// Obtain via [`LogEventV1::builder`]. Required fields (`source_binary`,
/// `action`, `target`) are set at construction time; all other fields have
/// sensible defaults or are `None`.
///
/// The builder auto-fills:
/// - `v = 1`
/// - `ts` = current UTC time in RFC 3339 format
/// - `hostname` = result of [`hostname::get()`] (empty string on error)
/// - `pid` = [`std::process::id()`]
/// - `level` = `"info"` (override with [`LogEventV1Builder::level`])
pub struct LogEventV1Builder {
    source_binary: String,
    action: String,
    target: String,
    level: String,
    team: Option<String>,
    agent: Option<String>,
    session_id: Option<String>,
    request_id: Option<String>,
    correlation_id: Option<String>,
    outcome: Option<String>,
    error: Option<String>,
    fields: serde_json::Map<String, serde_json::Value>,
    spans: Vec<SpanRefV1>,
}

impl LogEventV1Builder {
    fn new(source_binary: String, action: String, target: String) -> Self {
        Self {
            source_binary,
            action,
            target,
            level: "info".to_string(),
            team: None,
            agent: None,
            session_id: None,
            request_id: None,
            correlation_id: None,
            outcome: None,
            error: None,
            fields: serde_json::Map::new(),
            spans: Vec::new(),
        }
    }

    /// Set the log level (default: `"info"`).
    pub fn level(mut self, level: impl Into<String>) -> Self {
        self.level = level.into();
        self
    }

    /// Set the team name.
    pub fn team(mut self, team: impl Into<String>) -> Self {
        self.team = Some(team.into());
        self
    }

    /// Set the agent identity.
    pub fn agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Set the Claude session ID.
    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Set the per-request ID.
    pub fn request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Set the correlation ID.
    pub fn correlation_id(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }

    /// Set the outcome string (e.g., `"ok"`, `"err"`).
    pub fn outcome(mut self, outcome: impl Into<String>) -> Self {
        self.outcome = Some(outcome.into());
        self
    }

    /// Set the error description.
    pub fn error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    /// Insert a field into `fields`.
    pub fn field(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.fields.insert(key.into(), value);
        self
    }

    /// Append a span reference.
    pub fn span(mut self, span: SpanRefV1) -> Self {
        self.spans.push(span);
        self
    }

    /// Build the [`LogEventV1`].
    ///
    /// Auto-fills `v`, `ts`, `hostname`, and `pid` from the current process state.
    /// The returned event is not automatically validated; call [`LogEventV1::validate`]
    /// if you need to enforce invariants.
    pub fn build(self) -> LogEventV1 {
        let ts = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_default();
        let pid = std::process::id();

        LogEventV1 {
            v: 1,
            ts,
            level: self.level,
            source_binary: self.source_binary,
            hostname,
            pid,
            target: self.target,
            action: self.action,
            team: self.team,
            agent: self.agent,
            session_id: self.session_id,
            request_id: self.request_id,
            correlation_id: self.correlation_id,
            outcome: self.outcome,
            error: self.error,
            fields: self.fields,
            spans: self.spans,
        }
    }
}

// ── Convenience constructor ───────────────────────────────────────────────────

/// Create a [`LogEventV1`] using the builder pattern.
///
/// Equivalent to `LogEventV1::builder(source_binary, action, target).level(level).build()`.
///
/// # Examples
///
/// ```
/// use agent_team_mail_core::logging_event::new_log_event;
///
/// let event = new_log_event("atm", "send_message", "atm::send", "info");
/// assert_eq!(event.level, "info");
/// assert_eq!(event.source_binary, "atm");
/// ```
pub fn new_log_event(
    source_binary: &str,
    action: &str,
    target: &str,
    level: &str,
) -> LogEventV1 {
    LogEventV1::builder(source_binary, action, target)
        .level(level)
        .build()
}

// ── Fallback spool ────────────────────────────────────────────────────────────

/// Return the spool directory path: `{home_dir}/.config/atm/log-spool`.
pub fn spool_dir(home_dir: &Path) -> PathBuf {
    home_dir.join(".config/atm/log-spool")
}

/// Return the default spool directory by resolving the home directory via
/// [`crate::home::get_home_dir`].
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
pub fn default_spool_dir() -> anyhow::Result<PathBuf> {
    let home = crate::home::get_home_dir()?;
    Ok(spool_dir(&home))
}

/// Write `event` to an explicit spool `dir` (does not resolve home directory).
///
/// Spool files are written to:
/// `{dir}/{source_binary}-{pid}-{unix_millis}.jsonl`
///
/// Any error during directory creation or file writing is silently ignored
/// (fail-open). This function is intentionally infallible.
pub fn write_to_spool_dir(event: &LogEventV1, dir: &Path) {
    use std::fs::{OpenOptions, create_dir_all};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    let _ = create_dir_all(dir);

    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let filename = format!("{}-{}-{}.jsonl", event.source_binary, event.pid, millis);
    let path = dir.join(filename);

    let Ok(line) = serde_json::to_string(event) else {
        return;
    };

    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };

    let _ = writeln!(file, "{line}");
}

/// Write `event` to the fallback spool directory as a best-effort operation.
///
/// Spool files are written to:
/// `{home_dir}/.config/atm/log-spool/{source_binary}-{pid}-{unix_millis}.jsonl`
///
/// Any error during directory creation or file writing is silently ignored
/// (fail-open). This function is intentionally infallible.
///
/// # Examples
///
/// ```no_run
/// use agent_team_mail_core::logging_event::{new_log_event, write_to_spool};
/// use std::path::PathBuf;
///
/// let home_dir = PathBuf::from(std::env::var("ATM_HOME").unwrap_or_default());
/// let event = new_log_event("atm", "send_message", "atm::send", "info");
/// write_to_spool(&event, &home_dir);
/// ```
pub fn write_to_spool(event: &LogEventV1, home_dir: &Path) {
    use std::fs::{OpenOptions, create_dir_all};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    let dir = spool_dir(home_dir);
    let _ = create_dir_all(&dir);

    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let filename = format!("{}-{}-{}.jsonl", event.source_binary, event.pid, millis);
    let path = dir.join(filename);

    let Ok(line) = serde_json::to_string(event) else {
        return;
    };

    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };

    let _ = writeln!(file, "{line}");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_valid_event() -> LogEventV1 {
        new_log_event("atm", "test_action", "atm::test_module", "info")
    }

    #[test]
    fn test_serialization_round_trip() {
        let event = LogEventV1::builder("atm-daemon", "daemon_start", "atm_daemon::main")
            .level("info")
            .team("atm-dev")
            .agent("team-lead")
            .session_id("sess-abc-123")
            .outcome("ok")
            .field("iteration", serde_json::Value::Number(42.into()))
            .span(SpanRefV1 {
                name: "daemon_dispatch".to_string(),
                fields: {
                    let mut m = serde_json::Map::new();
                    m.insert("team".to_string(), serde_json::Value::String("atm-dev".to_string()));
                    m
                },
            })
            .build();

        let json = serde_json::to_string(&event).expect("serialize");
        let deserialized: LogEventV1 = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.v, 1);
        assert_eq!(deserialized.source_binary, "atm-daemon");
        assert_eq!(deserialized.action, "daemon_start");
        assert_eq!(deserialized.target, "atm_daemon::main");
        assert_eq!(deserialized.level, "info");
        assert_eq!(deserialized.team.as_deref(), Some("atm-dev"));
        assert_eq!(deserialized.agent.as_deref(), Some("team-lead"));
        assert_eq!(deserialized.session_id.as_deref(), Some("sess-abc-123"));
        assert_eq!(deserialized.outcome.as_deref(), Some("ok"));
        assert_eq!(deserialized.spans.len(), 1);
        assert_eq!(deserialized.spans[0].name, "daemon_dispatch");
        assert_eq!(deserialized.fields.get("iteration"), event.fields.get("iteration"));
    }

    #[test]
    fn test_validation_missing_required() {
        let mut event = make_valid_event();
        event.action = String::new();
        let result = event.validate();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::RequiredFieldEmpty { field: "action" }
        ));
    }

    #[test]
    fn test_validation_missing_ts() {
        let mut event = make_valid_event();
        event.ts = String::new();
        assert!(matches!(
            event.validate().unwrap_err(),
            ValidationError::RequiredFieldEmpty { field: "ts" }
        ));
    }

    #[test]
    fn test_validation_missing_source_binary() {
        let mut event = make_valid_event();
        event.source_binary = String::new();
        assert!(matches!(
            event.validate().unwrap_err(),
            ValidationError::RequiredFieldEmpty { field: "source_binary" }
        ));
    }

    #[test]
    fn test_validation_invalid_level() {
        let mut event = make_valid_event();
        event.level = "verbose".to_string();
        let result = event.validate();
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::InvalidLevel { .. }
        ));
    }

    #[test]
    fn test_validation_level_uppercase_ok() {
        // Case-insensitive: "INFO" must be accepted.
        let mut event = make_valid_event();
        event.level = "INFO".to_string();
        assert!(event.validate().is_ok(), "uppercase level should be valid");
    }

    #[test]
    fn test_validation_level_mixed_case_ok() {
        let mut event = make_valid_event();
        event.level = "Warn".to_string();
        assert!(event.validate().is_ok());
    }

    #[test]
    fn test_validation_unsupported_version() {
        let mut event = make_valid_event();
        event.v = 2;
        assert!(matches!(
            event.validate().unwrap_err(),
            ValidationError::UnsupportedVersion { version: 2 }
        ));
    }

    #[test]
    fn test_size_guard() {
        let mut event = make_valid_event();
        // Insert a field with 70 KiB of data.
        let big_value = "x".repeat(70 * 1024);
        event.fields.insert(
            "big_field".to_string(),
            serde_json::Value::String(big_value),
        );
        let result = event.validate();
        assert!(
            matches!(result.unwrap_err(), ValidationError::EventTooLarge { .. }),
            "event with 70 KiB field should fail the size guard"
        );
    }

    #[test]
    fn test_redaction_denylist_keys() {
        let mut event = make_valid_event();
        event.fields.insert(
            "password".to_string(),
            serde_json::Value::String("secret123".to_string()),
        );
        event.fields.insert(
            "normal_key".to_string(),
            serde_json::Value::String("keep_me".to_string()),
        );
        event.redact();

        assert_eq!(
            event.fields.get("password").and_then(|v| v.as_str()),
            Some("[REDACTED]"),
            "password field should be redacted"
        );
        assert_eq!(
            event.fields.get("normal_key").and_then(|v| v.as_str()),
            Some("keep_me"),
            "non-sensitive field should be preserved"
        );
    }

    #[test]
    fn test_redaction_denylist_all_keys() {
        for key in DENYLIST_KEYS {
            let mut event = make_valid_event();
            event.fields.insert(
                key.to_string(),
                serde_json::Value::String("sensitive_value".to_string()),
            );
            event.redact();
            assert_eq!(
                event.fields.get(*key).and_then(|v| v.as_str()),
                Some("[REDACTED]"),
                "key '{key}' should be redacted"
            );
        }
    }

    #[test]
    fn test_redaction_bearer_token() {
        let mut event = make_valid_event();
        event.fields.insert(
            "auth_header".to_string(),
            serde_json::Value::String("Bearer eyJhbGciOiJSUzI1NiJ9.payload.sig".to_string()),
        );
        event.fields.insert(
            "lowercase_bearer".to_string(),
            serde_json::Value::String("bearer some-token-here".to_string()),
        );
        event.fields.insert(
            "not_bearer".to_string(),
            serde_json::Value::String("Basic dXNlcjpwYXNz".to_string()),
        );
        event.redact();

        assert_eq!(
            event.fields.get("auth_header").and_then(|v| v.as_str()),
            Some("[REDACTED]"),
            "Bearer token should be redacted"
        );
        assert_eq!(
            event.fields.get("lowercase_bearer").and_then(|v| v.as_str()),
            Some("[REDACTED]"),
            "lowercase bearer token should be redacted"
        );
        assert_eq!(
            event.fields.get("not_bearer").and_then(|v| v.as_str()),
            Some("Basic dXNlcjpwYXNz"),
            "Basic auth should not be redacted by bearer pattern"
        );
    }

    #[test]
    fn test_redaction_span_fields() {
        let mut event = make_valid_event();
        let mut span_fields = serde_json::Map::new();
        span_fields.insert(
            "token".to_string(),
            serde_json::Value::String("tok_secret_value".to_string()),
        );
        span_fields.insert(
            "safe_field".to_string(),
            serde_json::Value::String("visible".to_string()),
        );
        event.spans.push(SpanRefV1 {
            name: "some_span".to_string(),
            fields: span_fields,
        });

        event.redact();

        assert_eq!(
            event.spans[0].fields.get("token").and_then(|v| v.as_str()),
            Some("[REDACTED]"),
            "span token field should be redacted"
        );
        assert_eq!(
            event.spans[0].fields.get("safe_field").and_then(|v| v.as_str()),
            Some("visible"),
            "safe span field should be preserved"
        );
    }

    #[test]
    fn test_new_log_event_convenience() {
        let event = new_log_event("atm-tui", "tui_start", "atm_tui::main", "debug");
        assert_eq!(event.v, 1);
        assert_eq!(event.source_binary, "atm-tui");
        assert_eq!(event.action, "tui_start");
        assert_eq!(event.target, "atm_tui::main");
        assert_eq!(event.level, "debug");
        assert!(!event.ts.is_empty());
        assert!(!event.hostname.is_empty());
        assert!(event.pid > 0);
    }

    #[test]
    fn test_builder_optional_fields() {
        let event = LogEventV1::builder("atm", "action", "target")
            .team("my-team")
            .agent("my-agent")
            .session_id("sess-1")
            .request_id("req-1")
            .correlation_id("corr-1")
            .outcome("ok")
            .error("none")
            .build();

        assert_eq!(event.team.as_deref(), Some("my-team"));
        assert_eq!(event.agent.as_deref(), Some("my-agent"));
        assert_eq!(event.session_id.as_deref(), Some("sess-1"));
        assert_eq!(event.request_id.as_deref(), Some("req-1"));
        assert_eq!(event.correlation_id.as_deref(), Some("corr-1"));
        assert_eq!(event.outcome.as_deref(), Some("ok"));
        assert_eq!(event.error.as_deref(), Some("none"));
    }

    #[test]
    fn test_option_fields_skip_when_none() {
        let event = make_valid_event();
        let json = serde_json::to_string(&event).expect("serialize");
        // None fields should not appear in JSON output.
        assert!(!json.contains("\"team\""), "team should be absent when None");
        assert!(!json.contains("\"agent\""), "agent should be absent when None");
        assert!(!json.contains("\"session_id\""), "session_id should be absent when None");
    }

    #[test]
    fn test_spool_write() {
        let dir = TempDir::new().expect("temp dir");
        let event = make_valid_event();
        write_to_spool(&event, dir.path());

        let spool = spool_dir(dir.path());
        let entries: Vec<_> = std::fs::read_dir(&spool)
            .expect("read spool dir")
            .flatten()
            .collect();
        assert_eq!(entries.len(), 1, "expected one spool file");

        let content = std::fs::read_to_string(entries[0].path()).expect("read spool file");
        let deserialized: LogEventV1 = serde_json::from_str(content.trim()).expect("parse spool JSONL");
        assert_eq!(deserialized.action, event.action);
    }

    #[test]
    fn test_spool_dir_path() {
        // Use a TempDir as the home path so the path is platform-native.
        let home = TempDir::new().expect("temp dir");
        let expected = home.path().join(".config/atm/log-spool");
        assert_eq!(spool_dir(home.path()), expected);
    }

    #[test]
    fn test_valid_event_passes_validation() {
        let event = make_valid_event();
        assert!(event.validate().is_ok());
    }

    #[test]
    fn test_redaction_case_insensitive_key() {
        let mut event = make_valid_event();
        // "Password" with mixed case should also be redacted.
        event.fields.insert(
            "Password".to_string(),
            serde_json::Value::String("hunter2".to_string()),
        );
        event.redact();
        assert_eq!(
            event.fields.get("Password").and_then(|v| v.as_str()),
            Some("[REDACTED]")
        );
    }
}
