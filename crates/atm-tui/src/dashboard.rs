//! Dashboard panel helpers: inbox count reads and session log path resolution.
//!
//! This module does not own any rendering code — that lives in [`crate::ui`].
//! It provides pure functions for computing the data shown in the left panel.

use std::path::{Path, PathBuf};

use agent_team_mail_core::home::get_home_dir;

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
    let inbox_path = home
        .join(".claude/teams")
        .join(team)
        .join("inboxes")
        .join(format!("{agent}.json"));

    if !inbox_path.exists() {
        return 0;
    }

    match std::fs::read_to_string(&inbox_path) {
        Ok(content) if !content.trim().is_empty() => {
            serde_json::from_str::<Vec<serde_json::Value>>(&content)
                .map(|v| v.len())
                .unwrap_or(0)
        }
        _ => 0,
    }
}

/// Construct the expected path for an agent's session log file.
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
pub fn session_log_path(team: &str, agent: &str) -> PathBuf {
    let base = get_home_dir().unwrap_or_default();
    base.join(".config/atm/agent-sessions")
        .join(team)
        .join(agent)
        .join("output.log")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: set ATM_HOME to the temp dir path (used instead of `serial_test`
    /// because the tests use different temp dirs and do not conflict).
    fn with_tmp_home<F: FnOnce(&Path)>(f: F) {
        let tmp = TempDir::new().expect("tmp dir");
        // SAFETY: test-only env mutation; tests run in isolation within a
        // single-threaded test process for this module.
        unsafe { std::env::set_var("ATM_HOME", tmp.path()) };
        f(tmp.path());
        unsafe { std::env::remove_var("ATM_HOME") };
    }

    // ── test_session_log_path ────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_session_log_path() {
        with_tmp_home(|home| {
            let path = session_log_path("atm-dev", "arch-ctm");
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
    #[serial]
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
    #[serial]
    fn test_inbox_count_with_messages() {
        with_tmp_home(|home| {
            let inbox_dir = home.join(".claude/teams/atm-dev/inboxes");
            fs::create_dir_all(&inbox_dir).unwrap();
            let inbox_path = inbox_dir.join("arch-ctm.json");

            // Write a valid JSON array with 3 messages.
            let payload = r#"[{"message_id":"m1"},{"message_id":"m2"},{"message_id":"m3"}]"#;
            fs::write(&inbox_path, payload).unwrap();

            let count = get_inbox_count(home, "atm-dev", "arch-ctm");
            assert_eq!(count, 3, "expected 3 messages in inbox");
        });
    }
}
