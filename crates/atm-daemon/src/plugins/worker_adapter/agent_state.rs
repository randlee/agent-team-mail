//! Agent turn-level state machine for Codex agents
//!
//! Tracks per-agent state at the turn level (Unknown/Active/Idle/Offline),
//! which is more granular than [`WorkerState`](super::lifecycle::WorkerState)
//! (process-level: Running/Crashed/Restarting/Idle).
//!
//! ## State Machine
//!
//! ```text
//!                 ┌──────────┐
//!                 │ Unknown │
//!                 └────┬─────┘
//!                      │ (first AfterAgent hook)
//!                      ▼
//!       ┌──────────────────────────┐
//!       │                          │
//!  nudge/send ──▶  Active            │
//!       │                          │
//!       │       AfterAgent         │
//!       │           │              │
//!       │           ▼              │
//!       │         Idle ────────────┘
//!       │           │
//!       │      (PID gone)
//!       │           │
//!       │           ▼
//!       │        Offline
//!       └──────────────────────────┘
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;
use tracing::debug;

/// Turn-level state of a Codex agent.
///
/// | State | Meaning | Safe to Nudge? |
/// |-------|---------|---------------|
/// | `Unknown` | Pane created, agent starting up | No |
/// | `Active` | Agent is processing a turn | No |
/// | `Idle` | Agent completed a turn (AfterAgent hook received) | Yes |
/// | `Offline` | Agent process has exited (PID gone) | No |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Pane created, agent starting up. Waiting for first AfterAgent hook.
    Unknown,
    /// Agent is processing a request (nudge sent or send-keys activity).
    Active,
    /// Agent completed a turn. Safe to send prompts.
    Idle,
    /// Agent process has exited (PID no longer running).
    Offline,
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => write!(f, "unknown"),
            Self::Active => write!(f, "active"),
            Self::Idle => write!(f, "idle"),
            Self::Offline => write!(f, "offline"),
        }
    }
}

impl AgentState {
    /// Returns `true` if it is safe to send a nudge to the agent.
    pub fn is_safe_to_nudge(self) -> bool {
        matches!(self, Self::Idle)
    }

    /// Returns `true` if the agent has permanently exited.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Offline)
    }
}

/// Pane and log file information for a running agent.
///
/// Stored in `AgentStateTracker` so the socket server can answer
/// `agent-pane` queries without direct access to worker handles.
#[derive(Debug, Clone)]
pub struct AgentPaneInfo {
    /// Backend pane identifier (e.g., tmux pane `"%42"`).
    pub pane_id: String,
    /// Absolute path to the agent's log file.
    pub log_path: PathBuf,
}

/// Human-readable transition metadata for troubleshooting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionMeta {
    pub reason: String,
    pub source: String,
}

/// Tracks per-agent turn-level state.
///
/// Thread-safe via external `Arc<Mutex<AgentStateTracker>>` wrapping.
pub struct AgentStateTracker {
    states: HashMap<String, AgentState>,
    last_transition: HashMap<String, Instant>,
    transition_meta: HashMap<String, TransitionMeta>,
    /// Pane and log path information per agent, stored for socket queries.
    pane_info: HashMap<String, AgentPaneInfo>,
}

impl AgentStateTracker {
    /// Create an empty tracker.
    pub fn new() -> Self {
        Self {
            states: HashMap::new(),
            last_transition: HashMap::new(),
            transition_meta: HashMap::new(),
            pane_info: HashMap::new(),
        }
    }

    /// Register a newly spawned agent in `Unknown` state.
    pub fn register_agent(&mut self, agent_id: &str) {
        self.set_state_inner(
            agent_id,
            AgentState::Unknown,
            "initial registration",
            "daemon",
        );
        debug!("Agent {agent_id} registered (state: Unknown)");
    }

    /// Remove an agent from tracking.
    pub fn unregister_agent(&mut self, agent_id: &str) {
        self.states.remove(agent_id);
        self.last_transition.remove(agent_id);
        self.transition_meta.remove(agent_id);
        self.pane_info.remove(agent_id);
        debug!("Agent {agent_id} unregistered from state tracker");
    }

    /// Transition an agent to a new state, logging the transition at DEBUG.
    pub fn set_state(&mut self, agent_id: &str, new_state: AgentState) {
        self.set_state_with_context(agent_id, new_state, "unspecified", "daemon");
    }

    /// Transition an agent with explicit reason/source metadata.
    pub fn set_state_with_context(
        &mut self,
        agent_id: &str,
        new_state: AgentState,
        reason: &str,
        source: &str,
    ) {
        let old = self.states.get(agent_id).copied();
        self.set_state_inner(agent_id, new_state, reason, source);
        match old {
            Some(old_state) => debug!(
                "Agent {agent_id}: {old_state} → {new_state} (reason={reason}, source={source})"
            ),
            None => {
                debug!("Agent {agent_id}: (new) → {new_state} (reason={reason}, source={source})")
            }
        }
    }

    fn set_state_inner(&mut self, agent_id: &str, state: AgentState, reason: &str, source: &str) {
        self.states.insert(agent_id.to_string(), state);
        self.last_transition
            .insert(agent_id.to_string(), Instant::now());
        self.transition_meta.insert(
            agent_id.to_string(),
            TransitionMeta {
                reason: reason.to_string(),
                source: source.to_string(),
            },
        );
    }

    /// Get the current state of an agent.
    pub fn get_state(&self, agent_id: &str) -> Option<AgentState> {
        self.states.get(agent_id).copied()
    }

    /// Get the duration since the last state transition for an agent.
    pub fn time_since_transition(&self, agent_id: &str) -> Option<std::time::Duration> {
        self.last_transition.get(agent_id).map(|t| t.elapsed())
    }

    /// Get transition metadata for an agent.
    pub fn transition_meta(&self, agent_id: &str) -> Option<&TransitionMeta> {
        self.transition_meta.get(agent_id)
    }

    /// Snapshot of all current agent states.
    pub fn all_states(&self) -> HashMap<String, AgentState> {
        self.states.clone()
    }

    /// Store pane and log file information for an agent.
    ///
    /// Called by the worker adapter after spawning a worker so that the socket
    /// server can answer `agent-pane` queries.
    ///
    /// # Arguments
    ///
    /// * `agent_id`  - Agent name (e.g., `"arch-ctm"`)
    /// * `pane_id`   - Backend pane identifier (e.g., `"%42"`)
    /// * `log_path`  - Absolute path to the agent's log file
    pub fn set_pane_info(&mut self, agent_id: &str, pane_id: &str, log_path: &std::path::Path) {
        self.pane_info.insert(
            agent_id.to_string(),
            AgentPaneInfo {
                pane_id: pane_id.to_string(),
                log_path: log_path.to_path_buf(),
            },
        );
        debug!(
            "Agent {agent_id} pane info stored: pane={pane_id} log={}",
            log_path.display()
        );
    }

    /// Retrieve pane and log file information for an agent.
    ///
    /// Returns `None` if the agent has not been registered or no pane info has
    /// been stored for it yet.
    pub fn get_pane_info(&self, agent_id: &str) -> Option<&AgentPaneInfo> {
        self.pane_info.get(agent_id)
    }
}

impl Default for AgentStateTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state_is_launching() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Unknown));
    }

    #[test]
    fn test_launching_to_idle_transition() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        tracker.set_state("arch-ctm", AgentState::Idle);
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Idle));
    }

    #[test]
    fn test_idle_to_busy_transition() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        tracker.set_state("arch-ctm", AgentState::Idle);
        tracker.set_state("arch-ctm", AgentState::Active);
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Active));
    }

    #[test]
    fn test_busy_to_idle_transition() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        tracker.set_state("arch-ctm", AgentState::Active);
        tracker.set_state("arch-ctm", AgentState::Idle);
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Idle));
    }

    #[test]
    fn test_idle_to_killed_transition() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        tracker.set_state("arch-ctm", AgentState::Idle);
        tracker.set_state("arch-ctm", AgentState::Offline);
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Offline));
    }

    #[test]
    fn test_full_lifecycle() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Unknown));

        // First AfterAgent hook → Idle
        tracker.set_state("arch-ctm", AgentState::Idle);
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Idle));
        assert!(tracker.get_state("arch-ctm").unwrap().is_safe_to_nudge());

        // Nudge sent → Active
        tracker.set_state("arch-ctm", AgentState::Active);
        assert!(!tracker.get_state("arch-ctm").unwrap().is_safe_to_nudge());

        // AfterAgent hook → Idle again
        tracker.set_state("arch-ctm", AgentState::Idle);

        // PID gone → Offline
        tracker.set_state("arch-ctm", AgentState::Offline);
        assert!(tracker.get_state("arch-ctm").unwrap().is_terminal());
        assert!(!tracker.get_state("arch-ctm").unwrap().is_safe_to_nudge());
    }

    #[test]
    fn test_unregister_removes_agent() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        assert!(tracker.get_state("arch-ctm").is_some());
        tracker.unregister_agent("arch-ctm");
        assert!(tracker.get_state("arch-ctm").is_none());
    }

    #[test]
    fn test_unregister_removes_pane_info() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        tracker.set_pane_info(
            "arch-ctm",
            "%42",
            &std::env::temp_dir().join("arch-ctm.log"),
        );
        assert!(tracker.get_pane_info("arch-ctm").is_some());
        tracker.unregister_agent("arch-ctm");
        assert!(tracker.get_pane_info("arch-ctm").is_none());
    }

    #[test]
    fn test_unknown_agent_returns_none() {
        let tracker = AgentStateTracker::new();
        assert!(tracker.get_state("unknown-agent").is_none());
    }

    #[test]
    fn test_all_states() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("agent-a");
        tracker.register_agent("agent-b");
        tracker.set_state("agent-b", AgentState::Idle);

        let states = tracker.all_states();
        assert_eq!(states.len(), 2);
        assert_eq!(states.get("agent-a"), Some(&AgentState::Unknown));
        assert_eq!(states.get("agent-b"), Some(&AgentState::Idle));
    }

    #[test]
    fn test_display() {
        assert_eq!(AgentState::Unknown.to_string(), "unknown");
        assert_eq!(AgentState::Active.to_string(), "active");
        assert_eq!(AgentState::Idle.to_string(), "idle");
        assert_eq!(AgentState::Offline.to_string(), "offline");
    }

    #[test]
    fn test_is_safe_to_nudge() {
        assert!(!AgentState::Unknown.is_safe_to_nudge());
        assert!(!AgentState::Active.is_safe_to_nudge());
        assert!(AgentState::Idle.is_safe_to_nudge());
        assert!(!AgentState::Offline.is_safe_to_nudge());
    }

    #[test]
    fn test_is_terminal() {
        assert!(!AgentState::Unknown.is_terminal());
        assert!(!AgentState::Active.is_terminal());
        assert!(!AgentState::Idle.is_terminal());
        assert!(AgentState::Offline.is_terminal());
    }

    #[test]
    fn test_time_since_transition() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        let elapsed = tracker.time_since_transition("arch-ctm");
        assert!(elapsed.is_some());
        assert!(elapsed.unwrap().as_secs() < 1);
    }

    // ── Pane info tests ───────────────────────────────────────────────────────

    #[test]
    fn test_pane_info_set_and_get() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        let log_path = std::env::temp_dir().join("arch-ctm.log");
        tracker.set_pane_info("arch-ctm", "%42", &log_path);

        let info = tracker
            .get_pane_info("arch-ctm")
            .expect("pane info should be set");
        assert_eq!(info.pane_id, "%42");
        assert_eq!(info.log_path, log_path);
    }

    #[test]
    fn test_pane_info_not_found() {
        let tracker = AgentStateTracker::new();
        assert!(tracker.get_pane_info("unregistered-agent").is_none());
    }

    #[test]
    fn test_pane_info_overwrite() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        let old_log = std::env::temp_dir().join("old.log");
        let new_log = std::env::temp_dir().join("new.log");
        tracker.set_pane_info("arch-ctm", "%10", &old_log);
        tracker.set_pane_info("arch-ctm", "%20", &new_log);

        let info = tracker.get_pane_info("arch-ctm").unwrap();
        assert_eq!(info.pane_id, "%20");
        assert_eq!(info.log_path, new_log);
    }
}
