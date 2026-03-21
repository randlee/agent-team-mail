//! Dashboard panel helpers: inbox count reads and session log path resolution.
//!
//! This module does not own any rendering code — that lives in [`crate::ui`].
//! It provides pure functions for computing the data shown in the left panel.

use std::path::{Path, PathBuf};

use agent_team_mail_core::home::{config_team_dir_for, get_home_dir};
use agent_team_mail_core::io::inbox_read_file_tolerant;
use agent_team_mail_core::io::lock::acquire_lock;
use agent_team_mail_core::schema::InboxMessage;
use serde_json::Value;

/// Read the number of messages in an agent's inbox file.
///
/// Returns `0` when the inbox does not exist, is empty, or cannot be parsed.
/// Never panics or propagates errors — silently returns `0` on any failure.
///
/// # Arguments
///
/// * `home` - ATM home directory (resolved via [`get_home_dir`]).
/// * `team` - Team name.
/// * `agent` - Agent name.
pub fn get_inbox_count(home: &Path, team: &str, agent: &str) -> usize {
    let inbox_path = config_team_dir_for(home, team)
        .join("inboxes")
        .join(format!("{agent}.json"));

    if !inbox_path.exists() {
        return 0;
    }

    match inbox_read_file_tolerant(&inbox_path) {
        Ok(messages) => messages.len(),
        Err(_) => 0,
    }
}

/// Read team member names from `~/.claude/teams/{team}/config.json`.
///
/// Returns an empty vector when the config is missing or malformed.
pub fn read_team_members(home: &Path, team: &str) -> Vec<String> {
    let config_path = config_team_dir_for(home, team).join("config.json");
    let lock_path = config_path.with_extension("lock");
    let _lock = match acquire_lock(&lock_path, 5) {
        Ok(lock) => lock,
        Err(_) => return Vec::new(),
    };
    let content = match std::fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let root: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    root.get("members")
        .and_then(|v| v.as_array())
        .map(|members| {
            members
                .iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Read recent inbox message previews for an agent.
///
/// Returns up to `max_items` lines formatted for compact dashboard display.
pub fn read_inbox_preview(home: &Path, team: &str, agent: &str, max_items: usize) -> Vec<String> {
    let inbox_path = config_team_dir_for(home, team)
        .join("inboxes")
        .join(format!("{agent}.json"));
    let lock_path = inbox_path.with_extension("lock");
    let _lock = match acquire_lock(&lock_path, 5) {
        Ok(lock) => lock,
        Err(_) => return Vec::new(),
    };
    let messages: Vec<InboxMessage> = match inbox_read_file_tolerant(&inbox_path) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    messages
        .iter()
        .rev()
        .take(max_items)
        .map(|m| {
            let from = m.from.as_str();
            let summary = m.summary.as_deref().unwrap_or(m.text.as_str());
            let mut line = format!("{from}: {summary}");
            if line.len() > 100 {
                line.truncate(97);
                line.push_str("...");
            }
            line
        })
        .collect()
}

/// Read recent inbox messages for an agent (newest first).
pub fn read_inbox_messages(
    home: &Path,
    team: &str,
    agent: &str,
    max_items: usize,
) -> Vec<InboxMessage> {
    let inbox_path = config_team_dir_for(home, team)
        .join("inboxes")
        .join(format!("{agent}.json"));
    let lock_path = inbox_path.with_extension("lock");
    let _lock = match acquire_lock(&lock_path, 5) {
        Ok(lock) => lock,
        Err(_) => return Vec::new(),
    };
    let messages: Vec<InboxMessage> = match inbox_read_file_tolerant(&inbox_path) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    messages.into_iter().rev().take(max_items).collect()
}

/// Mark a message as read in an agent inbox file.
///
/// Returns:
/// - `Ok(true)` when message read-state changed from unread -> read.
/// - `Ok(false)` when target message was already read or not found.
/// - `Err(...)` on lock/read/parse/write failure.
pub fn mark_inbox_message_read(
    home: &Path,
    team: &str,
    agent: &str,
    message_id: Option<&str>,
    from: &str,
    timestamp: &str,
) -> Result<bool, String> {
    let inbox_path = config_team_dir_for(home, team)
        .join("inboxes")
        .join(format!("{agent}.json"));
    let lock_path = inbox_path.with_extension("lock");
    let _lock = acquire_lock(&lock_path, 5).map_err(|e| format!("lock failed: {e}"))?;
    let content = std::fs::read(&inbox_path).map_err(|e| format!("read failed: {e}"))?;
    let mut messages: Vec<InboxMessage> =
        serde_json::from_slice(&content).map_err(|e| format!("parse failed: {e}"))?;

    let mut changed = false;
    for msg in &mut messages {
        let is_match = if let Some(mid) = message_id {
            msg.message_id.as_deref() == Some(mid)
        } else {
            msg.from == from && msg.timestamp == timestamp
        };
        if is_match {
            if !msg.read {
                msg.read = true;
                changed = true;
            }
            break;
        }
    }

    if changed {
        let payload =
            serde_json::to_vec_pretty(&messages).map_err(|e| format!("serialize failed: {e}"))?;
        std::fs::write(&inbox_path, payload).map_err(|e| format!("write failed: {e}"))?;
    }
    Ok(changed)
}

/// Construct the expected path for an agent's raw Claude Code session transcript.
///
/// This path is consumed by the **Agent Terminal** panel to display the raw
/// session output of a running Claude Code agent.  It is **not** the unified
/// structured log — for structured [`LogEventV1`] events (shown in the Log
/// Viewer panel) see `~/.config/atm/atm.log.jsonl`.
///
/// Path pattern:
/// ```text
/// {ATM_HOME}/.config/atm/agent-sessions/{team}/{agent}/output.log
/// ```
///
/// `ATM_HOME` is resolved via [`get_home_dir`], which honours the `ATM_HOME`
/// environment variable before falling back to the platform home directory.
///
/// # Arguments
///
/// * `team`  - Team name.
/// * `agent` - Agent identifier.
///
/// [`LogEventV1`]: agent_team_mail_core::logging_event::LogEventV1
pub fn session_log_path(team: &str, agent: &str) -> Option<PathBuf> {
    let base = get_home_dir().ok()?;
    Some(
        base.join(".config/atm/agent-sessions")
            .join(team)
            .join(agent)
            .join("output.log"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Serialises tests that mutate `ATM_HOME` to prevent races when the full
    /// workspace test suite runs crate tests in parallel.
    static ATM_HOME_LOCK: Mutex<()> = Mutex::new(());

    /// Helper: acquire the serialisation lock, set ATM_HOME to the temp dir path,
    /// run the closure, then restore.
    fn with_tmp_home<F: FnOnce(&Path)>(f: F) {
        let _guard = ATM_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().expect("tmp dir");
        // SAFETY: env mutation serialised by ATM_HOME_LOCK.
        unsafe { std::env::set_var("ATM_HOME", tmp.path()) };
        f(tmp.path());
        unsafe { std::env::remove_var("ATM_HOME") };
    }

    // ── test_session_log_path ────────────────────────────────────────────────

    #[test]
    fn test_session_log_path() {
        with_tmp_home(|home| {
            let path = session_log_path("atm-dev", "arch-ctm").expect("session log path");
            let expected = home
                .join(".config/atm/agent-sessions")
                .join("atm-dev")
                .join("arch-ctm")
                .join("output.log");
            assert_eq!(path, expected, "session log path mismatch");
        });
    }

    // ── test_inbox_count_empty ───────────────────────────────────────────────

    #[test]
    fn test_inbox_count_empty() {
        with_tmp_home(|home| {
            // Inbox file does not exist yet.
            let count = get_inbox_count(home, "atm-dev", "arch-ctm");
            assert_eq!(count, 0, "missing inbox should return 0");

            // Create an empty inbox file.
            let inbox_dir = home.join(".claude/teams/atm-dev/inboxes");
            fs::create_dir_all(&inbox_dir).unwrap();
            let inbox_path = inbox_dir.join("arch-ctm.json");
            fs::write(&inbox_path, b"").unwrap();
            let count = get_inbox_count(home, "atm-dev", "arch-ctm");
            assert_eq!(count, 0, "empty inbox file should return 0");
        });
    }

    // ── test_inbox_count_with_messages ──────────────────────────────────────

    #[test]
    fn test_inbox_count_with_messages() {
        with_tmp_home(|home| {
            let inbox_dir = home.join(".claude/teams/atm-dev/inboxes");
            fs::create_dir_all(&inbox_dir).unwrap();
            let inbox_path = inbox_dir.join("arch-ctm.json");

            // Write a valid JSON array with 3 minimally valid messages.
            let payload = r#"[{"message_id":"m1","from":"a","text":"one","timestamp":"2026-01-01T00:00:00Z"},{"message_id":"m2","from":"b","text":"two","timestamp":"2026-01-01T00:00:01Z"},{"message_id":"m3","from":"c","text":"three","timestamp":"2026-01-01T00:00:02Z"}]"#;
            fs::write(&inbox_path, payload).unwrap();

            let count = get_inbox_count(home, "atm-dev", "arch-ctm");
            assert_eq!(count, 3, "expected 3 messages in inbox");
        });
    }

    #[test]
    fn test_read_team_members_from_config() {
        with_tmp_home(|home| {
            let team_dir = home.join(".claude/teams/atm-dev");
            fs::create_dir_all(&team_dir).unwrap();
            fs::write(
                team_dir.join("config.json"),
                r#"{"members":[{"name":"team-lead"},{"name":"arch-ctm"}]}"#,
            )
            .unwrap();

            let members = read_team_members(home, "atm-dev");
            assert_eq!(members, vec!["team-lead", "arch-ctm"]);
        });
    }

    #[test]
    fn test_read_inbox_preview_returns_recent_messages() {
        with_tmp_home(|home| {
            let inbox_dir = home.join(".claude/teams/atm-dev/inboxes");
            fs::create_dir_all(&inbox_dir).unwrap();
            fs::write(
                inbox_dir.join("arch-ctm.json"),
                r#"[{"from":"a","text":"one","timestamp":"2026-01-01T00:00:00Z","read":false,"summary":"one"},{"from":"b","text":"two","timestamp":"2026-01-01T00:00:01Z","read":false,"summary":"two"},{"from":"c","text":"three","timestamp":"2026-01-01T00:00:02Z","read":false,"summary":"three"}]"#,
            )
            .unwrap();

            let preview = read_inbox_preview(home, "atm-dev", "arch-ctm", 2);
            assert_eq!(preview, vec!["c: three", "b: two"]);
        });
    }

    #[test]
    fn test_read_inbox_messages_returns_recent_first() {
        with_tmp_home(|home| {
            let inbox_dir = home.join(".claude/teams/atm-dev/inboxes");
            fs::create_dir_all(&inbox_dir).unwrap();
            fs::write(
                inbox_dir.join("arch-ctm.json"),
                r#"[{"from":"a","text":"one","timestamp":"2026-01-01T00:00:00Z","read":false},{"from":"b","text":"two","timestamp":"2026-01-01T00:00:01Z","read":true},{"from":"c","text":"three","timestamp":"2026-01-01T00:00:02Z","read":false}]"#,
            )
            .unwrap();

            let messages = read_inbox_messages(home, "atm-dev", "arch-ctm", 2);
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[0].from, "c");
            assert_eq!(messages[1].from, "b");
        });
    }

    #[test]
    fn test_mark_inbox_message_read_updates_file() {
        with_tmp_home(|home| {
            let inbox_dir = home.join(".claude/teams/atm-dev/inboxes");
            fs::create_dir_all(&inbox_dir).unwrap();
            fs::write(
                inbox_dir.join("arch-ctm.json"),
                r#"[{"from":"a","text":"one","timestamp":"2026-01-01T00:00:00Z","read":false,"message_id":"m1"},{"from":"b","text":"two","timestamp":"2026-01-01T00:00:01Z","read":false,"message_id":"m2"}]"#,
            )
            .unwrap();

            let changed = mark_inbox_message_read(
                home,
                "atm-dev",
                "arch-ctm",
                Some("m2"),
                "b",
                "2026-01-01T00:00:01Z",
            )
            .unwrap();
            assert!(changed);

            let after = read_inbox_messages(home, "atm-dev", "arch-ctm", 10);
            let marked = after.iter().find(|m| m.message_id.as_deref() == Some("m2"));
            assert!(marked.is_some_and(|m| m.read));
        });
    }
}
