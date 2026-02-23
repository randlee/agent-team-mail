//! Normalized daemon stream event types for the `atm-agent-mcp → daemon → TUI` pipeline.
//!
//! This module defines the wire types for streaming turn-level events from any
//! transport (MCP, cli-json, app-server) through the daemon to the TUI.
//!
//! # Architecture
//!
//! ```text
//! atm-agent-mcp (3 transports)
//!   └── emit DaemonStreamEvent via socket ("stream-event" command)
//!         └── atm-daemon receives
//!               ├── updates AgentStreamState in SharedStreamStateStore
//!               └── broadcasts on tokio::sync::broadcast (future: to "stream-subscribe")
//!
//! atm-tui
//!   └── polls "agent-stream-state" for turn status per agent
//! ```
//!
//! # Wire format
//!
//! [`DaemonStreamEvent`] is serialized as JSON with `#[serde(tag = "kind")]`.
//! Each event is sent as the payload of a `"stream-event"` socket command.

use serde::{Deserialize, Serialize};

// ── DaemonStreamEvent ────────────────────────────────────────────────────────

/// Normalized event emitted by all three transports to the daemon.
///
/// This is the transport-agnostic event contract. The daemon accepts these via
/// the `"stream-event"` socket command and fans them out to TUI subscribers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DaemonStreamEvent {
    /// A new turn has begun.
    TurnStarted {
        /// Agent identity (e.g., `"arch-ctm"`).
        agent: String,
        /// Thread identifier from the underlying transport.
        thread_id: String,
        /// Unique turn identifier.
        turn_id: String,
        /// Transport that generated the event (`"mcp"`, `"cli-json"`, `"app-server"`).
        transport: String,
    },
    /// A turn has completed (successfully, interrupted, or failed).
    TurnCompleted {
        /// Agent identity.
        agent: String,
        /// Thread identifier.
        thread_id: String,
        /// Unique turn identifier.
        turn_id: String,
        /// Final turn outcome.
        status: TurnStatusWire,
        /// Transport that generated the event.
        transport: String,
    },
    /// The agent has returned to idle after a turn.
    TurnIdle {
        /// Agent identity.
        agent: String,
        /// Last known turn identifier (may be empty if unknown).
        turn_id: String,
        /// Transport that generated the event.
        transport: String,
    },
}

// ── TurnStatusWire ───────────────────────────────────────────────────────────

/// Serializable turn status for wire transfer.
///
/// This is the daemon-facing counterpart of the transport-local `TurnStatus`
/// from `stream_norm.rs` in `atm-agent-mcp`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatusWire {
    /// The turn completed normally.
    Completed,
    /// The turn was interrupted.
    Interrupted,
    /// The turn failed (e.g., process crash).
    Failed,
}

// ── AgentStreamState ─────────────────────────────────────────────────────────

/// Per-agent stream turn state, maintained by the daemon from incoming
/// [`DaemonStreamEvent`]s.
///
/// Returned by the `"agent-stream-state"` socket command.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentStreamState {
    /// The most recent turn identifier (if any).
    pub turn_id: Option<String>,
    /// The thread identifier from the last event.
    pub thread_id: Option<String>,
    /// The transport that last reported an event.
    pub transport: Option<String>,
    /// Coarse state derived from the most recent [`DaemonStreamEvent`].
    pub turn_status: StreamTurnStatus,
}

/// Coarse turn status for TUI display.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamTurnStatus {
    /// No turn in progress (or turn completed and agent returned to idle).
    #[default]
    Idle,
    /// A turn is currently in progress.
    Busy,
    /// The last turn ended in a terminal state (completed, interrupted, or failed).
    Terminal,
}

impl std::fmt::Display for StreamTurnStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Busy => write!(f, "busy"),
            Self::Terminal => write!(f, "terminal"),
        }
    }
}

// ── State update logic ───────────────────────────────────────────────────────

impl AgentStreamState {
    /// Apply a [`DaemonStreamEvent`] to update this agent's stream state.
    ///
    /// Only updates if the event's `agent` field matches the agent this state
    /// tracks. Callers are responsible for routing events to the correct state.
    pub fn apply(&mut self, event: &DaemonStreamEvent) {
        match event {
            DaemonStreamEvent::TurnStarted {
                thread_id,
                turn_id,
                transport,
                ..
            } => {
                self.turn_id = Some(turn_id.clone());
                self.thread_id = Some(thread_id.clone());
                self.transport = Some(transport.clone());
                self.turn_status = StreamTurnStatus::Busy;
            }
            DaemonStreamEvent::TurnCompleted {
                thread_id,
                turn_id,
                transport,
                ..
            } => {
                self.turn_id = Some(turn_id.clone());
                self.thread_id = Some(thread_id.clone());
                self.transport = Some(transport.clone());
                self.turn_status = StreamTurnStatus::Terminal;
            }
            DaemonStreamEvent::TurnIdle {
                turn_id,
                transport,
                ..
            } => {
                self.turn_id = Some(turn_id.clone());
                self.transport = Some(transport.clone());
                self.turn_status = StreamTurnStatus::Idle;
            }
        }
    }

    /// Extract the agent name from a [`DaemonStreamEvent`].
    pub fn agent_from_event(event: &DaemonStreamEvent) -> &str {
        match event {
            DaemonStreamEvent::TurnStarted { agent, .. }
            | DaemonStreamEvent::TurnCompleted { agent, .. }
            | DaemonStreamEvent::TurnIdle { agent, .. } => agent,
        }
    }
}

// ── From conversions ─────────────────────────────────────────────────────────

impl DaemonStreamEvent {
    /// Return the agent name this event is about.
    pub fn agent(&self) -> &str {
        match self {
            Self::TurnStarted { agent, .. } => agent,
            Self::TurnCompleted { agent, .. } => agent,
            Self::TurnIdle { agent, .. } => agent,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_stream_event_serialization_round_trip() {
        let events = vec![
            DaemonStreamEvent::TurnStarted {
                agent: "arch-ctm".to_string(),
                thread_id: "th-1".to_string(),
                turn_id: "turn-abc".to_string(),
                transport: "app-server".to_string(),
            },
            DaemonStreamEvent::TurnCompleted {
                agent: "arch-ctm".to_string(),
                thread_id: "th-1".to_string(),
                turn_id: "turn-abc".to_string(),
                status: TurnStatusWire::Completed,
                transport: "app-server".to_string(),
            },
            DaemonStreamEvent::TurnIdle {
                agent: "arch-ctm".to_string(),
                turn_id: "turn-abc".to_string(),
                transport: "cli-json".to_string(),
            },
        ];

        for event in &events {
            let json = serde_json::to_string(event).expect("serialize");
            let deserialized: DaemonStreamEvent =
                serde_json::from_str(&json).expect("deserialize");
            assert_eq!(&deserialized, event, "round-trip mismatch for {json}");
        }
    }

    #[test]
    fn turn_status_wire_serialization() {
        assert_eq!(
            serde_json::to_string(&TurnStatusWire::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&TurnStatusWire::Interrupted).unwrap(),
            "\"interrupted\""
        );
        assert_eq!(
            serde_json::to_string(&TurnStatusWire::Failed).unwrap(),
            "\"failed\""
        );
    }

    #[test]
    fn agent_stream_state_apply_turn_started() {
        let mut state = AgentStreamState::default();
        let event = DaemonStreamEvent::TurnStarted {
            agent: "a".to_string(),
            thread_id: "th1".to_string(),
            turn_id: "t1".to_string(),
            transport: "app-server".to_string(),
        };
        state.apply(&event);
        assert_eq!(state.turn_status, StreamTurnStatus::Busy);
        assert_eq!(state.turn_id.as_deref(), Some("t1"));
        assert_eq!(state.thread_id.as_deref(), Some("th1"));
        assert_eq!(state.transport.as_deref(), Some("app-server"));
    }

    #[test]
    fn agent_stream_state_apply_turn_completed() {
        let mut state = AgentStreamState {
            turn_status: StreamTurnStatus::Busy,
            turn_id: Some("t1".into()),
            ..Default::default()
        };
        let event = DaemonStreamEvent::TurnCompleted {
            agent: "a".to_string(),
            thread_id: "th1".to_string(),
            turn_id: "t1".to_string(),
            status: TurnStatusWire::Failed,
            transport: "cli-json".to_string(),
        };
        state.apply(&event);
        assert_eq!(state.turn_status, StreamTurnStatus::Terminal);
    }

    #[test]
    fn agent_stream_state_apply_turn_idle() {
        let mut state = AgentStreamState {
            turn_status: StreamTurnStatus::Terminal,
            ..Default::default()
        };
        let event = DaemonStreamEvent::TurnIdle {
            agent: "a".to_string(),
            turn_id: "t1".to_string(),
            transport: "mcp".to_string(),
        };
        state.apply(&event);
        assert_eq!(state.turn_status, StreamTurnStatus::Idle);
    }

    #[test]
    fn agent_from_event_extracts_agent() {
        let event = DaemonStreamEvent::TurnStarted {
            agent: "test-agent".to_string(),
            thread_id: String::new(),
            turn_id: String::new(),
            transport: String::new(),
        };
        assert_eq!(AgentStreamState::agent_from_event(&event), "test-agent");
    }

    #[test]
    fn stream_turn_status_display() {
        assert_eq!(format!("{}", StreamTurnStatus::Idle), "idle");
        assert_eq!(format!("{}", StreamTurnStatus::Busy), "busy");
        assert_eq!(format!("{}", StreamTurnStatus::Terminal), "terminal");
    }

    #[test]
    fn stream_turn_status_default_is_idle() {
        assert_eq!(StreamTurnStatus::default(), StreamTurnStatus::Idle);
    }

    #[test]
    fn agent_stream_state_default_is_idle() {
        let state = AgentStreamState::default();
        assert_eq!(state.turn_status, StreamTurnStatus::Idle);
        assert!(state.turn_id.is_none());
        assert!(state.thread_id.is_none());
        assert!(state.transport.is_none());
    }
}
