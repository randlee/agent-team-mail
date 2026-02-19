//! Append-only JSONL audit log for ATM tool calls and Codex forwards (FR-9).
//!
//! [`AuditLog`] writes structured entries to a team-wide audit file at
//! `{sessions_dir}/{team}/audit.jsonl`. Each line is a self-contained JSON
//! object that can be filtered by `agent_id` and `identity` for per-session
//! analysis.
//!
//! # Design principles
//!
//! - **Non-fatal**: All write errors are swallowed and logged via `tracing::warn`.
//!   Audit failures must never crash the proxy.
//! - **Append-only**: The file is opened in append mode for every write to
//!   tolerate concurrent proxy instances (though rare in practice).
//! - **Structured**: Each line is valid JSON matching [`AuditEntry`].

use std::path::PathBuf;

use serde::Serialize;

/// Maximum number of characters kept from a prompt for audit logging (FR-9.2).
const PROMPT_SUMMARY_MAX: usize = 200;

/// Maximum number of characters kept from a message for audit logging (FR-9.1).
const MESSAGE_SUMMARY_MAX: usize = 200;

/// A single audit log entry, serialized as one JSONL line.
#[derive(Debug, Serialize)]
pub struct AuditEntry {
    /// ISO 8601 UTC timestamp.
    pub timestamp: String,
    /// Event type: `"atm_send"`, `"atm_read"`, `"atm_broadcast"`,
    /// `"atm_pending_count"`, `"codex"`, `"codex-reply"`.
    pub event_type: String,
    /// Codex agent_id associated with this event, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// ATM identity associated with this event, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// Recipient for `atm_send` events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    /// Truncated message content for ATM tool calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_summary: Option<String>,
    /// First 200 characters of the prompt for `codex`/`codex-reply` forwards.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_summary: Option<String>,
}

/// Append-only audit log writer for a single team.
///
/// Each [`AuditLog`] instance writes to
/// `{sessions_dir}/{team}/audit.jsonl`.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    /// Create an audit log for the given team.
    ///
    /// The log file path is resolved via [`crate::lock::sessions_dir()`].
    pub fn new(team: &str) -> Self {
        let path = crate::lock::sessions_dir().join(team).join("audit.jsonl");
        Self { path }
    }

    /// Create an audit log with an explicit path (for testing).
    pub fn new_with_path(path: PathBuf) -> Self {
        Self { path }
    }

    /// Log an ATM tool call (FR-9.1).
    ///
    /// `event_type` should be one of `"atm_send"`, `"atm_read"`,
    /// `"atm_broadcast"`, `"atm_pending_count"`.
    pub async fn log_atm_call(
        &self,
        event_type: &str,
        agent_id: Option<&str>,
        identity: Option<&str>,
        recipient: Option<&str>,
        message_summary: Option<&str>,
    ) {
        let entry = AuditEntry {
            timestamp: now_iso8601(),
            event_type: event_type.to_string(),
            agent_id: agent_id.map(String::from),
            identity: identity.map(String::from),
            recipient: recipient.map(String::from),
            message_summary: message_summary.map(|s| truncate(s, MESSAGE_SUMMARY_MAX)),
            prompt_summary: None,
        };
        self.append(&entry).await;
    }

    /// Log a `codex` or `codex-reply` forward to the child process (FR-9.2).
    ///
    /// The prompt is truncated to 200 characters.
    pub async fn log_codex_forward(
        &self,
        event_type: &str,
        agent_id: Option<&str>,
        identity: Option<&str>,
        prompt: &str,
    ) {
        let entry = AuditEntry {
            timestamp: now_iso8601(),
            event_type: event_type.to_string(),
            agent_id: agent_id.map(String::from),
            identity: identity.map(String::from),
            recipient: None,
            message_summary: None,
            prompt_summary: Some(truncate(prompt, PROMPT_SUMMARY_MAX)),
        };
        self.append(&entry).await;
    }

    /// Append a serialized entry to the audit file.
    ///
    /// Creates parent directories if needed. Swallows all errors.
    async fn append(&self, entry: &AuditEntry) {
        if let Err(e) = self.try_append(entry).await {
            tracing::warn!(path = %self.path.display(), error = %e, "audit log write failed");
        }
    }

    async fn try_append(&self, entry: &AuditEntry) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;

        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let mut line = serde_json::to_string(entry)
            .map_err(std::io::Error::other)?;
        line.push('\n');

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }
}

/// Truncate a string to `max_chars` characters (Unicode-safe).
fn truncate(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Return the current UTC time as an ISO 8601 string.
fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Reuse the same algorithm used throughout the crate.
    let total_days = secs / 86400;
    let time_of_day = secs % 86400;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;
    let (year, month, day) = days_to_ymd(total_days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    fn setup_atm_home(dir: &TempDir) {
        // SAFETY: tests are serialised via #[serial]; no concurrent env mutation.
        unsafe { std::env::set_var("ATM_HOME", dir.path().to_str().unwrap()) };
    }

    fn teardown_atm_home() {
        unsafe { std::env::remove_var("ATM_HOME") };
    }

    fn read_audit_lines(path: &std::path::Path) -> Vec<serde_json::Value> {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[tokio::test]
    #[serial]
    async fn test_audit_log_creates_file() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        let log = AuditLog::new("test-team");
        log.log_atm_call("atm_send", None, None, None, None).await;

        teardown_atm_home();

        assert!(log.path.exists(), "audit file should be created");
    }

    #[tokio::test]
    #[serial]
    async fn test_audit_log_writes_valid_json() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        let log = AuditLog::new("test-team");
        log.log_atm_call("atm_send", Some("codex:abc"), Some("dev"), Some("arch-ctm"), Some("hello"))
            .await;

        teardown_atm_home();

        let entries = read_audit_lines(&log.path);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].get("timestamp").is_some());
        assert_eq!(entries[0]["event_type"], "atm_send");
    }

    #[tokio::test]
    #[serial]
    async fn test_audit_log_event_type_atm_send() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        let log = AuditLog::new("test-team");
        log.log_atm_call(
            "atm_send",
            Some("codex:123"),
            Some("team-lead"),
            Some("arch-ctm"),
            Some("Hello there"),
        )
        .await;

        teardown_atm_home();

        let entries = read_audit_lines(&log.path);
        assert_eq!(entries[0]["event_type"], "atm_send");
        assert_eq!(entries[0]["agent_id"], "codex:123");
        assert_eq!(entries[0]["identity"], "team-lead");
        assert_eq!(entries[0]["recipient"], "arch-ctm");
        assert_eq!(entries[0]["message_summary"], "Hello there");
        // prompt_summary should be absent for ATM calls
        assert!(entries[0].get("prompt_summary").is_none());
    }

    #[tokio::test]
    #[serial]
    async fn test_audit_log_event_type_codex() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        let log = AuditLog::new("test-team");
        log.log_codex_forward("codex", Some("codex:456"), Some("dev"), "Build a feature")
            .await;

        teardown_atm_home();

        let entries = read_audit_lines(&log.path);
        assert_eq!(entries[0]["event_type"], "codex");
        assert_eq!(entries[0]["agent_id"], "codex:456");
        assert_eq!(entries[0]["prompt_summary"], "Build a feature");
        // recipient and message_summary should be absent for codex forwards
        assert!(entries[0].get("recipient").is_none());
        assert!(entries[0].get("message_summary").is_none());
    }

    #[tokio::test]
    #[serial]
    async fn test_audit_log_prompt_truncated_at_200_chars() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        let long_prompt = "x".repeat(500);
        let log = AuditLog::new("test-team");
        log.log_codex_forward("codex", None, None, &long_prompt)
            .await;

        teardown_atm_home();

        let entries = read_audit_lines(&log.path);
        let summary = entries[0]["prompt_summary"].as_str().unwrap();
        assert_eq!(summary.len(), 200);
    }

    #[tokio::test]
    #[serial]
    async fn test_audit_log_prompt_truncated_unicode_safe() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        // Each emoji is 4 bytes; 300 emojis = 1200 bytes but only 300 chars.
        // Without the fix, byte-slicing at index 200 would split a multi-byte sequence and panic.
        let emoji_prompt = "🎉".repeat(300);
        let log = AuditLog::new("test-team");
        log.log_codex_forward("codex", None, None, &emoji_prompt)
            .await;

        teardown_atm_home();

        let entries = read_audit_lines(&log.path);
        let summary = entries[0]["prompt_summary"].as_str().unwrap();
        // Truncated to 200 Unicode characters (each emoji is 4 bytes = 800 bytes, not 200).
        assert_eq!(summary.chars().count(), 200);
    }

    #[tokio::test]
    #[serial]
    async fn test_audit_log_appends_multiple_entries() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        let log = AuditLog::new("test-team");
        log.log_atm_call("atm_send", None, None, None, Some("msg1"))
            .await;
        log.log_atm_call("atm_read", None, None, None, None).await;
        log.log_codex_forward("codex-reply", None, None, "prompt")
            .await;

        teardown_atm_home();

        let entries = read_audit_lines(&log.path);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["event_type"], "atm_send");
        assert_eq!(entries[1]["event_type"], "atm_read");
        assert_eq!(entries[2]["event_type"], "codex-reply");
    }

    #[tokio::test]
    #[serial]
    async fn test_audit_log_swallows_write_error() {
        // Use a path under /dev/null (or a non-writable location) to trigger an error.
        // On all platforms, writing to a directory path will fail.
        let log = AuditLog::new_with_path(std::path::PathBuf::from("/dev/null/impossible/audit.jsonl"));
        // Must not panic
        log.log_atm_call("atm_send", None, None, None, None).await;
    }

    #[tokio::test]
    #[serial]
    async fn test_audit_log_atm_read_entry() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        let log = AuditLog::new("test-team");
        log.log_atm_call("atm_read", Some("codex:789"), Some("reader"), None, None)
            .await;

        teardown_atm_home();

        let entries = read_audit_lines(&log.path);
        assert_eq!(entries[0]["event_type"], "atm_read");
        assert_eq!(entries[0]["agent_id"], "codex:789");
        assert_eq!(entries[0]["identity"], "reader");
        assert!(entries[0].get("recipient").is_none());
        assert!(entries[0].get("message_summary").is_none());
    }
}
