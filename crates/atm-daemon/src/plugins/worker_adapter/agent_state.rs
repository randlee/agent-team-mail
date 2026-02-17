//! Agent turn-level state machine for Codex agents
//!
//! Tracks per-agent state at the turn level (Launching/Busy/Idle/Killed),
//! which is more granular than [`WorkerState`](super::lifecycle::WorkerState)
//! (process-level: Running/Crashed/Restarting/Idle).
//!
//! ## State Machine
//!
//! ```text
//!                 ┌──────────┐
//!                 │ Launching │
//!                 └────┬─────┘
//!                      │ (first AfterAgent hook)
//!                      ▼
//!       ┌──────────────────────────┐
//!       │                          │
//!  nudge/send ──▶  Busy            │
//!       │                          │
//!       │       AfterAgent         │
//!       │           │              │
//!       │           ▼              │
//!       │         Idle ────────────┘
//!       │           │
//!       │      (PID gone)
//!       │           │
//!       │           ▼
//!       │        Killed
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
/// | `Launching` | Pane created, agent starting up | No |
/// | `Busy` | Agent is processing a turn | No |
/// | `Idle` | Agent completed a turn (AfterAgent hook received) | Yes |
/// | `Killed` | Agent process has exited (PID gone) | No |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Pane created, agent starting up. Waiting for first AfterAgent hook.
    Launching,
    /// Agent is processing a request (nudge sent or send-keys activity).
    Busy,
    /// Agent completed a turn. Safe to send prompts.
    Idle,
    /// Agent process has exited (PID no longer running).
    Killed,
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Launching => write!(f, "launching"),
            Self::Busy => write!(f, "busy"),
            Self::Idle => write!(f, "idle"),
            Self::Killed => write!(f, "killed"),
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
        matches!(self, Self::Killed)
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

/// Tracks per-agent turn-level state.
///
/// Thread-safe via external `Arc<Mutex<AgentStateTracker>>` wrapping.
pub struct AgentStateTracker {
    states: HashMap<String, AgentState>,
    last_transition: HashMap<String, Instant>,
    /// Pane and log path information per agent, stored for socket queries.
    pane_info: HashMap<String, AgentPaneInfo>,
}

impl AgentStateTracker {
    /// Create an empty tracker.
    pub fn new() -> Self {
        Self {
            states: HashMap::new(),
            last_transition: HashMap::new(),
            pane_info: HashMap::new(),
        }
    }

    /// Register a newly spawned agent in `Launching` state.
    pub fn register_agent(&mut self, agent_id: &str) {
        self.set_state_inner(agent_id, AgentState::Launching);
        debug!("Agent {agent_id} registered (state: Launching)");
    }

    /// Remove an agent from tracking.
    pub fn unregister_agent(&mut self, agent_id: &str) {
        self.states.remove(agent_id);
        self.last_transition.remove(agent_id);
        self.pane_info.remove(agent_id);
        debug!("Agent {agent_id} unregistered from state tracker");
    }

    /// Transition an agent to a new state, logging the transition at DEBUG.
    pub fn set_state(&mut self, agent_id: &str, new_state: AgentState) {
        let old = self.states.get(agent_id).copied();
        self.set_state_inner(agent_id, new_state);
        match old {
            Some(old_state) => debug!("Agent {agent_id}: {old_state} → {new_state}"),
            None => debug!("Agent {agent_id}: (new) → {new_state}"),
        }
    }

    fn set_state_inner(&mut self, agent_id: &str, state: AgentState) {
        self.states.insert(agent_id.to_string(), state);
        self.last_transition.insert(agent_id.to_string(), Instant::now());
    }

    /// Get the current state of an agent.
    pub fn get_state(&self, agent_id: &str) -> Option<AgentState> {
        self.states.get(agent_id).copied()
    }

    /// Get the duration since the last state transition for an agent.
    pub fn time_since_transition(&self, agent_id: &str) -> Option<std::time::Duration> {
        self.last_transition.get(agent_id).map(|t| t.elapsed())
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
        debug!("Agent {agent_id} pane info stored: pane={pane_id} log={}", log_path.display());
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
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Launching));
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
        tracker.set_state("arch-ctm", AgentState::Busy);
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Busy));
    }

    #[test]
    fn test_busy_to_idle_transition() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        tracker.set_state("arch-ctm", AgentState::Busy);
        tracker.set_state("arch-ctm", AgentState::Idle);
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Idle));
    }

    #[test]
    fn test_idle_to_killed_transition() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        tracker.set_state("arch-ctm", AgentState::Idle);
        tracker.set_state("arch-ctm", AgentState::Killed);
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Killed));
    }

    #[test]
    fn test_full_lifecycle() {
        let mut tracker = AgentStateTracker::new();
        tracker.register_agent("arch-ctm");
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Launching));

        // First AfterAgent hook → Idle
        tracker.set_state("arch-ctm", AgentState::Idle);
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Idle));
        assert!(tracker.get_state("arch-ctm").unwrap().is_safe_to_nudge());

        // Nudge sent → Busy
        tracker.set_state("arch-ctm", AgentState::Busy);
        assert!(!tracker.get_state("arch-ctm").unwrap().is_safe_to_nudge());

        // AfterAgent hook → Idle again
        tracker.set_state("arch-ctm", AgentState::Idle);

        // PID gone → Killed
        tracker.set_state("arch-ctm", AgentState::Killed);
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
        tracker.set_pane_info("arch-ctm", "%42", std::path::Path::new("/tmp/arch-ctm.log"));
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
        assert_eq!(states.get("agent-a"), Some(&AgentState::Launching));
        assert_eq!(states.get("agent-b"), Some(&AgentState::Idle));
    }

    #[test]
    fn test_display() {
        assert_eq!(AgentState::Launching.to_string(), "launching");
        assert_eq!(AgentState::Busy.to_string(), "busy");
        assert_eq!(AgentState::Idle.to_string(), "idle");
        assert_eq!(AgentState::Killed.to_string(), "killed");
    }

    #[test]
    fn test_is_safe_to_nudge() {
        assert!(!AgentState::Launching.is_safe_to_nudge());
        assert!(!AgentState::Busy.is_safe_to_nudge());
        assert!(AgentState::Idle.is_safe_to_nudge());
        assert!(!AgentState::Killed.is_safe_to_nudge());
    }

    #[test]
    fn test_is_terminal() {
        assert!(!AgentState::Launching.is_terminal());
        assert!(!AgentState::Busy.is_terminal());
        assert!(!AgentState::Idle.is_terminal());
        assert!(AgentState::Killed.is_terminal());
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
        tracker.set_pane_info("arch-ctm", "%42", std::path::Path::new("/tmp/arch-ctm.log"));

        let info = tracker.get_pane_info("arch-ctm").expect("pane info should be set");
        assert_eq!(info.pane_id, "%42");
        assert_eq!(info.log_path, std::path::PathBuf::from("/tmp/arch-ctm.log"));
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
        tracker.set_pane_info("arch-ctm", "%10", std::path::Path::new("/tmp/old.log"));
        tracker.set_pane_info("arch-ctm", "%20", std::path::Path::new("/tmp/new.log"));

        let info = tracker.get_pane_info("arch-ctm").unwrap();
        assert_eq!(info.pane_id, "%20");
        assert_eq!(info.log_path, std::path::PathBuf::from("/tmp/new.log"));
    }
}
