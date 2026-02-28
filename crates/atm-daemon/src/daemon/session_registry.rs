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

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Lifecycle state of a tracked agent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    /// Session is believed to be running.
    Active,
    /// Session has been explicitly marked dead (e.g., process exited).
    Dead,
}

/// A single agent session record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Team name this session belongs to.
    pub team: String,
    /// Agent/member name in the team.
    pub agent_name: String,
    /// Claude Code session UUID (from `CLAUDE_SESSION_ID`).
    pub session_id: String,
    /// OS process ID of the agent process.
    pub process_id: u32,
    /// Current lifecycle state.
    pub state: SessionState,
    /// Last state update timestamp (RFC3339 UTC).
    pub updated_at: String,
    /// Runtime kind (e.g., `codex`, `gemini`) when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// Runtime-native session/thread identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_session_id: Option<String>,
    /// Runtime backend pane identifier when applicable (e.g., tmux `%42`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// Runtime home/state directory when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_home: Option<String>,
}

impl SessionRecord {
    /// Return `true` if the OS process is still alive.
    ///
    /// Uses `kill(pid, 0)` on Unix. Always returns `false` on non-Unix platforms.
    pub fn is_process_alive(&self) -> bool {
        is_pid_alive(self.process_id)
    }
}

/// Registry mapping team-scoped keys to their session records.
///
/// Wrap in `Arc<Mutex<SessionRegistry>>` for concurrent access.
#[derive(Debug, Default)]
pub struct SessionRegistry {
    sessions: HashMap<String, SessionRecord>,
    persist_path: Option<PathBuf>,
}

impl SessionRegistry {
    /// Create a new, empty registry.
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            persist_path: None,
        }
    }

    /// Create a registry that persists on every mutation.
    pub fn with_persist_path(persist_path: PathBuf) -> Self {
        Self {
            sessions: HashMap::new(),
            persist_path: Some(persist_path),
        }
    }

    /// Load a persisted registry from disk, or return an empty registry when
    /// the file is missing/corrupt.
    pub fn load_or_new(persist_path: PathBuf) -> Self {
        if let Some(sessions) = load_sessions_from_file(&persist_path) {
            Self {
                sessions,
                persist_path: Some(persist_path),
            }
        } else {
            Self::with_persist_path(persist_path)
        }
    }

    /// Insert or update the session record for `name`.
    ///
    /// If an entry already exists its `session_id`, `process_id`, and `state`
    /// are replaced. The state is reset to [`SessionState::Active`] on every
    /// upsert.
    pub fn upsert(&mut self, name: &str, session_id: &str, process_id: u32) {
        self.upsert_for_team("", name, session_id, process_id);
    }

    /// Insert or update the session record for `team/name`.
    pub fn upsert_for_team(&mut self, team: &str, name: &str, session_id: &str, process_id: u32) {
        let key = make_key(team, name);
        let existing = self.sessions.get(&key).cloned();
        let runtime = existing.as_ref().and_then(|r| r.runtime.clone());
        let runtime_session_id = existing.as_ref().and_then(|r| r.runtime_session_id.clone());
        let pane_id = existing.as_ref().and_then(|r| r.pane_id.clone());
        let runtime_home = existing.as_ref().and_then(|r| r.runtime_home.clone());
        self.upsert_runtime_for_team(
            team,
            name,
            session_id,
            process_id,
            runtime,
            runtime_session_id,
            pane_id,
            runtime_home,
        );
    }

    /// Insert or update the session record for `team/name` with runtime metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_runtime_for_team(
        &mut self,
        team: &str,
        name: &str,
        session_id: &str,
        process_id: u32,
        runtime: Option<String>,
        runtime_session_id: Option<String>,
        pane_id: Option<String>,
        runtime_home: Option<String>,
    ) {
        let now = chrono::Utc::now().to_rfc3339();
        self.sessions.insert(
            make_key(team, name),
            SessionRecord {
                team: team.to_string(),
                agent_name: name.to_string(),
                session_id: session_id.to_string(),
                process_id,
                state: SessionState::Active,
                updated_at: now,
                runtime,
                runtime_session_id,
                pane_id,
                runtime_home,
            },
        );
        self.persist_best_effort();
    }

    /// Return an immutable reference to the session record for `name`, or
    /// `None` if the agent is not registered.
    pub fn query(&self, name: &str) -> Option<&SessionRecord> {
        if let Some(r) = self.sessions.get(name) {
            return Some(r);
        }
        // Backward-compatible lookup by agent name if unique across teams.
        let mut iter = self
            .sessions
            .values()
            .filter(|r| r.agent_name == name || make_key(&r.team, &r.agent_name) == name);
        let first = iter.next()?;
        if iter.next().is_some() {
            return None;
        }
        Some(first)
    }

    /// Query a team-scoped session record.
    pub fn query_for_team(&self, team: &str, name: &str) -> Option<&SessionRecord> {
        self.sessions.get(&make_key(team, name))
    }

    /// Mark the session for `name` as [`SessionState::Dead`].
    ///
    /// Does nothing if the agent is not registered.
    pub fn mark_dead(&mut self, name: &str) {
        if let Some(record) = self.sessions.get_mut(name) {
            record.state = SessionState::Dead;
            self.persist_best_effort();
            return;
        }

        // Backward-compatible mark by unique agent name.
        let matches: Vec<String> = self
            .sessions
            .iter()
            .filter_map(|(k, r)| (r.agent_name == name).then_some(k.clone()))
            .collect();

        if matches.len() == 1
            && let Some(record) = self.sessions.get_mut(&matches[0])
        {
            record.state = SessionState::Dead;
            self.persist_best_effort();
        }
    }

    /// Mark a team-scoped session as dead.
    pub fn mark_dead_for_team(&mut self, team: &str, name: &str) {
        if let Some(record) = self.sessions.get_mut(&make_key(team, name)) {
            record.state = SessionState::Dead;
            record.updated_at = chrono::Utc::now().to_rfc3339();
            self.persist_best_effort();
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

    fn persist_best_effort(&self) {
        let Some(path) = self.persist_path.as_ref() else {
            return;
        };

        if let Err(e) = write_sessions_to_file(path, &self.sessions) {
            eprintln!(
                "[session-registry] warn: failed to persist {}: {e}",
                path.display()
            );
        }
    }
}

/// Shared, thread-safe session registry handle.
pub type SharedSessionRegistry = std::sync::Arc<std::sync::Mutex<SessionRegistry>>;

/// Create a new empty [`SharedSessionRegistry`].
pub fn new_session_registry() -> SharedSessionRegistry {
    #[cfg(test)]
    let registry = SessionRegistry::new();

    #[cfg(not(test))]
    let registry = match agent_team_mail_core::home::get_home_dir() {
        Ok(home) => {
            let path = home.join(".claude/daemon/session-registry.json");
            SessionRegistry::load_or_new(path)
        }
        Err(_) => SessionRegistry::new(),
    };
    std::sync::Arc::new(std::sync::Mutex::new(registry))
}

// ── Platform-specific liveness check ─────────────────────────────────────────

/// Check whether an OS process with the given PID is alive.
///
/// On Unix this uses `kill(pid, 0)` — a read-only existence probe that sends
/// no signal. On non-Unix platforms this always returns `false`.
pub fn is_pid_alive(pid: u32) -> bool {
    if pid <= 1 || pid > i32::MAX as u32 {
        return false;
    }
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
    let pid_t = pid as libc::pid_t;
    // SAFETY: kill with sig=0 never sends a signal; it only checks PID existence.
    let result = unsafe { libc::kill(pid_t, 0) };
    result == 0
}

fn load_sessions_from_file(path: &Path) -> Option<HashMap<String, SessionRecord>> {
    let content = std::fs::read_to_string(path).ok()?;
    let persisted: PersistedRegistry = serde_json::from_str(&content).ok()?;
    Some(
        persisted
            .sessions
            .into_iter()
            .map(|(k, mut v)| {
                if v.team.is_empty() || v.agent_name.is_empty() {
                    let (team, agent_name) = parse_key(&k);
                    if v.team.is_empty() {
                        v.team = team;
                    }
                    if v.agent_name.is_empty() {
                        v.agent_name = agent_name;
                    }
                }
                (make_key(&v.team, &v.agent_name), v)
            })
            .collect(),
    )
}

fn write_sessions_to_file(
    path: &Path,
    sessions: &HashMap<String, SessionRecord>,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let persisted = PersistedRegistry {
        sessions: sessions.clone(),
    };
    let serialized = serde_json::to_string_pretty(&persisted)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serialized)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedRegistry {
    sessions: HashMap<String, SessionRecord>,
}

fn make_key(team: &str, name: &str) -> String {
    format!("{team}::{name}")
}

fn parse_key(key: &str) -> (String, String) {
    match key.split_once("::") {
        Some((team, name)) => (team.to_string(), name.to_string()),
        None => ("".to_string(), key.to_string()),
    }
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

    #[test]
    fn test_persisted_registry_writes_and_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude/daemon/session-registry.json");

        let mut reg = SessionRegistry::with_persist_path(path.clone());
        reg.upsert("team-lead", "sess-a", 42);
        reg.mark_dead("team-lead");

        let loaded = SessionRegistry::load_or_new(path);
        let rec = loaded.query("team-lead").unwrap();
        assert_eq!(rec.session_id, "sess-a");
        assert_eq!(rec.process_id, 42);
        assert_eq!(rec.state, SessionState::Dead);
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
            team: "atm-dev".to_string(),
            agent_name: "team-lead".to_string(),
            session_id: "test".to_string(),
            process_id: std::process::id(),
            state: SessionState::Active,
            updated_at: chrono::Utc::now().to_rfc3339(),
            runtime: None,
            runtime_session_id: None,
            pane_id: None,
            runtime_home: None,
        };
        assert!(record.is_process_alive());
    }
}
