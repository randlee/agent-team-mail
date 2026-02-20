//! Session registry for tracking Claude Code agent sessions.
//!
//! The registry maps agent names (e.g., `"team-lead"`) to session records
//! containing the Claude Code session ID and the OS process ID. It is shared
//! between the socket server and any component that writes session lifecycle
//! events (e.g., the hook watcher).
//!
//! ## Liveness
//!
//! Liveness is checked using `kill(pid, 0)` on Unix, which probes whether the
//! process exists without sending an actual signal. On non-Unix platforms the
//! check always returns `false` (conservative: treat as dead).
//!
//! ## Thread safety
//!
//! The registry itself is not `Sync`. Callers are expected to wrap it in
//! `Arc<Mutex<SessionRegistry>>` before sharing between tasks.

use std::collections::HashMap;

/// Lifecycle state of a tracked agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// Session is believed to be running.
    Active,
    /// Session has been explicitly marked dead (e.g., process exited).
    Dead,
}

/// A single agent session record.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    /// Claude Code session UUID (from `CLAUDE_SESSION_ID`).
    pub session_id: String,
    /// OS process ID of the agent process.
    pub process_id: u32,
    /// Current lifecycle state.
    pub state: SessionState,
}

impl SessionRecord {
    /// Return `true` if the OS process is still alive.
    ///
    /// Uses `kill(pid, 0)` on Unix. Always returns `false` on non-Unix platforms.
    pub fn is_process_alive(&self) -> bool {
        is_pid_alive(self.process_id)
    }
}

/// Registry mapping agent names to their session records.
///
/// Wrap in `Arc<Mutex<SessionRegistry>>` for concurrent access.
#[derive(Debug, Default)]
pub struct SessionRegistry {
    sessions: HashMap<String, SessionRecord>,
}

impl SessionRegistry {
    /// Create a new, empty registry.
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    /// Insert or update the session record for `name`.
    ///
    /// If an entry already exists its `session_id`, `process_id`, and `state`
    /// are replaced. The state is reset to [`SessionState::Active`] on every
    /// upsert.
    pub fn upsert(&mut self, name: &str, session_id: &str, process_id: u32) {
        self.sessions.insert(
            name.to_string(),
            SessionRecord {
                session_id: session_id.to_string(),
                process_id,
                state: SessionState::Active,
            },
        );
    }

    /// Return an immutable reference to the session record for `name`, or
    /// `None` if the agent is not registered.
    pub fn query(&self, name: &str) -> Option<&SessionRecord> {
        self.sessions.get(name)
    }

    /// Mark the session for `name` as [`SessionState::Dead`].
    ///
    /// Does nothing if the agent is not registered.
    pub fn mark_dead(&mut self, name: &str) {
        if let Some(record) = self.sessions.get_mut(name) {
            record.state = SessionState::Dead;
        }
    }

    /// Return the number of registered sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Return `true` if no sessions are registered.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// Shared, thread-safe session registry handle.
pub type SharedSessionRegistry = std::sync::Arc<std::sync::Mutex<SessionRegistry>>;

/// Create a new empty [`SharedSessionRegistry`].
pub fn new_session_registry() -> SharedSessionRegistry {
    std::sync::Arc::new(std::sync::Mutex::new(SessionRegistry::new()))
}

// ── Platform-specific liveness check ─────────────────────────────────────────

/// Check whether an OS process with the given PID is alive.
///
/// On Unix this uses `kill(pid, 0)` — a read-only existence probe that sends
/// no signal. On non-Unix platforms this always returns `false`.
pub fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        pid_alive_unix(pid)
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(unix)]
fn pid_alive_unix(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) is a read-only existence check; no signal is sent.
    unsafe extern "C" {
        fn kill(pid: libc::pid_t, sig: libc::c_int) -> libc::c_int;
    }
    let pid_t = pid as libc::pid_t;
    // SAFETY: kill with sig=0 never sends a signal; it only checks PID existence.
    let result = unsafe { kill(pid_t, 0) };
    result == 0
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_registry_is_empty() {
        let reg = SessionRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn test_upsert_inserts_new_entry() {
        let mut reg = SessionRegistry::new();
        reg.upsert("team-lead", "session-abc", 1234);

        let record = reg.query("team-lead").unwrap();
        assert_eq!(record.session_id, "session-abc");
        assert_eq!(record.process_id, 1234);
        assert_eq!(record.state, SessionState::Active);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn test_upsert_updates_existing_entry() {
        let mut reg = SessionRegistry::new();
        reg.upsert("team-lead", "session-old", 1000);
        reg.upsert("team-lead", "session-new", 2000);

        let record = reg.query("team-lead").unwrap();
        assert_eq!(record.session_id, "session-new");
        assert_eq!(record.process_id, 2000);
        assert_eq!(record.state, SessionState::Active);
        assert_eq!(reg.len(), 1); // still one entry
    }

    #[test]
    fn test_upsert_resets_dead_to_active() {
        let mut reg = SessionRegistry::new();
        reg.upsert("team-lead", "session-abc", 1234);
        reg.mark_dead("team-lead");
        assert_eq!(reg.query("team-lead").unwrap().state, SessionState::Dead);

        // Re-upsert should reset to Active
        reg.upsert("team-lead", "session-xyz", 5678);
        assert_eq!(reg.query("team-lead").unwrap().state, SessionState::Active);
    }

    #[test]
    fn test_query_nonexistent_returns_none() {
        let reg = SessionRegistry::new();
        assert!(reg.query("ghost").is_none());
    }

    #[test]
    fn test_mark_dead_changes_state() {
        let mut reg = SessionRegistry::new();
        reg.upsert("team-lead", "session-abc", 1234);
        reg.mark_dead("team-lead");

        let record = reg.query("team-lead").unwrap();
        assert_eq!(record.state, SessionState::Dead);
    }

    #[test]
    fn test_mark_dead_nonexistent_is_noop() {
        let mut reg = SessionRegistry::new();
        // Should not panic
        reg.mark_dead("ghost");
    }

    #[test]
    fn test_multiple_agents() {
        let mut reg = SessionRegistry::new();
        reg.upsert("team-lead", "sess-1", 100);
        reg.upsert("arch-ctm", "sess-2", 200);
        reg.upsert("publisher", "sess-3", 300);

        assert_eq!(reg.len(), 3);
        assert_eq!(reg.query("arch-ctm").unwrap().process_id, 200);
    }

    #[test]
    fn test_new_session_registry_shared() {
        let shared = new_session_registry();
        {
            let mut reg = shared.lock().unwrap();
            reg.upsert("team-lead", "sess-a", 42);
        }
        let reg = shared.lock().unwrap();
        assert!(reg.query("team-lead").is_some());
    }

    /// Liveness check: the current process must be alive.
    #[cfg(unix)]
    #[test]
    fn test_is_pid_alive_current_process() {
        let pid = std::process::id();
        assert!(is_pid_alive(pid));
    }

    /// Liveness check: an impossible PID should be dead.
    #[cfg(unix)]
    #[test]
    fn test_is_pid_alive_nonexistent_pid() {
        // i32::MAX exceeds kernel PID range on Linux/macOS; kill() returns ESRCH.
        assert!(!is_pid_alive(i32::MAX as u32));
    }

    /// SessionRecord::is_process_alive uses the current process (always alive).
    #[cfg(unix)]
    #[test]
    fn test_record_is_process_alive_current() {
        let record = SessionRecord {
            session_id: "test".to_string(),
            process_id: std::process::id(),
            state: SessionState::Active,
        };
        assert!(record.is_process_alive());
    }
}
