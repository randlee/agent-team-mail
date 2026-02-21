//! Control protocol message types for live agent stdin/interrupt actions.
//!
//! These payload types are carried inside daemon socket requests (`command:
//! "control"`).  They are versioned independently from the daemon socket
//! protocol via the `v` field.

use serde::{Deserialize, Serialize};

/// Current control payload schema version.
pub const CONTROL_SCHEMA_VERSION: u32 = 1;

/// Request payload for daemon `command: "control"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlRequest {
    /// Control schema version.
    pub v: u32,
    /// Stable idempotency key for this logical request.
    pub request_id: String,
    /// Control message type per protocol spec §3.1 and §3.3.
    ///
    /// - `"control.stdin.request"` for [`ControlAction::Stdin`]
    /// - `"control.interrupt.request"` for [`ControlAction::Interrupt`]
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Signal field required for interrupt requests per protocol spec §3.3.
    ///
    /// Must be `"interrupt"` when `action == ControlAction::Interrupt`.
    /// `None` for all other actions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// RFC3339 UTC timestamp from sender.
    pub sent_at: String,
    /// Team namespace.
    pub team: String,
    /// Claude session identifier.
    pub session_id: String,
    /// Target worker identifier.
    pub agent_id: String,
    /// Sender identity.
    pub sender: String,
    /// Control action kind.
    pub action: ControlAction,
    /// Inline stdin payload (UTF-8 text).
    ///
    /// Required for [`ControlAction::Stdin`] requests unless `content_ref` is set.
    /// Serialized as `"content"` per the control protocol spec §3.1.
    #[serde(rename = "content", skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    /// Optional file reference for oversized stdin payloads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_ref: Option<ContentRef>,
}

/// Control action kind.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    Stdin,
    Interrupt,
}

/// Acknowledgement payload returned by the daemon for a control request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlAck {
    pub request_id: String,
    pub result: ControlResult,
    pub duplicate: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub acked_at: String,
}

/// Result status for control processing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlResult {
    Ok,
    NotLive,
    NotFound,
    Busy,
    Timeout,
    Rejected,
    InternalError,
}

/// File-backed content reference for oversize payloads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentRef {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub mime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_request_round_trip() {
        let req = ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: "req-1".to_string(),
            msg_type: "control.stdin.request".to_string(),
            signal: None,
            sent_at: "2026-02-21T00:00:00Z".to_string(),
            team: "atm-dev".to_string(),
            session_id: "sess-1".to_string(),
            agent_id: "arch-ctm".to_string(),
            sender: "team-lead".to_string(),
            action: ControlAction::Stdin,
            payload: Some("hello".to_string()),
            content_ref: None,
        };
        let json = serde_json::to_string(&req).expect("serialize request");
        // Verify the wire-format key is "content", not "payload" (protocol spec §3.1).
        assert!(
            json.contains("\"content\":"),
            "serialized ControlRequest must use key \"content\" not \"payload\"; got: {json}"
        );
        assert!(
            !json.contains("\"payload\":"),
            "serialized ControlRequest must not contain key \"payload\"; got: {json}"
        );
        let decoded: ControlRequest = serde_json::from_str(&json).expect("deserialize request");
        assert_eq!(decoded, req);
    }

    #[test]
    fn control_ack_round_trip() {
        let ack = ControlAck {
            request_id: "req-2".to_string(),
            result: ControlResult::Ok,
            duplicate: false,
            detail: Some("accepted".to_string()),
            acked_at: "2026-02-21T00:00:01Z".to_string(),
        };
        let json = serde_json::to_string(&ack).expect("serialize ack");
        let decoded: ControlAck = serde_json::from_str(&json).expect("deserialize ack");
        assert_eq!(decoded, ack);
    }

    #[test]
    fn content_ref_round_trip() {
        let cref = ContentRef {
            path: "/tmp/input.txt".to_string(),
            size_bytes: 12,
            sha256: "abc123".to_string(),
            mime: "text/plain".to_string(),
            expires_at: Some("2026-02-21T00:10:00Z".to_string()),
        };
        let json = serde_json::to_string(&cref).expect("serialize content ref");
        let decoded: ContentRef = serde_json::from_str(&json).expect("deserialize content ref");
        assert_eq!(decoded, cref);
    }
}
