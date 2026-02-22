//! Shared streaming event abstractions for JSONL-based transport notification parsing.
//!
//! This module defines types and functions that normalize JSONL notification streams
//! into higher-level lifecycle events.
//!
//! # Shared abstraction
//!
//! These types are protocol-agnostic. [`crate::transport::AppServerTransport`] uses
//! them directly. [`crate::transport::JsonCodecTransport`] has a separate notification
//! parser (using a `type` field rather than `method`) but expresses its
//! turn-lifecycle transitions as [`TurnState`] mutations from this module.
//!
//! # Protocol model
//!
//! The app-server protocol uses JSONL notifications (one JSON object per line).
//! Each notification has a `method` field and optional `params`. This module
//! maps the raw notification stream into [`AppServerNotification`] values,
//! and then maps those into coarser [`SessionEvent`] lifecycle signals.
//!
//! # Turn state machine
//!
//! ```text
//! Idle ──(turn/started)──► Busy ──(turn/completed)──► Terminal
//!                                └──(process crash)──► Terminal
//! ```

use serde_json::Value;

// ─── TurnStatus ───────────────────────────────────────────────────────────────

/// The final outcome of a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnStatus {
    /// The turn completed normally.
    Completed,
    /// The turn was interrupted (e.g. by a cancellation notification).
    Interrupted,
    /// The turn failed (e.g. process crash or protocol error).
    Failed,
}

// ─── TurnState ────────────────────────────────────────────────────────────────

/// Per-thread turn state, tracking the current lifecycle stage.
#[derive(Debug, Clone)]
pub enum TurnState {
    /// No turn is in progress.
    Idle,
    /// A turn is in progress with the given turn identifier.
    Busy { turn_id: String },
    /// The turn has ended (completed, interrupted, or failed).
    Terminal { turn_id: String, status: TurnStatus },
}

impl TurnState {
    /// Returns `true` if the state is [`TurnState::Idle`].
    pub fn is_idle(&self) -> bool {
        matches!(self, TurnState::Idle)
    }
}

// ─── AppServerNotification ────────────────────────────────────────────────────

/// A parsed app-server protocol notification.
///
/// This is the normalized view of a single JSONL line received from the
/// app-server child process.  Unknown methods are represented as
/// [`AppServerNotification::Unknown`] rather than causing errors.
#[derive(Debug)]
pub enum AppServerNotification {
    /// A new turn has begun (`turn/started`).
    TurnStarted { turn_id: String },
    /// The current turn has ended (`turn/completed`).
    TurnCompleted { turn_id: String, status: TurnStatus },
    /// A content item within a turn has started (`item/started`).
    ItemStarted { item_id: String },
    /// A content item within a turn has completed (`item/completed`).
    ItemCompleted { item_id: String },
    /// A streaming delta for a content item (`item/delta`).
    ItemDelta { method: String, params: Value },
    /// An unrecognised notification method.  Non-fatal; callers should
    /// log at `debug` level and continue processing.
    Unknown { method: String },
}

/// Parse a single JSONL line into an [`AppServerNotification`].
///
/// Returns `None` if the line cannot be parsed as a JSON-RPC notification
/// (e.g. blank line, malformed JSON, or missing `method` field).
///
/// Unknown methods produce [`AppServerNotification::Unknown`] rather than
/// returning `None`, so callers can log them explicitly.
pub fn parse_app_server_notification(line: &str) -> Option<AppServerNotification> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let v: Value = serde_json::from_str(line).ok()?;
    let method = v.get("method")?.as_str()?.to_string();
    let params = v.get("params").cloned().unwrap_or(Value::Null);

    let notification = match method.as_str() {
        "turn/started" => {
            let turn_id = params
                .get("turnId")
                .or_else(|| params.get("turn_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            AppServerNotification::TurnStarted { turn_id }
        }
        "turn/completed" => {
            let turn_id = params
                .get("turnId")
                .or_else(|| params.get("turn_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let status_str = params
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("completed");
            let status = match status_str {
                "interrupted" => TurnStatus::Interrupted,
                "failed" => TurnStatus::Failed,
                _ => TurnStatus::Completed,
            };
            AppServerNotification::TurnCompleted { turn_id, status }
        }
        "item/started" => {
            let item_id = params
                .get("itemId")
                .or_else(|| params.get("item_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            AppServerNotification::ItemStarted { item_id }
        }
        "item/completed" => {
            let item_id = params
                .get("itemId")
                .or_else(|| params.get("item_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            AppServerNotification::ItemCompleted { item_id }
        }
        // Delta method patterns from app-server protocol reference Section 5.
        // The protocol uses specific compound method names rather than a single
        // "item/delta" method.  Known delta variants include:
        //   item/agentMessage/delta, item/plan/delta,
        //   item/commandExecution/outputDelta, item/text/TextDelta,
        //   item/toolUse/PartAdded, item/thinking/delta, item/progress
        // The method name is preserved in ItemDelta { method } so downstream
        // consumers can dispatch on the specific variant if needed.
        // G.7 will refine TUI fanout routing based on the specific method variants.
        m if m.starts_with("item/")
            && (m.ends_with("/delta")
                || m.ends_with("/outputDelta")
                || m.ends_with("/progress")
                || m.ends_with("TextDelta")
                || m.ends_with("PartAdded")) =>
        {
            AppServerNotification::ItemDelta { method: m.to_string(), params }
        }
        _ => AppServerNotification::Unknown { method },
    };

    Some(notification)
}

// ─── SessionEvent ─────────────────────────────────────────────────────────────

/// Normalized lifecycle event for downstream consumers (proxy, daemon).
///
/// Currently defined but not yet wired to a channel — the downstream wiring
/// (proxy observing turn transitions from app-server and emitting daemon
/// lifecycle events via `lifecycle_emit`) is a Sprint G.4 deliverable.
/// This type is defined here so G.4 can use it without changing stream_norm.rs.
///
/// These are coarser-grained than [`AppServerNotification`] and represent
/// the transitions that the daemon cares about: whether the agent is
/// processing a turn, has finished, or has failed.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// The agent has begun processing a turn.
    TurnBusy { turn_id: String },
    /// The agent has returned to idle (turn completed normally or interrupted).
    TurnIdle { turn_id: String },
    /// The turn has entered a terminal state (completed, interrupted, or failed).
    TurnTerminal { turn_id: String, status: TurnStatus },
}

/// Checks whether a JSON-RPC response value indicates a server overload error
/// (error code `-32001`).
///
/// This is used by the backpressure retry logic in [`crate::transport::AppServerTransport`].
pub fn is_overload_error(response: &Value) -> bool {
    response
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_i64())
        .map(|code| code == -32001)
        .unwrap_or(false)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_turn_started() {
        let line = r#"{"method":"turn/started","params":{"turnId":"t1"}}"#;
        let n = parse_app_server_notification(line).unwrap();
        assert!(matches!(n, AppServerNotification::TurnStarted { turn_id } if turn_id == "t1"));
    }

    #[test]
    fn parse_turn_completed_default_status() {
        let line = r#"{"method":"turn/completed","params":{"turnId":"t1"}}"#;
        let n = parse_app_server_notification(line).unwrap();
        assert!(matches!(
            n,
            AppServerNotification::TurnCompleted { turn_id, status: TurnStatus::Completed }
            if turn_id == "t1"
        ));
    }

    #[test]
    fn parse_turn_completed_interrupted() {
        let line = r#"{"method":"turn/completed","params":{"turnId":"t2","status":"interrupted"}}"#;
        let n = parse_app_server_notification(line).unwrap();
        assert!(matches!(
            n,
            AppServerNotification::TurnCompleted { status: TurnStatus::Interrupted, .. }
        ));
    }

    #[test]
    fn parse_item_started() {
        let line = r#"{"method":"item/started","params":{"itemId":"i1"}}"#;
        let n = parse_app_server_notification(line).unwrap();
        assert!(matches!(n, AppServerNotification::ItemStarted { item_id } if item_id == "i1"));
    }

    #[test]
    fn parse_item_completed() {
        let line = r#"{"method":"item/completed","params":{"itemId":"i1"}}"#;
        let n = parse_app_server_notification(line).unwrap();
        assert!(matches!(n, AppServerNotification::ItemCompleted { item_id } if item_id == "i1"));
    }

    #[test]
    fn parse_item_delta_legacy() {
        // The legacy "item/delta" literal also matches because it starts with
        // "item/" and ends with "/delta".
        let line = r#"{"method":"item/delta","params":{"delta":"hello"}}"#;
        let n = parse_app_server_notification(line).unwrap();
        assert!(matches!(n, AppServerNotification::ItemDelta { .. }));
    }

    #[test]
    fn parse_item_agent_message_delta() {
        // Actual app-server protocol delta method: item/agentMessage/delta
        // (Section 5 of the protocol reference).
        let line = r#"{"method":"item/agentMessage/delta","params":{"text":"hello"}}"#;
        let n = parse_app_server_notification(line)
            .expect("item/agentMessage/delta should parse as ItemDelta");
        assert!(
            matches!(
                &n,
                AppServerNotification::ItemDelta { method, .. } if method == "item/agentMessage/delta"
            ),
            "expected ItemDelta with method item/agentMessage/delta, got: {n:?}"
        );
    }

    #[test]
    fn parse_item_command_execution_output_delta() {
        // Actual app-server protocol delta method: item/commandExecution/outputDelta
        // (Section 5 of the protocol reference).
        let line = r#"{"method":"item/commandExecution/outputDelta","params":{"output":"ls\n"}}"#;
        let n = parse_app_server_notification(line)
            .expect("item/commandExecution/outputDelta should parse as ItemDelta");
        assert!(
            matches!(
                &n,
                AppServerNotification::ItemDelta { method, .. }
                    if method == "item/commandExecution/outputDelta"
            ),
            "expected ItemDelta with method item/commandExecution/outputDelta, got: {n:?}"
        );
    }

    #[test]
    fn parse_unknown_method() {
        let line = r#"{"method":"unknown/event","params":{}}"#;
        let n = parse_app_server_notification(line).unwrap();
        assert!(matches!(
            n,
            AppServerNotification::Unknown { method } if method == "unknown/event"
        ));
    }

    #[test]
    fn parse_empty_line_returns_none() {
        assert!(parse_app_server_notification("").is_none());
        assert!(parse_app_server_notification("   ").is_none());
    }

    #[test]
    fn parse_malformed_json_returns_none() {
        assert!(parse_app_server_notification("{not valid json").is_none());
    }

    #[test]
    fn parse_missing_method_returns_none() {
        assert!(parse_app_server_notification(r#"{"params":{}}"#).is_none());
    }

    #[test]
    fn is_overload_error_detects_minus_32001() {
        let v = serde_json::json!({"error":{"code":-32001,"message":"overloaded"}});
        assert!(is_overload_error(&v));
    }

    #[test]
    fn is_overload_error_false_for_other_codes() {
        let v = serde_json::json!({"error":{"code":-32600,"message":"invalid request"}});
        assert!(!is_overload_error(&v));
    }

    #[test]
    fn is_overload_error_false_for_success() {
        let v = serde_json::json!({"result":{}});
        assert!(!is_overload_error(&v));
    }

    #[test]
    fn turn_state_is_idle_only_for_idle_variant() {
        assert!(TurnState::Idle.is_idle());
        assert!(!TurnState::Busy { turn_id: "t1".into() }.is_idle());
        assert!(!TurnState::Terminal {
            turn_id: "t1".into(),
            status: TurnStatus::Completed
        }
        .is_idle());
    }
}
