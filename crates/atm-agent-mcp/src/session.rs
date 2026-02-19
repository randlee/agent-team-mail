//! In-memory session registry for active Codex agent sessions.
//!
//! [`SessionRegistry`] maintains a map of [`SessionEntry`] records keyed by
//! `agent_id` (format: `"codex:<uuid>"`). It enforces identity uniqueness
//! (one active session per identity) and a configurable maximum concurrency
//! limit.
//!
//! Persistence to disk is handled via [`RegistrySnapshot`]: callers call
//! [`SessionRegistry::to_snapshot`] to obtain a serializable snapshot and
//! write it themselves (Sprint A.5). On startup,
//! [`SessionRegistry::load_from_snapshot`] restores entries and immediately
//! marks them [`SessionStatus::Stale`] (FR-3.2).
//!
//! # Thread safety
//!
//! `SessionRegistry` is not `Send + Sync` itself. Callers wrap it in
//! `Arc<Mutex<SessionRegistry>>` at the call site (see [`crate::proxy`]).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Status of a single agent session (process-level lifecycle).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    /// Session is actively running in this proxy process.
    Active,
    /// Session existed in a prior proxy process and has not yet been resumed.
    Stale,
    /// Session has been explicitly closed.
    Closed,
}

/// Per-turn thread lifecycle state (FR-17).
///
/// Tracks whether a Codex thread is currently processing a turn (`Busy`),
/// waiting for the next command (`Idle`), or has been permanently closed
/// (`Closed`).  This is orthogonal to [`SessionStatus`], which tracks the
/// process-level session lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadState {
    /// A Codex turn is in progress.
    Busy,
    /// Turn complete, waiting for the next command.
    Idle,
    /// Thread removed from service — identity released.
    Closed,
}

/// Default [`ThreadState`] used by serde when the field is absent in older
/// snapshots.
fn default_thread_state() -> ThreadState {
    ThreadState::Busy
}

/// A single registered agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    /// Unique session identifier, format: `"codex:<uuid>"`.
    pub agent_id: String,
    /// ATM identity bound to this session.
    pub identity: String,
    /// ATM team this session belongs to.
    pub team: String,
    /// Codex threadId, populated once the child returns a response.
    pub thread_id: Option<String>,
    /// Working directory captured at session creation.
    pub cwd: String,
    /// Git repository root, or `None` when not in a git repository.
    pub repo_root: Option<String>,
    /// Derived repository name, or `None` when not in a git repository.
    pub repo_name: Option<String>,
    /// Current git branch, or `None` when not in a git repository.
    pub branch: Option<String>,
    /// ISO 8601 timestamp when the session was created.
    pub started_at: String,
    /// ISO 8601 timestamp of the most recent activity.
    pub last_active: String,
    /// Current lifecycle status.
    pub status: SessionStatus,
    /// Per-turn thread lifecycle state (FR-17).
    ///
    /// Defaults to [`ThreadState::Busy`] for new registrations (FR-17.2).
    /// Uses a serde default so that older registry snapshots without this
    /// field can be loaded without error.
    #[serde(default = "default_thread_state")]
    pub thread_state: ThreadState,
    /// Optional organizational label for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Path to the agent file used to create this session, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_source: Option<String>,
}

/// Errors produced by [`SessionRegistry`] operations.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// The requested identity is already bound to an active session.
    #[error("identity '{identity}' is already bound to active session '{agent_id}'")]
    IdentityConflict {
        /// The identity that caused the conflict.
        identity: String,
        /// The agent_id currently holding the identity.
        agent_id: String,
    },
    /// The maximum number of concurrent sessions has been reached.
    #[error("max concurrent sessions ({max}) reached")]
    MaxSessionsExceeded {
        /// The configured maximum.
        max: usize,
    },
}

/// Serializable snapshot of all sessions, used for disk persistence (FR-5.3).
///
/// Obtain one by calling [`SessionRegistry::to_snapshot`]. Restore with
/// [`SessionRegistry::load_from_snapshot`].
///
/// # Examples
///
/// ```
/// use atm_agent_mcp::session::{SessionRegistry, RegistrySnapshot};
///
/// let registry = SessionRegistry::new(10);
/// let snap = registry.to_snapshot();
/// let json = serde_json::to_string(&snap).unwrap();
/// let snap2: RegistrySnapshot = serde_json::from_str(&json).unwrap();
/// assert_eq!(snap2.sessions.len(), 0);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistrySnapshot {
    /// Schema version for forward compatibility.
    ///
    /// Version 1 is the current and only defined version.
    #[serde(default = "default_registry_version")]
    pub version: u32,
    /// All session entries captured at snapshot time.
    pub sessions: Vec<SessionEntry>,
}

/// Return the current registry snapshot schema version.
fn default_registry_version() -> u32 {
    1
}

/// In-memory registry of all agent sessions.
///
/// Wrap in `Arc<Mutex<SessionRegistry>>` when sharing across async tasks.
///
/// # Examples
///
/// ```
/// use atm_agent_mcp::session::SessionRegistry;
///
/// let mut registry = SessionRegistry::new(10);
/// let entry = registry.register(
///     "arch-ctm".to_string(),
///     "atm-dev".to_string(),
///     "/tmp".to_string(),
///     None, None, None,
/// ).unwrap();
/// assert_eq!(entry.identity, "arch-ctm");
/// assert_eq!(entry.status, atm_agent_mcp::session::SessionStatus::Active);
/// ```
#[derive(Debug)]
pub struct SessionRegistry {
    /// All sessions keyed by `agent_id`.
    sessions: HashMap<String, SessionEntry>,
    /// Maps active identity → agent_id (only `Active` sessions).
    identity_map: HashMap<String, String>,
    /// Upper bound on active (non-stale, non-closed) sessions.
    max_concurrent: usize,
}

impl SessionRegistry {
    /// Create a new empty registry with the given concurrency ceiling.
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            sessions: HashMap::new(),
            identity_map: HashMap::new(),
            max_concurrent,
        }
    }

    /// The configured maximum number of concurrent active sessions.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Number of sessions with [`SessionStatus::Active`] status.
    pub fn active_count(&self) -> usize {
        self.sessions
            .values()
            .filter(|e| e.status == SessionStatus::Active)
            .count()
    }

    /// Register a new session for `identity`.
    ///
    /// # Errors
    ///
    /// - [`RegistryError::IdentityConflict`] if `identity` already has an
    ///   active session.
    /// - [`RegistryError::MaxSessionsExceeded`] if `active_count() >= max_concurrent`.
    pub fn register(
        &mut self,
        identity: String,
        team: String,
        cwd: String,
        repo_root: Option<String>,
        repo_name: Option<String>,
        branch: Option<String>,
    ) -> Result<SessionEntry, RegistryError> {
        // Check identity conflict
        if let Some(existing_id) = self.identity_map.get(&identity) {
            return Err(RegistryError::IdentityConflict {
                identity,
                agent_id: existing_id.clone(),
            });
        }

        // Check concurrency limit
        if self.active_count() >= self.max_concurrent {
            return Err(RegistryError::MaxSessionsExceeded {
                max: self.max_concurrent,
            });
        }

        let agent_id = format!("codex:{}", Uuid::new_v4());
        let now = now_iso8601();

        let entry = SessionEntry {
            agent_id: agent_id.clone(),
            identity: identity.clone(),
            team,
            thread_id: None,
            cwd,
            repo_root,
            repo_name,
            branch,
            started_at: now.clone(),
            last_active: now,
            status: SessionStatus::Active,
            // New sessions start Busy (FR-17.2): the first codex call that
            // triggers registration is already in progress.
            thread_state: ThreadState::Busy,
            tag: None,
            agent_source: None,
        };

        self.sessions.insert(agent_id.clone(), entry.clone());
        self.identity_map.insert(identity, agent_id);

        Ok(entry)
    }

    /// Look up a session by `agent_id`.
    pub fn get(&self, agent_id: &str) -> Option<&SessionEntry> {
        self.sessions.get(agent_id)
    }

    /// Set the Codex `threadId` for a session.
    ///
    /// Does nothing if the `agent_id` is not found.
    pub fn set_thread_id(&mut self, agent_id: &str, thread_id: String) {
        if let Some(entry) = self.sessions.get_mut(agent_id) {
            entry.thread_id = Some(thread_id);
        }
    }

    /// Set the agent source file path for a session.
    ///
    /// Records the path to the agent file (`agent_file` argument) that was
    /// used to create this session, enabling callers to display a human-friendly
    /// agent name derived from the file stem.
    ///
    /// Does nothing if the `agent_id` is not found.
    pub fn set_agent_source(&mut self, agent_id: &str, source: String) {
        if let Some(entry) = self.sessions.get_mut(agent_id) {
            entry.agent_source = Some(source);
        }
    }

    /// Update the `last_active` timestamp and git context fields for a session.
    ///
    /// Does nothing if the `agent_id` is not found.
    pub fn touch(
        &mut self,
        agent_id: &str,
        repo_root: Option<String>,
        repo_name: Option<String>,
        branch: Option<String>,
    ) {
        if let Some(entry) = self.sessions.get_mut(agent_id) {
            entry.last_active = now_iso8601();
            entry.repo_root = repo_root;
            entry.repo_name = repo_name;
            entry.branch = branch;
        }
    }

    /// Close a session, setting its status to [`SessionStatus::Closed`] and
    /// its thread state to [`ThreadState::Closed`], releasing its identity for
    /// reuse.
    ///
    /// Does nothing if the `agent_id` is not found.
    pub fn close(&mut self, agent_id: &str) {
        if let Some(entry) = self.sessions.get_mut(agent_id) {
            entry.status = SessionStatus::Closed;
            entry.thread_state = ThreadState::Closed;
            self.identity_map.remove(&entry.identity.clone());
        }
    }

    /// Mark all currently [`SessionStatus::Active`] sessions as
    /// [`SessionStatus::Stale`] and clear the identity map.
    ///
    /// Called on startup when a persisted registry is loaded so that prior
    /// sessions cannot be confused with freshly started ones.
    pub fn mark_all_stale(&mut self) {
        for entry in self.sessions.values_mut() {
            if entry.status == SessionStatus::Active {
                entry.status = SessionStatus::Stale;
            }
        }
        self.identity_map.clear();
    }

    /// Attempt to resume a stale session by `agent_id`, optionally rebinding
    /// it to a new identity.
    ///
    /// Returns a reference to the updated [`SessionEntry`] on success, or
    /// `None` if the session does not exist or is not stale.
    pub fn resume_stale(&mut self, agent_id: &str, new_identity: String) -> Option<&SessionEntry> {
        let entry = self.sessions.get_mut(agent_id)?;
        if entry.status != SessionStatus::Stale {
            return None;
        }
        // Release old identity mapping if still present
        self.identity_map.remove(&entry.identity.clone());
        entry.identity = new_identity.clone();
        entry.status = SessionStatus::Active;
        // Resuming starts a new turn, so thread is busy again (FR-17).
        entry.thread_state = ThreadState::Busy;
        entry.last_active = now_iso8601();
        self.identity_map.insert(new_identity, agent_id.to_string());
        self.sessions.get(agent_id)
    }

    /// Insert a pre-built [`SessionEntry`] directly into the registry.
    ///
    /// This is used on startup to load persisted sessions from disk in their
    /// already-stale state (FR-3.2). The entry is stored as-is without
    /// checking concurrency limits or updating the identity map (stale sessions
    /// do not occupy identity slots).
    ///
    /// If a session with the same `agent_id` already exists it is overwritten.
    pub fn insert_stale(&mut self, entry: SessionEntry) {
        // Stale sessions do not occupy identity slots
        self.sessions.insert(entry.agent_id.clone(), entry);
    }

    /// Set the working directory for a session.
    ///
    /// Does nothing if the `agent_id` is not found.
    pub fn set_cwd(&mut self, agent_id: &str, cwd: String) {
        if let Some(entry) = self.sessions.get_mut(agent_id) {
            entry.cwd = cwd;
        }
    }

    /// Set the per-turn [`ThreadState`] for a session (FR-17).
    ///
    /// Does nothing if the `agent_id` is not found.
    pub fn set_thread_state(&mut self, agent_id: &str, state: ThreadState) {
        if let Some(entry) = self.sessions.get_mut(agent_id) {
            entry.thread_state = state;
        }
    }

    /// Return the current [`ThreadState`] for a session.
    ///
    /// Returns `None` if the `agent_id` is not found.
    pub fn get_thread_state(&self, agent_id: &str) -> Option<ThreadState> {
        self.sessions.get(agent_id).map(|e| e.thread_state.clone())
    }

    /// Return the `agent_id` currently bound to `identity` (active sessions only).
    ///
    /// Returns `None` if the identity is not active.
    pub fn find_by_identity(&self, identity: &str) -> Option<&str> {
        self.identity_map.get(identity).map(String::as_str)
    }

    /// List all sessions regardless of status.
    ///
    /// Order is unspecified.
    pub fn list_all(&self) -> Vec<&SessionEntry> {
        self.sessions.values().collect()
    }

    /// Export all sessions as a serializable snapshot.
    ///
    /// The caller is responsible for writing the snapshot to disk.
    /// This method is pure and synchronous — it holds no locks and performs
    /// no I/O.
    ///
    /// # Examples
    ///
    /// ```
    /// use atm_agent_mcp::session::SessionRegistry;
    ///
    /// let registry = SessionRegistry::new(10);
    /// let snap = registry.to_snapshot();
    /// assert!(snap.sessions.is_empty());
    /// ```
    pub fn to_snapshot(&self) -> RegistrySnapshot {
        RegistrySnapshot {
            version: 1,
            sessions: self.sessions.values().cloned().collect(),
        }
    }

    /// Restore a registry from a snapshot, marking all loaded sessions as
    /// [`SessionStatus::Stale`] (FR-3.2).
    ///
    /// Any `Active` entries in the snapshot are treated as stale because the
    /// proxy process that created them is no longer running. `Stale` and
    /// `Closed` entries are preserved as-is.
    ///
    /// If the snapshot is empty the registry is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use atm_agent_mcp::session::{SessionRegistry, RegistrySnapshot, SessionEntry, SessionStatus};
    ///
    /// let snap = RegistrySnapshot { version: 1, sessions: vec![] };
    /// let registry = SessionRegistry::load_from_snapshot(snap, 10);
    /// assert_eq!(registry.active_count(), 0);
    /// ```
    pub fn load_from_snapshot(snapshot: RegistrySnapshot, max_concurrent: usize) -> Self {
        let mut registry = Self::new(max_concurrent);
        for mut entry in snapshot.sessions {
            if entry.status == SessionStatus::Active {
                entry.status = SessionStatus::Stale;
            }
            // Stale and Closed sessions do not occupy identity map slots.
            registry.sessions.insert(entry.agent_id.clone(), entry);
        }
        registry
    }
}

/// Return the current UTC time formatted as a simplified ISO 8601 string.
///
/// Example: `"2026-02-18T12:34:56Z"`.
fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Convert seconds-since-epoch to a human-readable UTC datetime string.
    // We hand-roll this to avoid pulling in chrono for a single formatting call.
    epoch_secs_to_iso8601(secs)
}

/// Convert Unix epoch seconds to `"YYYY-MM-DDTHH:MM:SSZ"`.
///
/// Handles dates from 1970 through 2999.
fn epoch_secs_to_iso8601(secs: u64) -> String {
    // Days since epoch
    let total_days = secs / 86400;
    let time_of_day = secs % 86400;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    // Calendar calculation
    let (year, month, day) = days_to_ymd(total_days);

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert days-since-epoch (1970-01-01) to (year, month, day).
fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Shift epoch from 1970 to 2000 for simpler math (year 2000 = day 10957)
    // Algorithm: Gregorian calendar cycle is 400 years = 146097 days.
    days += 719468; // shift to a reference epoch of March 1, year 0
    let era = days / 146097;
    let doe = days % 146097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month (March=0, April=1, ..., Jan=10, Feb=11)
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Helper ─────────────────────────────────────────────────────────────

    fn make_registry(max: usize) -> SessionRegistry {
        SessionRegistry::new(max)
    }

    fn reg_entry(
        registry: &mut SessionRegistry,
        identity: &str,
    ) -> Result<SessionEntry, RegistryError> {
        registry.register(
            identity.to_string(),
            "atm-dev".to_string(),
            "/tmp".to_string(),
            None,
            None,
            None,
        )
    }

    // ─── Registration ────────────────────────────────────────────────────────

    #[test]
    fn register_new_session_succeeds() {
        let mut r = make_registry(10);
        let entry = reg_entry(&mut r, "arch-ctm").unwrap();
        assert_eq!(entry.identity, "arch-ctm");
        assert_eq!(entry.team, "atm-dev");
        assert!(entry.agent_id.starts_with("codex:"));
        assert_eq!(entry.status, SessionStatus::Active);
        assert_eq!(r.active_count(), 1);
    }

    #[test]
    fn register_duplicate_identity_fails_with_conflict() {
        let mut r = make_registry(10);
        reg_entry(&mut r, "arch-ctm").unwrap();
        let err = reg_entry(&mut r, "arch-ctm").unwrap_err();
        assert!(
            matches!(err, RegistryError::IdentityConflict { ref identity, .. } if identity == "arch-ctm")
        );
    }

    #[test]
    fn register_after_close_succeeds() {
        let mut r = make_registry(10);
        let first = reg_entry(&mut r, "arch-ctm").unwrap();
        r.close(&first.agent_id);
        // Identity released — re-registration should succeed
        let second = reg_entry(&mut r, "arch-ctm").unwrap();
        assert_ne!(first.agent_id, second.agent_id);
        assert_eq!(second.status, SessionStatus::Active);
    }

    #[test]
    fn max_concurrent_enforced() {
        let mut r = make_registry(2);
        reg_entry(&mut r, "agent-1").unwrap();
        reg_entry(&mut r, "agent-2").unwrap();
        let err = reg_entry(&mut r, "agent-3").unwrap_err();
        assert!(matches!(
            err,
            RegistryError::MaxSessionsExceeded { max: 2 }
        ));
    }

    // ─── Stale / resume ──────────────────────────────────────────────────────

    #[test]
    fn mark_all_stale_clears_identity_map() {
        let mut r = make_registry(10);
        reg_entry(&mut r, "agent-a").unwrap();
        reg_entry(&mut r, "agent-b").unwrap();
        r.mark_all_stale();
        assert_eq!(r.active_count(), 0);
        assert!(r.find_by_identity("agent-a").is_none());
        assert!(r.find_by_identity("agent-b").is_none());
        // Sessions still exist, just stale
        assert_eq!(r.list_all().len(), 2);
    }

    #[test]
    fn resume_stale_session_makes_it_active() {
        let mut r = make_registry(10);
        let entry = reg_entry(&mut r, "agent-a").unwrap();
        let id = entry.agent_id.clone();
        r.mark_all_stale();
        let resumed = r.resume_stale(&id, "agent-a-new".to_string());
        assert!(resumed.is_some());
        let resumed = resumed.unwrap();
        assert_eq!(resumed.status, SessionStatus::Active);
        assert_eq!(resumed.identity, "agent-a-new");
        assert_eq!(r.active_count(), 1);
    }

    #[test]
    fn resume_active_session_returns_none() {
        let mut r = make_registry(10);
        let entry = reg_entry(&mut r, "agent-a").unwrap();
        // Not stale — resume should fail
        let result = r.resume_stale(&entry.agent_id, "agent-a".to_string());
        assert!(result.is_none());
    }

    // ─── Close ───────────────────────────────────────────────────────────────

    #[test]
    fn close_session_removes_from_identity_map() {
        let mut r = make_registry(10);
        let entry = reg_entry(&mut r, "arch-ctm").unwrap();
        r.close(&entry.agent_id);
        assert!(r.find_by_identity("arch-ctm").is_none());
        // Session still exists in sessions map
        assert!(r.get(&entry.agent_id).is_some());
        assert_eq!(r.get(&entry.agent_id).unwrap().status, SessionStatus::Closed);
    }

    // ─── thread_id ───────────────────────────────────────────────────────────

    #[test]
    fn set_thread_id_updates_entry() {
        let mut r = make_registry(10);
        let entry = reg_entry(&mut r, "arch-ctm").unwrap();
        r.set_thread_id(&entry.agent_id, "thread-abc-123".to_string());
        let updated = r.get(&entry.agent_id).unwrap();
        assert_eq!(updated.thread_id, Some("thread-abc-123".to_string()));
    }

    // ─── list_all ────────────────────────────────────────────────────────────

    #[test]
    fn list_all_returns_all_sessions() {
        let mut r = make_registry(10);
        reg_entry(&mut r, "agent-a").unwrap();
        reg_entry(&mut r, "agent-b").unwrap();
        let all = r.list_all();
        assert_eq!(all.len(), 2);
    }

    // ─── find_by_identity ────────────────────────────────────────────────────

    #[test]
    fn find_by_identity_returns_agent_id() {
        let mut r = make_registry(10);
        let entry = reg_entry(&mut r, "arch-ctm").unwrap();
        let found = r.find_by_identity("arch-ctm");
        assert_eq!(found, Some(entry.agent_id.as_str()));
    }

    #[test]
    fn find_by_identity_missing_returns_none() {
        let r = make_registry(10);
        assert!(r.find_by_identity("nobody").is_none());
    }

    // ─── active_count ────────────────────────────────────────────────────────

    #[test]
    fn active_count_excludes_stale_and_closed() {
        let mut r = make_registry(10);
        let a = reg_entry(&mut r, "agent-a").unwrap();
        reg_entry(&mut r, "agent-b").unwrap();
        let c = reg_entry(&mut r, "agent-c").unwrap();

        r.mark_all_stale(); // all → stale, active_count = 0
        // Resume one
        r.resume_stale(&a.agent_id, "agent-a".to_string());
        // Register a new one (stale don't count against limit when active_count checked)
        let d = registry_with_stale_register(&mut r, "agent-d");
        r.close(&c.agent_id);

        // active = agent-a (resumed) + agent-d
        let _ = d;
        assert_eq!(r.active_count(), 2);
    }

    fn registry_with_stale_register(r: &mut SessionRegistry, identity: &str) -> SessionEntry {
        r.register(
            identity.to_string(),
            "atm-dev".to_string(),
            "/tmp".to_string(),
            None,
            None,
            None,
        )
        .unwrap()
    }

    // ─── insert_stale ─────────────────────────────────────────────────────────

    #[test]
    fn insert_stale_adds_entry_without_identity_slot() {
        let mut r = make_registry(10);
        let entry = crate::session::SessionEntry {
            agent_id: "codex:persisted-1234".to_string(),
            identity: "arch-ctm".to_string(),
            team: "atm-dev".to_string(),
            thread_id: None,
            cwd: "/tmp".to_string(),
            repo_root: None,
            repo_name: None,
            branch: None,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            last_active: "2026-01-01T00:00:00Z".to_string(),
            status: SessionStatus::Stale,
            thread_state: ThreadState::Idle,
            tag: None,
            agent_source: None,
        };
        r.insert_stale(entry);
        // Session is stored
        let found = r.get("codex:persisted-1234");
        assert!(found.is_some());
        assert_eq!(found.unwrap().status, SessionStatus::Stale);
        // But identity slot is NOT occupied (find_by_identity returns None)
        assert!(r.find_by_identity("arch-ctm").is_none());
        // Active count unaffected
        assert_eq!(r.active_count(), 0);
    }

    // ─── set_cwd ──────────────────────────────────────────────────────────────

    #[test]
    fn set_cwd_updates_entry() {
        let mut r = make_registry(10);
        let entry = reg_entry(&mut r, "arch-ctm").unwrap();
        r.set_cwd(&entry.agent_id, "/new/cwd".to_string());
        let updated = r.get(&entry.agent_id).unwrap();
        assert_eq!(updated.cwd, "/new/cwd");
    }

    #[test]
    fn set_cwd_nonexistent_agent_is_noop() {
        let mut r = make_registry(10);
        // Should not panic
        r.set_cwd("codex:no-such-agent", "/tmp".to_string());
    }

    // ─── RegistrySnapshot / to_snapshot / load_from_snapshot ────────────────

    #[test]
    fn to_snapshot_empty_registry() {
        let r = make_registry(10);
        let snap = r.to_snapshot();
        assert!(snap.sessions.is_empty());
    }

    #[test]
    fn to_snapshot_includes_all_sessions() {
        let mut r = make_registry(10);
        reg_entry(&mut r, "agent-a").unwrap();
        reg_entry(&mut r, "agent-b").unwrap();
        let snap = r.to_snapshot();
        assert_eq!(snap.sessions.len(), 2);
        let identities: Vec<&str> = snap.sessions.iter().map(|e| e.identity.as_str()).collect();
        assert!(identities.contains(&"agent-a"));
        assert!(identities.contains(&"agent-b"));
    }

    #[test]
    fn to_snapshot_includes_all_statuses() {
        let mut r = make_registry(10);
        let a = reg_entry(&mut r, "agent-a").unwrap();
        reg_entry(&mut r, "agent-b").unwrap();
        r.close(&a.agent_id);
        reg_entry(&mut r, "agent-c").unwrap();
        r.mark_all_stale();

        let snap = r.to_snapshot();
        assert_eq!(snap.sessions.len(), 3);
    }

    #[test]
    fn load_from_snapshot_empty_gives_empty_registry() {
        let snap = RegistrySnapshot { version: 1, sessions: vec![] };
        let r = SessionRegistry::load_from_snapshot(snap, 10);
        assert_eq!(r.active_count(), 0);
        assert!(r.list_all().is_empty());
    }

    #[test]
    fn load_from_snapshot_marks_active_as_stale() {
        let entry = SessionEntry {
            agent_id: "codex:snap-test-1".to_string(),
            identity: "arch-ctm".to_string(),
            team: "atm-dev".to_string(),
            thread_id: Some("thread-xyz".to_string()),
            cwd: "/tmp".to_string(),
            repo_root: None,
            repo_name: None,
            branch: None,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            last_active: "2026-01-01T00:00:00Z".to_string(),
            status: SessionStatus::Active,
            thread_state: ThreadState::Busy,
            tag: None,
            agent_source: None,
        };
        let snap = RegistrySnapshot {
            version: 1,
            sessions: vec![entry],
        };
        let r = SessionRegistry::load_from_snapshot(snap, 10);
        assert_eq!(r.active_count(), 0, "active sessions become stale on load");
        let found = r.get("codex:snap-test-1").expect("session must be present");
        assert_eq!(found.status, SessionStatus::Stale);
        // Identity map must NOT contain the stale session
        assert!(r.find_by_identity("arch-ctm").is_none());
    }

    #[test]
    fn load_from_snapshot_preserves_stale_and_closed() {
        let stale = SessionEntry {
            agent_id: "codex:stale-1".to_string(),
            identity: "stale-agent".to_string(),
            team: "atm-dev".to_string(),
            thread_id: None,
            cwd: "/tmp".to_string(),
            repo_root: None,
            repo_name: None,
            branch: None,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            last_active: "2026-01-01T00:00:00Z".to_string(),
            status: SessionStatus::Stale,
            thread_state: ThreadState::Idle,
            tag: None,
            agent_source: None,
        };
        let closed = SessionEntry {
            agent_id: "codex:closed-1".to_string(),
            identity: "closed-agent".to_string(),
            team: "atm-dev".to_string(),
            thread_id: None,
            cwd: "/tmp".to_string(),
            repo_root: None,
            repo_name: None,
            branch: None,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            last_active: "2026-01-01T00:00:00Z".to_string(),
            status: SessionStatus::Closed,
            thread_state: ThreadState::Closed,
            tag: None,
            agent_source: None,
        };
        let snap = RegistrySnapshot {
            version: 1,
            sessions: vec![stale, closed],
        };
        let r = SessionRegistry::load_from_snapshot(snap, 10);
        let s = r.get("codex:stale-1").unwrap();
        assert_eq!(s.status, SessionStatus::Stale);
        let c = r.get("codex:closed-1").unwrap();
        assert_eq!(c.status, SessionStatus::Closed);
    }

    #[test]
    fn round_trip_snapshot_serialize_deserialize() {
        let mut r = make_registry(10);
        let e = reg_entry(&mut r, "arch-ctm").unwrap();
        r.set_thread_id(&e.agent_id, "thread-rt-1".to_string());
        reg_entry(&mut r, "dev-agent").unwrap();

        let snap = r.to_snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let snap2: RegistrySnapshot = serde_json::from_str(&json).unwrap();
        let r2 = SessionRegistry::load_from_snapshot(snap2, 10);

        assert_eq!(r2.list_all().len(), 2);
        // All were Active, so all become Stale after load
        assert_eq!(r2.active_count(), 0);
        let loaded = r2.get(&e.agent_id).unwrap();
        assert_eq!(loaded.status, SessionStatus::Stale);
        assert_eq!(loaded.thread_id, Some("thread-rt-1".to_string()));
    }

    // ─── ThreadState / set_thread_state / get_thread_state ──────────────────

    #[test]
    fn new_session_has_busy_thread_state() {
        let mut r = make_registry(10);
        let entry = reg_entry(&mut r, "arch-ctm").unwrap();
        assert_eq!(
            r.get(&entry.agent_id).unwrap().thread_state,
            ThreadState::Busy,
            "newly registered sessions must start Busy (FR-17.2)"
        );
    }

    #[test]
    fn set_thread_state_updates_entry() {
        let mut r = make_registry(10);
        let entry = reg_entry(&mut r, "arch-ctm").unwrap();
        r.set_thread_state(&entry.agent_id, ThreadState::Idle);
        assert_eq!(
            r.get_thread_state(&entry.agent_id),
            Some(ThreadState::Idle)
        );
    }

    #[test]
    fn set_thread_state_nonexistent_is_noop() {
        let mut r = make_registry(10);
        // Should not panic
        r.set_thread_state("codex:no-such-agent", ThreadState::Idle);
    }

    #[test]
    fn get_thread_state_unknown_agent_returns_none() {
        let r = make_registry(10);
        assert!(r.get_thread_state("codex:ghost").is_none());
    }

    #[test]
    fn close_sets_thread_state_to_closed() {
        let mut r = make_registry(10);
        let entry = reg_entry(&mut r, "arch-ctm").unwrap();
        r.set_thread_state(&entry.agent_id, ThreadState::Idle);
        r.close(&entry.agent_id);
        assert_eq!(
            r.get_thread_state(&entry.agent_id),
            Some(ThreadState::Closed),
            "close() must set thread_state to Closed"
        );
    }

    #[test]
    fn resume_stale_sets_thread_state_to_busy() {
        let mut r = make_registry(10);
        let entry = reg_entry(&mut r, "agent-a").unwrap();
        let id = entry.agent_id.clone();
        // Make idle, then stale
        r.set_thread_state(&id, ThreadState::Idle);
        r.mark_all_stale();
        // Resume — should go back to Busy
        let resumed = r.resume_stale(&id, "agent-a".to_string()).unwrap();
        assert_eq!(
            resumed.thread_state,
            ThreadState::Busy,
            "resume_stale() must set thread_state to Busy"
        );
    }

    #[test]
    fn thread_state_serialization_round_trip() {
        // Verify that ThreadState serializes to lowercase strings matching serde attr
        let busy_json = serde_json::to_string(&ThreadState::Busy).unwrap();
        assert_eq!(busy_json, "\"busy\"");
        let idle_json = serde_json::to_string(&ThreadState::Idle).unwrap();
        assert_eq!(idle_json, "\"idle\"");
        let closed_json = serde_json::to_string(&ThreadState::Closed).unwrap();
        assert_eq!(closed_json, "\"closed\"");

        // Round-trip
        let state: ThreadState = serde_json::from_str("\"idle\"").unwrap();
        assert_eq!(state, ThreadState::Idle);
    }

    #[test]
    fn session_entry_thread_state_defaults_to_busy_when_deserializing_old_format() {
        // Simulate an older registry snapshot JSON without thread_state field
        let json = r#"{
            "agent_id": "codex:old-session",
            "identity": "legacy",
            "team": "atm-dev",
            "thread_id": null,
            "cwd": "/tmp",
            "repo_root": null,
            "repo_name": null,
            "branch": null,
            "started_at": "2026-01-01T00:00:00Z",
            "last_active": "2026-01-01T00:00:00Z",
            "status": "active"
        }"#;
        let entry: SessionEntry = serde_json::from_str(json).unwrap();
        assert_eq!(
            entry.thread_state,
            ThreadState::Busy,
            "missing thread_state field must default to Busy via serde default"
        );
    }

    // ─── epoch_secs_to_iso8601 ───────────────────────────────────────────────

    #[test]
    fn epoch_secs_zero_is_unix_epoch() {
        let s = epoch_secs_to_iso8601(0);
        assert_eq!(s, "1970-01-01T00:00:00Z");
    }

    #[test]
    fn now_iso8601_is_not_empty() {
        let s = now_iso8601();
        assert!(!s.is_empty());
        assert!(s.ends_with('Z'));
        assert!(s.contains('T'));
    }
}
