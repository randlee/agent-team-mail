//! Auto mail injection for the MCP proxy.
//!
//! Implements FR-8: fetching unread ATM mail and injecting it as `codex-reply`
//! turns into idle Codex threads.
//!
//! # Key types
//!
//! - [`MailEnvelope`] — a single message formatted for injection
//! - [`MailPoller`] — holds polling configuration derived from [`crate::config::AgentMcpConfig`]
//!
//! # Functions
//!
//! - [`fetch_unread_mail`] — read unread messages without marking them read
//! - [`mark_messages_read`] — mark a set of message IDs as read (called only after delivery)
//! - [`build_mail_envelopes`] — convert [`agent_team_mail_core::InboxMessage`] to [`MailEnvelope`]
//! - [`format_mail_turn_content`] — format a slice of envelopes into an injection prompt string

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use agent_team_mail_core::InboxMessage;
use agent_team_mail_core::home::get_home_dir;
use agent_team_mail_core::io::inbox_update;
use serde::{Deserialize, Serialize};

use crate::config::AgentMcpConfig;

// ---------------------------------------------------------------------------
// MailEnvelope
// ---------------------------------------------------------------------------

/// A single inbound message formatted for injection into a Codex turn.
///
/// Wraps the essential metadata from an [`InboxMessage`] together with
/// potentially-truncated text. Serializable so the full envelope can be
/// embedded in the injection prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailEnvelope {
    /// ATM identity of the sender.
    pub sender: String,
    /// ISO 8601 timestamp of when the message was sent.
    pub timestamp: String,
    /// Unique message ID (used for deduplication and mark-read).
    pub message_id: String,
    /// Message body, possibly truncated.
    pub text: String,
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Format a slice of mail envelopes into a prompt string for codex-reply injection.
///
/// Each envelope is rendered with a header line (From / Time / ID) followed by
/// the message body. The entire block is wrapped with a summary count.
///
/// # Examples
///
/// ```
/// use atm_agent_mcp::mail_inject::{MailEnvelope, format_mail_turn_content};
///
/// let env = MailEnvelope {
///     sender: "alice".into(),
///     timestamp: "2026-02-19T10:00:00Z".into(),
///     message_id: "abc".into(),
///     text: "Hello from alice".into(),
/// };
/// let content = format_mail_turn_content(&[env]);
/// assert!(content.contains("1 unread message"));
/// assert!(content.contains("alice"));
/// ```
pub fn format_mail_turn_content(messages: &[MailEnvelope]) -> String {
    let n = messages.len();
    let noun = if n == 1 { "message" } else { "messages" };
    let mut out = format!("You have {n} unread {noun}:\n\n");
    for (i, env) in messages.iter().enumerate() {
        out.push_str(&format!(
            "[{}] From: {} | Time: {} | ID: {}\n{}\n\n",
            i + 1,
            env.sender,
            env.timestamp,
            env.message_id,
            env.text,
        ));
    }
    out.trim_end().to_string()
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

/// Convert a slice of [`InboxMessage`] values to [`MailEnvelope`] values.
///
/// Applies:
/// - `max_messages` limit (takes only the first N unread messages)
/// - `max_message_length` truncation: if a message body exceeds the limit,
///   it is cut at that character boundary and `" [...truncated]"` is appended.
///
/// Messages that have no `message_id` are skipped because they cannot be
/// reliably marked read after delivery.
///
/// # Examples
///
/// ```
/// use agent_team_mail_core::InboxMessage;
/// use atm_agent_mcp::mail_inject::build_mail_envelopes;
/// use std::collections::HashMap;
///
/// let msg = InboxMessage {
///     from: "bob".into(),
///     text: "hi".into(),
///     timestamp: "2026-02-19T00:00:00Z".into(),
///     read: false,
///     summary: None,
///     message_id: Some("id-1".into()),
///     unknown_fields: HashMap::new(),
/// };
/// let envelopes = build_mail_envelopes(&[msg], 10, 4096);
/// assert_eq!(envelopes.len(), 1);
/// assert_eq!(envelopes[0].sender, "bob");
/// ```
pub fn build_mail_envelopes(
    messages: &[InboxMessage],
    max_messages: usize,
    max_message_length: usize,
) -> Vec<MailEnvelope> {
    const TRUNCATION_SUFFIX: &str = " [...truncated]";

    messages
        .iter()
        .filter(|m| !m.read && m.message_id.is_some())
        .take(max_messages)
        .map(|m| {
            let text = truncate_utf8_chars(&m.text, max_message_length, TRUNCATION_SUFFIX);
            MailEnvelope {
                sender: m.from.clone(),
                timestamp: m.timestamp.clone(),
                message_id: m.message_id.clone().expect("filtered above"),
                text,
            }
        })
        .collect()
}

fn truncate_utf8_chars(text: &str, max_chars: usize, suffix: &str) -> String {
    match text.char_indices().nth(max_chars).map(|(idx, _)| idx) {
        Some(cutoff) => {
            let mut out = text[..cutoff].to_string();
            out.push_str(suffix);
            out
        }
        None => text.to_string(),
    }
}

// ---------------------------------------------------------------------------
// MailPoller
// ---------------------------------------------------------------------------

/// Polling configuration for auto mail injection (FR-8.2, FR-8.8).
///
/// Built from [`AgentMcpConfig`] via [`MailPoller::new`]. Encapsulates all
/// tunable parameters so the proxy loop does not depend on the full config.
#[derive(Debug, Clone)]
pub struct MailPoller {
    /// How often to poll for new mail when a thread is idle.
    pub poll_interval: Duration,
    /// Maximum number of messages to inject per turn (FR-8.5).
    pub max_messages: usize,
    /// Maximum message body length in chars before truncation (FR-8.5).
    pub max_message_length: usize,
    /// Whether auto-mail injection is enabled globally (FR-8.8).
    pub auto_mail_enabled: bool,
}

impl MailPoller {
    /// Build a [`MailPoller`] from the resolved proxy configuration.
    ///
    /// Reads:
    /// - `config.mail_poll_interval_ms` → [`MailPoller::poll_interval`] (default 5000 ms)
    /// - `config.max_mail_messages` → [`MailPoller::max_messages`] (default 10)
    /// - `config.max_mail_message_length` → [`MailPoller::max_message_length`] (default 4096)
    /// - `config.auto_mail` → [`MailPoller::auto_mail_enabled`] (default true)
    pub fn new(config: &AgentMcpConfig) -> Self {
        Self {
            poll_interval: Duration::from_millis(config.mail_poll_interval_ms),
            max_messages: config.max_mail_messages,
            max_message_length: config.max_mail_message_length,
            auto_mail_enabled: config.auto_mail,
        }
    }

    /// Returns `true` when auto-mail injection is globally enabled.
    pub fn is_enabled(&self) -> bool {
        self.auto_mail_enabled
    }
}

// ---------------------------------------------------------------------------
// Inbox path helper
// ---------------------------------------------------------------------------

/// Build the inbox file path for `identity` in `team`.
///
/// Path: `<home>/.claude/teams/<team>/inboxes/<identity>.json`
fn inbox_path(home: &std::path::Path, team: &str, identity: &str) -> PathBuf {
    home.join(".claude")
        .join("teams")
        .join(team)
        .join("inboxes")
        .join(format!("{identity}.json"))
}

// ---------------------------------------------------------------------------
// fetch_unread_mail
// ---------------------------------------------------------------------------

/// Fetch unread messages for `identity` in `team`, returning formatted envelopes.
///
/// Messages are NOT marked as read here — call [`mark_messages_read`] only
/// after the codex-reply has been successfully written to the child's stdin
/// and the request ID recorded (FR-8.12).
///
/// Returns an empty `Vec` when the inbox does not exist or no unread messages
/// are present.
///
/// # Parameters
///
/// - `identity` — the ATM identity whose inbox should be checked
/// - `team` — the ATM team name
/// - `max_messages` — cap on returned envelopes (FR-8.5)
/// - `max_message_length` — per-message character truncation limit (FR-8.5)
pub fn fetch_unread_mail(
    identity: &str,
    team: &str,
    max_messages: usize,
    max_message_length: usize,
) -> Vec<MailEnvelope> {
    let home = match get_home_dir() {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("fetch_unread_mail: cannot resolve home dir: {e}");
            return Vec::new();
        }
    };

    let path = inbox_path(&home, team, identity);
    if !path.exists() {
        return Vec::new();
    }

    let content = match std::fs::read(&path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "fetch_unread_mail: cannot read inbox for '{}': {e}",
                identity
            );
            return Vec::new();
        }
    };

    let messages: Vec<InboxMessage> = match serde_json::from_slice(&content) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                "fetch_unread_mail: failed to parse inbox for '{}': {e}",
                identity
            );
            return Vec::new();
        }
    };

    build_mail_envelopes(&messages, max_messages, max_message_length)
}

// ---------------------------------------------------------------------------
// mark_messages_read
// ---------------------------------------------------------------------------

/// Mark the specified message IDs as read in `identity`'s inbox in `team`.
///
/// This is a best-effort operation: failures are logged as warnings but do
/// not propagate. Callers should invoke this only **after** the codex-reply
/// has been written to the child stdin and the request ID has been recorded
/// (FR-8.12).
///
/// Messages whose `message_id` is `None` are never matched, consistent with
/// how [`build_mail_envelopes`] skips them.
pub fn mark_messages_read(identity: &str, team: &str, message_ids: &[String]) {
    if message_ids.is_empty() {
        return;
    }

    let home = match get_home_dir() {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("mark_messages_read: cannot resolve home dir: {e}");
            return;
        }
    };

    let path = inbox_path(&home, team, identity);
    if !path.exists() {
        return;
    }

    let ids_set: HashSet<&str> = message_ids.iter().map(|s| s.as_str()).collect();
    if let Err(e) = inbox_update(&path, team, identity, |messages| {
        for msg in messages.iter_mut() {
            if let Some(ref mid) = msg.message_id {
                if ids_set.contains(mid.as_str()) {
                    msg.read = true;
                }
            }
        }
    }) {
        tracing::warn!("mark_messages_read: failed atomic update for '{}': {e}", identity);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_core::InboxMessage;
    use serial_test::serial;
    use std::collections::HashMap;
    use std::fs;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn set_atm_home(dir: &TempDir) {
        unsafe { std::env::set_var("ATM_HOME", dir.path()) };
    }

    fn unset_atm_home() {
        unsafe { std::env::remove_var("ATM_HOME") };
    }

    fn make_msg(from: &str, text: &str, read: bool, id: Option<&str>) -> InboxMessage {
        InboxMessage {
            from: from.to_string(),
            text: text.to_string(),
            timestamp: "2026-02-19T10:00:00Z".to_string(),
            read,
            summary: None,
            message_id: id.map(|s| s.to_string()),
            unknown_fields: HashMap::new(),
        }
    }

    fn seed_inbox(home: &std::path::Path, team: &str, agent: &str, messages: &[InboxMessage]) {
        let dir = home
            .join(".claude")
            .join("teams")
            .join(team)
            .join("inboxes");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{agent}.json"));
        fs::write(&path, serde_json::to_string_pretty(messages).unwrap()).unwrap();
    }

    fn read_inbox_file(home: &std::path::Path, team: &str, agent: &str) -> Vec<InboxMessage> {
        let path = home
            .join(".claude")
            .join("teams")
            .join(team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        let content = fs::read_to_string(&path).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    // -----------------------------------------------------------------------
    // format_mail_turn_content
    // -----------------------------------------------------------------------

    #[test]
    fn format_single_message() {
        let env = MailEnvelope {
            sender: "alice".into(),
            timestamp: "2026-02-19T10:00:00Z".into(),
            message_id: "msg-1".into(),
            text: "Hello from alice".into(),
        };
        let content = format_mail_turn_content(&[env]);
        assert!(content.contains("1 unread message"), "singular noun");
        assert!(content.contains("[1]"));
        assert!(content.contains("alice"));
        assert!(content.contains("msg-1"));
        assert!(content.contains("Hello from alice"));
    }

    #[test]
    fn format_multiple_messages_plural_noun() {
        let envs: Vec<MailEnvelope> = (0..3)
            .map(|i| MailEnvelope {
                sender: format!("sender{i}"),
                timestamp: "2026-02-19T10:00:00Z".into(),
                message_id: format!("id-{i}"),
                text: format!("body {i}"),
            })
            .collect();
        let content = format_mail_turn_content(&envs);
        assert!(content.contains("3 unread messages"), "plural noun");
        assert!(content.contains("[3]"));
    }

    // -----------------------------------------------------------------------
    // build_mail_envelopes
    // -----------------------------------------------------------------------

    #[test]
    fn build_envelopes_skips_read_messages() {
        let messages = vec![
            make_msg("a", "unread", false, Some("id-1")),
            make_msg("b", "already read", true, Some("id-2")),
        ];
        let envelopes = build_mail_envelopes(&messages, 10, 4096);
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].message_id, "id-1");
    }

    #[test]
    fn build_envelopes_skips_messages_without_id() {
        let messages = vec![
            make_msg("a", "no id", false, None),
            make_msg("b", "has id", false, Some("id-1")),
        ];
        let envelopes = build_mail_envelopes(&messages, 10, 4096);
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].message_id, "id-1");
    }

    #[test]
    fn build_envelopes_max_messages_limit() {
        let messages: Vec<InboxMessage> = (0..10)
            .map(|i| make_msg("s", &format!("msg{i}"), false, Some(&format!("id-{i}"))))
            .collect();
        let envelopes = build_mail_envelopes(&messages, 3, 4096);
        assert_eq!(envelopes.len(), 3);
    }

    #[test]
    fn build_envelopes_truncates_long_text() {
        let long_text = "x".repeat(100);
        let messages = vec![make_msg("a", &long_text, false, Some("id-1"))];
        let envelopes = build_mail_envelopes(&messages, 10, 10);
        assert_eq!(envelopes.len(), 1);
        assert!(envelopes[0].text.ends_with(" [...truncated]"));
        // Should be exactly 10 chars of content + suffix
        assert_eq!(&envelopes[0].text[..10], &long_text[..10]);
    }

    #[test]
    fn build_envelopes_no_truncation_at_exact_limit() {
        let text = "x".repeat(50);
        let messages = vec![make_msg("a", &text, false, Some("id-1"))];
        let envelopes = build_mail_envelopes(&messages, 10, 50);
        assert_eq!(envelopes[0].text, text);
        assert!(!envelopes[0].text.contains("truncated"));
    }

    #[test]
    fn build_envelopes_truncation_is_utf8_safe() {
        let text = "é".repeat(20);
        let messages = vec![make_msg("a", &text, false, Some("id-1"))];
        let envelopes = build_mail_envelopes(&messages, 10, 5);
        assert_eq!(envelopes.len(), 1);
        assert!(envelopes[0].text.ends_with(" [...truncated]"));
    }

    // -----------------------------------------------------------------------
    // MailPoller::new
    // -----------------------------------------------------------------------

    #[test]
    fn mail_poller_uses_config_defaults() {
        let config = AgentMcpConfig::default();
        let poller = MailPoller::new(&config);
        assert_eq!(poller.poll_interval, Duration::from_millis(5000));
        assert_eq!(poller.max_messages, 10);
        assert_eq!(poller.max_message_length, 4096);
        assert!(poller.is_enabled());
    }

    #[test]
    fn mail_poller_disabled_when_auto_mail_false() {
        let config = AgentMcpConfig {
            auto_mail: false,
            ..Default::default()
        };
        let poller = MailPoller::new(&config);
        assert!(!poller.is_enabled());
    }

    #[test]
    fn mail_poller_custom_values() {
        let config = AgentMcpConfig {
            mail_poll_interval_ms: 2000,
            max_mail_messages: 5,
            max_mail_message_length: 1024,
            ..Default::default()
        };
        let poller = MailPoller::new(&config);
        assert_eq!(poller.poll_interval, Duration::from_millis(2000));
        assert_eq!(poller.max_messages, 5);
        assert_eq!(poller.max_message_length, 1024);
    }

    // -----------------------------------------------------------------------
    // fetch_unread_mail
    // -----------------------------------------------------------------------

    #[test]
    #[serial]
    fn fetch_returns_empty_when_inbox_missing() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);
        let envelopes = fetch_unread_mail("nobody", "team", 10, 4096);
        unset_atm_home();
        assert!(envelopes.is_empty());
    }

    #[test]
    #[serial]
    fn fetch_returns_unread_envelopes() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        seed_inbox(
            dir.path(),
            "team",
            "agent",
            &[
                make_msg("alice", "hello", false, Some("id-1")),
                make_msg("bob", "already read", true, Some("id-2")),
            ],
        );

        let envelopes = fetch_unread_mail("agent", "team", 10, 4096);
        unset_atm_home();

        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].sender, "alice");
        assert_eq!(envelopes[0].message_id, "id-1");
    }

    #[test]
    #[serial]
    fn fetch_does_not_mark_messages_read() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        seed_inbox(
            dir.path(),
            "team",
            "agent",
            &[make_msg("alice", "hello", false, Some("id-1"))],
        );

        fetch_unread_mail("agent", "team", 10, 4096);
        let messages = read_inbox_file(dir.path(), "team", "agent");
        unset_atm_home();

        assert!(!messages[0].read, "fetch must not mark messages as read");
    }

    // -----------------------------------------------------------------------
    // mark_messages_read
    // -----------------------------------------------------------------------

    #[test]
    #[serial]
    fn mark_read_updates_inbox_file() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        seed_inbox(
            dir.path(),
            "team",
            "agent",
            &[
                make_msg("a", "msg1", false, Some("id-1")),
                make_msg("b", "msg2", false, Some("id-2")),
            ],
        );

        mark_messages_read("agent", "team", &["id-1".to_string()]);
        let messages = read_inbox_file(dir.path(), "team", "agent");
        unset_atm_home();

        let msg1 = messages.iter().find(|m| m.message_id.as_deref() == Some("id-1")).unwrap();
        let msg2 = messages.iter().find(|m| m.message_id.as_deref() == Some("id-2")).unwrap();
        assert!(msg1.read, "id-1 should be marked read");
        assert!(!msg2.read, "id-2 should remain unread");
    }

    #[test]
    #[serial]
    fn mark_read_noop_on_empty_id_list() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        seed_inbox(
            dir.path(),
            "team",
            "agent",
            &[make_msg("a", "msg", false, Some("id-1"))],
        );

        mark_messages_read("agent", "team", &[]);
        let messages = read_inbox_file(dir.path(), "team", "agent");
        unset_atm_home();

        assert!(!messages[0].read, "no messages should be marked when ids list is empty");
    }

    #[test]
    #[serial]
    fn mark_read_noop_when_inbox_missing() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);
        // Should not panic when inbox doesn't exist
        mark_messages_read("nobody", "team", &["id-1".to_string()]);
        unset_atm_home();
    }

    // -----------------------------------------------------------------------
    // Delivery sequencing: fetch → format → dispatch → mark-read
    // -----------------------------------------------------------------------

    #[test]
    #[serial]
    fn full_injection_sequence_marks_read_only_after_delivery() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        seed_inbox(
            dir.path(),
            "team",
            "agent",
            &[
                make_msg("alice", "msg-a", false, Some("m-a")),
                make_msg("bob", "msg-b", false, Some("m-b")),
            ],
        );

        // Step 1: fetch (no mark-read yet)
        let envelopes = fetch_unread_mail("agent", "team", 10, 4096);
        assert_eq!(envelopes.len(), 2);

        // Step 2: messages still unread
        {
            let msgs = read_inbox_file(dir.path(), "team", "agent");
            assert!(msgs.iter().all(|m| !m.read), "must remain unread before delivery");
        }

        // Step 3: format content (simulates writing to child stdin)
        let content = format_mail_turn_content(&envelopes);
        assert!(content.contains("alice"));

        // Step 4: mark read (called after successful child write)
        let ids: Vec<String> = envelopes.iter().map(|e| e.message_id.clone()).collect();
        mark_messages_read("agent", "team", &ids);

        // Step 5: verify messages are now read
        let msgs = read_inbox_file(dir.path(), "team", "agent");
        unset_atm_home();
        assert!(msgs.iter().all(|m| m.read), "all messages should be marked read after delivery");
    }
}
