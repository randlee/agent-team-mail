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
use std::time::Duration;

/// Lifecycle state of a tracked agent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    /// Session is believed to be running.
    Active,
    /// Session has been explicitly marked dead (e.g., process exited).
    Dead,
}

/// Result of attempting a session-scoped dead-mark operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkDeadForSessionOutcome {
    /// Matching active session was marked dead.
    MarkedDead,
    /// Matching session was already dead (idempotent replay).
    AlreadyDead,
    /// No tracked session exists for the target team/member.
    UnknownSession,
    /// Team/member exists but session IDs do not match.
    SessionMismatch { current_session_id: String },
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
    /// Most recent successful daemon-side heartbeat (resolve/send) for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    /// Most recent timestamp where daemon liveness probe confirmed process alive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_alive_at: Option<String>,
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

#[derive(Debug, Clone)]
struct LivenessCacheEntry {
    alive: bool,
    checked_at: chrono::DateTime<chrono::Utc>,
}

/// Registry mapping team-scoped keys to their session records.
///
/// Wrap in `Arc<Mutex<SessionRegistry>>` for concurrent access.
#[derive(Debug, Default)]
pub struct SessionRegistry {
    sessions: HashMap<String, SessionRecord>,
    persist_path: Option<PathBuf>,
    liveness_cache: HashMap<u32, LivenessCacheEntry>,
}

impl SessionRegistry {
    /// Time-to-live for cached PID liveness probe results.
    pub(crate) const PID_LIVENESS_TTL: Duration = Duration::from_secs(5);

    /// Create a new, empty registry.
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            persist_path: None,
            liveness_cache: HashMap::new(),
        }
    }

    /// Create a registry that persists on every mutation.
    pub fn with_persist_path(persist_path: PathBuf) -> Self {
        Self {
            sessions: HashMap::new(),
            persist_path: Some(persist_path),
            liveness_cache: HashMap::new(),
        }
    }

    /// Load a persisted registry from disk, or return an empty registry when
    /// the file is missing/corrupt.
    pub fn load_or_new(persist_path: PathBuf) -> Self {
        if let Some(sessions) = load_sessions_from_file(&persist_path) {
            Self {
                sessions,
                persist_path: Some(persist_path),
                liveness_cache: HashMap::new(),
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
        let now_dt = chrono::Utc::now();
        let now = now_dt.to_rfc3339();
        let pid_alive = process_id > 1 && is_pid_alive(process_id);
        self.sessions.insert(
            make_key(team, name),
            SessionRecord {
                team: team.to_string(),
                agent_name: name.to_string(),
                session_id: session_id.to_string(),
                process_id,
                state: SessionState::Active,
                updated_at: now,
                last_seen_at: Some(now_dt.to_rfc3339()),
                last_alive_at: pid_alive.then(|| now_dt.to_rfc3339()),
                runtime,
                runtime_session_id,
                pane_id,
                runtime_home,
            },
        );
        if process_id > 1 {
            self.liveness_cache.insert(
                process_id,
                LivenessCacheEntry {
                    alive: pid_alive,
                    checked_at: now_dt,
                },
            );
        }
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

    /// Query by agent name and reconcile liveness through the bounded cache.
    pub fn query_with_liveness(&mut self, name: &str) -> Option<SessionRecord> {
        if self.sessions.contains_key(name) {
            return self.refresh_record_and_clone(name);
        }
        // Backward-compatible lookup by unique agent name.
        let matches: Vec<String> = self
            .sessions
            .iter()
            .filter_map(|(k, r)| {
                (r.agent_name == name || make_key(&r.team, &r.agent_name) == name)
                    .then_some(k.clone())
            })
            .collect();
        if matches.len() != 1 {
            return None;
        }
        self.refresh_record_and_clone(&matches[0])
    }

    /// Team-scoped query with liveness reconciliation.
    pub fn query_for_team_with_liveness(
        &mut self,
        team: &str,
        name: &str,
    ) -> Option<SessionRecord> {
        self.refresh_record_and_clone(&make_key(team, name))
    }

    /// Mark the session for `name` as [`SessionState::Dead`].
    ///
    /// Does nothing if the agent is not registered.
    pub fn mark_dead(&mut self, name: &str) {
        if let Some(record) = self.sessions.get_mut(name) {
            record.state = SessionState::Dead;
            self.liveness_cache.remove(&record.process_id);
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
            self.liveness_cache.remove(&record.process_id);
            self.persist_best_effort();
        }
    }

    /// Mark a team-scoped session as dead.
    pub fn mark_dead_for_team(&mut self, team: &str, name: &str) {
        if let Some(record) = self.sessions.get_mut(&make_key(team, name)) {
            record.state = SessionState::Dead;
            record.updated_at = chrono::Utc::now().to_rfc3339();
            self.liveness_cache.remove(&record.process_id);
            self.persist_best_effort();
        }
    }

    /// Mark a team-scoped session as dead only when `session_id` matches.
    pub fn mark_dead_for_team_session(
        &mut self,
        team: &str,
        name: &str,
        session_id: &str,
    ) -> MarkDeadForSessionOutcome {
        let Some(record) = self.sessions.get_mut(&make_key(team, name)) else {
            return MarkDeadForSessionOutcome::UnknownSession;
        };

        if record.session_id != session_id {
            return MarkDeadForSessionOutcome::SessionMismatch {
                current_session_id: record.session_id.clone(),
            };
        }

        if record.state == SessionState::Dead {
            return MarkDeadForSessionOutcome::AlreadyDead;
        }

        record.state = SessionState::Dead;
        record.updated_at = chrono::Utc::now().to_rfc3339();
        self.liveness_cache.remove(&record.process_id);
        self.persist_best_effort();
        MarkDeadForSessionOutcome::MarkedDead
    }

    /// Remove a team-scoped session record.
    pub fn remove_for_team(&mut self, team: &str, name: &str) {
        if let Some(record) = self.sessions.remove(&make_key(team, name)) {
            self.liveness_cache.remove(&record.process_id);
            self.persist_best_effort();
        }
    }

    /// Return all tracked session records for a team.
    pub fn sessions_for_team(&self, team: &str) -> Vec<SessionRecord> {
        self.sessions
            .values()
            .filter(|record| record.team == team)
            .cloned()
            .collect()
    }

    /// Return all tracked session records for a team after bounded liveness
    /// reconciliation.
    pub fn sessions_for_team_with_liveness(&mut self, team: &str) -> Vec<SessionRecord> {
        let keys: Vec<String> = self
            .sessions
            .iter()
            .filter_map(|(k, v)| (v.team == team).then_some(k.clone()))
            .collect();
        let mut changed = false;
        for key in &keys {
            if self.refresh_record_liveness(key) {
                changed = true;
            }
        }
        if changed {
            self.persist_best_effort();
        }
        keys.into_iter()
            .filter_map(|k| self.sessions.get(&k).cloned())
            .collect()
    }

    /// Return all tracked agent names for a team.
    pub fn agent_names_for_team(&self, team: &str) -> Vec<String> {
        self.sessions_for_team(team)
            .into_iter()
            .map(|record| record.agent_name)
            .collect()
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

    fn refresh_record_and_clone(&mut self, key: &str) -> Option<SessionRecord> {
        if !self.sessions.contains_key(key) {
            return None;
        }
        let changed = self.refresh_record_liveness(key);
        if changed {
            self.persist_best_effort();
        }
        self.sessions.get(key).cloned()
    }

    fn refresh_record_liveness(&mut self, key: &str) -> bool {
        let now = chrono::Utc::now();
        let Some(existing) = self.sessions.get(key) else {
            return false;
        };
        let process_id = existing.process_id;
        let state = existing.state.clone();

        if process_id <= 1 {
            let Some(record) = self.sessions.get_mut(key) else {
                return false;
            };
            if record.state != SessionState::Dead {
                record.state = SessionState::Dead;
                record.updated_at = now.to_rfc3339();
                return true;
            }
            return false;
        }

        let (alive, probed) = self.pid_alive_cached(process_id, now);
        let mut changed = false;
        if state == SessionState::Active {
            let Some(record) = self.sessions.get_mut(key) else {
                return false;
            };
            if alive {
                record.last_seen_at = Some(now.to_rfc3339());
                changed = true;
                if probed || record.last_alive_at.is_none() {
                    record.last_alive_at = Some(now.to_rfc3339());
                    changed = true;
                }
            } else {
                record.state = SessionState::Dead;
                record.updated_at = now.to_rfc3339();
                changed = true;
            }
        }
        changed
    }

    fn pid_alive_cached(&mut self, pid: u32, now: chrono::DateTime<chrono::Utc>) -> (bool, bool) {
        if let Some(entry) = self.liveness_cache.get(&pid) {
            if now
                .signed_duration_since(entry.checked_at)
                .to_std()
                .is_ok_and(|age| age < Self::PID_LIVENESS_TTL)
            {
                return (entry.alive, false);
            }
        }
        let alive = is_pid_alive(pid);
        self.liveness_cache.insert(
            pid,
            LivenessCacheEntry {
                alive,
                checked_at: now,
            },
        );
        (alive, true)
    }

    /// Record a successful daemon-side heartbeat for `team/name`.
    ///
    /// Returns `true` when the record exists and was updated.
    pub fn heartbeat_for_team(&mut self, team: &str, name: &str) -> bool {
        let key = make_key(team, name);
        let Some(record) = self.sessions.get_mut(&key) else {
            return false;
        };
        let now = chrono::Utc::now().to_rfc3339();
        record.last_seen_at = Some(now.clone());
        record.updated_at = now;
        self.persist_best_effort();
        true
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
/// Delegates to `atm_core::pid::is_pid_alive` which provides cross-platform
/// support (Unix via `kill(pid, 0)`, Windows via `OpenProcess`).
pub fn is_pid_alive(pid: u32) -> bool {
    if pid <= 1 || pid > i32::MAX as u32 {
        return false;
    }
    agent_team_mail_core::pid::is_pid_alive(pid)
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
    fn test_mark_dead_for_team_session_marks_matching_active_record_dead() {
        let mut reg = SessionRegistry::new();
        reg.upsert_for_team("atm-dev", "arch-ctm", "sess-1", 1234);

        let outcome = reg.mark_dead_for_team_session("atm-dev", "arch-ctm", "sess-1");
        assert_eq!(outcome, MarkDeadForSessionOutcome::MarkedDead);
        assert_eq!(
            reg.query_for_team("atm-dev", "arch-ctm").unwrap().state,
            SessionState::Dead
        );
    }

    #[test]
    fn test_mark_dead_for_team_session_returns_already_dead_for_duplicate_replay() {
        let mut reg = SessionRegistry::new();
        reg.upsert_for_team("atm-dev", "arch-ctm", "sess-1", 1234);
        reg.mark_dead_for_team("atm-dev", "arch-ctm");

        let outcome = reg.mark_dead_for_team_session("atm-dev", "arch-ctm", "sess-1");
        assert_eq!(outcome, MarkDeadForSessionOutcome::AlreadyDead);
        assert_eq!(
            reg.query_for_team("atm-dev", "arch-ctm").unwrap().state,
            SessionState::Dead
        );
    }

    #[test]
    fn test_mark_dead_for_team_session_returns_unknown_when_member_not_registered() {
        let mut reg = SessionRegistry::new();
        let outcome = reg.mark_dead_for_team_session("atm-dev", "arch-ctm", "sess-1");
        assert_eq!(outcome, MarkDeadForSessionOutcome::UnknownSession);
    }

    #[test]
    fn test_mark_dead_for_team_session_returns_mismatch_without_state_change() {
        let mut reg = SessionRegistry::new();
        reg.upsert_for_team("atm-dev", "arch-ctm", "sess-current", 1234);

        let outcome = reg.mark_dead_for_team_session("atm-dev", "arch-ctm", "sess-other");
        assert_eq!(
            outcome,
            MarkDeadForSessionOutcome::SessionMismatch {
                current_session_id: "sess-current".to_string()
            }
        );
        assert_eq!(
            reg.query_for_team("atm-dev", "arch-ctm").unwrap().state,
            SessionState::Active
        );
    }

    #[test]
    fn test_remove_for_team_deletes_only_target_member() {
        let mut reg = SessionRegistry::new();
        reg.upsert_for_team("atm-dev", "arch-ctm", "sess-a", 100);
        reg.upsert_for_team("atm-dev", "arch-gtm", "sess-b", 101);

        reg.remove_for_team("atm-dev", "arch-ctm");
        assert!(reg.query_for_team("atm-dev", "arch-ctm").is_none());
        assert!(reg.query_for_team("atm-dev", "arch-gtm").is_some());
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
    fn test_sessions_for_team_and_agent_names_are_team_scoped() {
        let mut reg = SessionRegistry::new();
        reg.upsert_for_team("atm-dev", "arch-ctm", "sess-1", 111);
        reg.upsert_for_team("atm-dev", "arch-gtm", "sess-2", 222);
        reg.upsert_for_team("other-team", "researcher", "sess-3", 333);

        let mut sessions = reg.sessions_for_team("atm-dev");
        sessions.sort_by(|a, b| a.agent_name.cmp(&b.agent_name));
        let names_from_sessions: Vec<String> =
            sessions.iter().map(|s| s.agent_name.clone()).collect();
        assert_eq!(
            names_from_sessions,
            vec!["arch-ctm".to_string(), "arch-gtm".to_string()]
        );

        let mut names = reg.agent_names_for_team("atm-dev");
        names.sort();
        assert_eq!(names, vec!["arch-ctm".to_string(), "arch-gtm".to_string()]);
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

    #[test]
    fn test_stale_cleanup_selection_preserves_active_and_removes_only_stale_records() {
        let mut reg = SessionRegistry::new();
        let active_pid = std::process::id();
        let stale_pid = i32::MAX as u32;
        reg.upsert_for_team("atm-dev", "active-member", "sess-active", active_pid);
        reg.upsert_for_team("atm-dev", "stale-member", "sess-stale", stale_pid);

        // Force stale PID cache to expire so cleanup selection re-probes liveness.
        if let Some(entry) = reg.liveness_cache.get_mut(&stale_pid) {
            entry.checked_at = chrono::Utc::now()
                - chrono::Duration::from_std(SessionRegistry::PID_LIVENESS_TTL).unwrap()
                - chrono::Duration::milliseconds(10);
        }

        // Simulate cleanup selection logic: remove only sessions that converge to Dead.
        let sessions = reg.sessions_for_team_with_liveness("atm-dev");
        for session in sessions
            .into_iter()
            .filter(|session| session.state == SessionState::Dead)
        {
            reg.remove_for_team("atm-dev", &session.agent_name);
        }

        assert!(
            reg.query_for_team("atm-dev", "active-member").is_some(),
            "cleanup must preserve active/living sessions"
        );
        assert!(
            reg.query_for_team("atm-dev", "stale-member").is_none(),
            "cleanup must remove stale dead sessions only"
        );
    }

    #[test]
    fn test_load_or_new_upsert_same_team_agent_replaces_without_duplication() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude/daemon/session-registry.json");

        let mut initial = SessionRegistry::with_persist_path(path.clone());
        initial.upsert_for_team("atm-dev", "arch-ctm", "sess-initial", 111);
        drop(initial);

        let mut loaded = SessionRegistry::load_or_new(path);
        loaded.upsert_for_team("atm-dev", "arch-ctm", "sess-restarted", 222);

        let members = loaded.sessions_for_team("atm-dev");
        assert_eq!(
            members.len(),
            1,
            "reload + upsert for same (team,agent) must not duplicate rows"
        );
        let record = loaded
            .query_for_team("atm-dev", "arch-ctm")
            .expect("member should remain queryable");
        assert_eq!(record.session_id, "sess-restarted");
        assert_eq!(record.process_id, 222);
        assert_eq!(record.state, SessionState::Active);
    }

    #[test]
    fn test_query_for_team_with_liveness_marks_dead_when_pid_is_not_alive() {
        let mut reg = SessionRegistry::new();
        let pid = i32::MAX as u32;
        reg.upsert_for_team("atm-dev", "arch-ctm", "sess-dead", pid);
        if let Some(entry) = reg.liveness_cache.get_mut(&pid) {
            entry.checked_at = chrono::Utc::now()
                - chrono::Duration::from_std(SessionRegistry::PID_LIVENESS_TTL).unwrap()
                - chrono::Duration::milliseconds(10);
        }
        let refreshed = reg
            .query_for_team_with_liveness("atm-dev", "arch-ctm")
            .expect("session should exist");
        assert_eq!(refreshed.state, SessionState::Dead);
    }

    #[test]
    fn test_query_for_team_with_liveness_updates_last_alive_timestamp() {
        let mut reg = SessionRegistry::new();
        let pid = std::process::id();
        reg.upsert_for_team("atm-dev", "team-lead", "sess-live", pid);
        if let Some(record) = reg.sessions.get_mut("atm-dev::team-lead") {
            record.last_seen_at = None;
            record.last_alive_at = None;
        }
        let refreshed = reg
            .query_for_team_with_liveness("atm-dev", "team-lead")
            .expect("session should exist");
        assert_eq!(refreshed.state, SessionState::Active);
        assert!(
            refreshed.last_seen_at.is_some(),
            "active liveness query should refresh last_seen_at heartbeat"
        );
        assert!(
            refreshed.last_alive_at.is_some(),
            "active liveness probe should refresh last_alive_at"
        );
    }

    #[test]
    fn test_heartbeat_for_team_updates_last_seen_and_updated_at() {
        let mut reg = SessionRegistry::new();
        reg.upsert_for_team("atm-dev", "arch-ctm", "sess-live", std::process::id());
        let before = reg
            .query_for_team("atm-dev", "arch-ctm")
            .expect("record should exist")
            .updated_at
            .clone();

        std::thread::sleep(std::time::Duration::from_millis(1));
        assert!(
            reg.heartbeat_for_team("atm-dev", "arch-ctm"),
            "heartbeat should update existing record"
        );
        let after = reg
            .query_for_team("atm-dev", "arch-ctm")
            .expect("record should exist");
        assert!(
            after.last_seen_at.is_some(),
            "heartbeat must set last_seen_at"
        );
        assert_ne!(after.updated_at, before, "updated_at must advance");
    }

    #[test]
    fn test_liveness_cache_reprobe_after_ttl_reclassifies_dead() {
        let mut reg = SessionRegistry::new();
        let pid = i32::MAX as u32;
        reg.upsert_for_team("atm-dev", "arch-ctm", "sess-stale", pid);
        // Force a fresh-but-incorrect cached alive result to verify bounded stale windows.
        reg.liveness_cache.insert(
            pid,
            LivenessCacheEntry {
                alive: true,
                checked_at: chrono::Utc::now(),
            },
        );

        let first = reg
            .query_for_team_with_liveness("atm-dev", "arch-ctm")
            .expect("session should exist");
        assert_eq!(
            first.state,
            SessionState::Active,
            "fresh cache entry should be honored within TTL"
        );

        if let Some(entry) = reg.liveness_cache.get_mut(&pid) {
            entry.checked_at = chrono::Utc::now()
                - chrono::Duration::from_std(SessionRegistry::PID_LIVENESS_TTL).unwrap()
                - chrono::Duration::milliseconds(10);
        }

        let second = reg
            .query_for_team_with_liveness("atm-dev", "arch-ctm")
            .expect("session should exist");
        assert_eq!(
            second.state,
            SessionState::Dead,
            "expired cache entry must trigger re-probe and dead convergence"
        );
    }

    /// Liveness check: the current process must be alive.
    #[test]
    fn test_is_pid_alive_current_process() {
        let pid = std::process::id();
        assert!(is_pid_alive(pid));
    }

    /// Liveness check: an impossible PID should be dead.
    #[test]
    fn test_is_pid_alive_nonexistent_pid() {
        assert!(!is_pid_alive(i32::MAX as u32));
    }

    /// SessionRecord::is_process_alive uses the current process (always alive).
    #[test]
    fn test_record_is_process_alive_current() {
        let record = SessionRecord {
            team: "atm-dev".to_string(),
            agent_name: "team-lead".to_string(),
            session_id: "test".to_string(),
            process_id: std::process::id(),
            state: SessionState::Active,
            updated_at: chrono::Utc::now().to_rfc3339(),
            last_seen_at: Some(chrono::Utc::now().to_rfc3339()),
            last_alive_at: Some(chrono::Utc::now().to_rfc3339()),
            runtime: None,
            runtime_session_id: None,
            pane_id: None,
            runtime_home: None,
        };
        assert!(record.is_process_alive());
    }
}
