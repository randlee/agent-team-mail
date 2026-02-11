//! Inbox message schema for agent team communication

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Message in an agent's inbox
///
/// Messages are stored in `~/.claude/teams/{team_name}/inboxes/{agent_name}.json`
/// as an array of InboxMessage objects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxMessage {
    /// Sender agent name or 'team-lead'
    pub from: String,

    /// Message content (markdown supported)
    pub text: String,

    /// ISO 8601 UTC timestamp
    pub timestamp: String,

    /// Whether the message has been read
    pub read: bool,

    /// Brief summary (5-10 words)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,

    /// Message ID for deduplication (atm-originated messages only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,

    /// Unknown fields for forward compatibility
    #[serde(flatten)]
    pub unknown_fields: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inbox_message_roundtrip_minimal() {
        let json = r#"{
            "from": "team-lead",
            "text": "CI failure detected",
            "timestamp": "2026-02-11T14:30:00.000Z",
            "read": false
        }"#;

        let msg: InboxMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.from, "team-lead");
        assert_eq!(msg.text, "CI failure detected");
        assert_eq!(msg.timestamp, "2026-02-11T14:30:00.000Z");
        assert!(!msg.read);
        assert!(msg.summary.is_none());
        assert!(msg.message_id.is_none());

        let serialized = serde_json::to_string(&msg).unwrap();
        let reparsed: InboxMessage = serde_json::from_str(&serialized).unwrap();
        assert_eq!(msg.from, reparsed.from);
        assert_eq!(msg.text, reparsed.text);
    }

    #[test]
    fn test_inbox_message_roundtrip_complete() {
        let json = r#"{
            "from": "ci-fix-agent",
            "text": "Investigation complete. Fix implemented.",
            "timestamp": "2026-02-11T14:35:00.000Z",
            "read": true,
            "summary": "Fix implemented",
            "message_id": "msg-abc-123"
        }"#;

        let msg: InboxMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.from, "ci-fix-agent");
        assert_eq!(msg.text, "Investigation complete. Fix implemented.");
        assert!(msg.read);
        assert_eq!(msg.summary, Some("Fix implemented".to_string()));
        assert_eq!(msg.message_id, Some("msg-abc-123".to_string()));

        let serialized = serde_json::to_string(&msg).unwrap();
        let reparsed: InboxMessage = serde_json::from_str(&serialized).unwrap();
        assert_eq!(msg.message_id, reparsed.message_id);
    }

    #[test]
    fn test_inbox_message_roundtrip_with_unknown_fields() {
        let json = r#"{
            "from": "team-lead",
            "text": "Test message",
            "timestamp": "2026-02-11T14:30:00.000Z",
            "read": false,
            "unknownField": "value",
            "futureFeature": {"nested": "data"}
        }"#;

        let msg: InboxMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.from, "team-lead");
        assert_eq!(msg.unknown_fields.len(), 2);
        assert!(msg.unknown_fields.contains_key("unknownField"));
        assert!(msg.unknown_fields.contains_key("futureFeature"));

        let serialized = serde_json::to_string(&msg).unwrap();
        let reparsed: InboxMessage = serde_json::from_str(&serialized).unwrap();
        assert_eq!(msg.unknown_fields.len(), reparsed.unknown_fields.len());
        assert_eq!(
            msg.unknown_fields.get("unknownField"),
            reparsed.unknown_fields.get("unknownField")
        );
    }

    #[test]
    fn test_inbox_message_array() {
        let json = r#"[
            {
                "from": "team-lead",
                "text": "First message",
                "timestamp": "2026-02-11T14:30:00.000Z",
                "read": false,
                "summary": "First message"
            },
            {
                "from": "ci-fix-agent",
                "text": "Second message",
                "timestamp": "2026-02-11T14:31:00.000Z",
                "read": true,
                "summary": "Second message"
            }
        ]"#;

        let messages: Vec<InboxMessage> = serde_json::from_str(json).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].from, "team-lead");
        assert_eq!(messages[1].from, "ci-fix-agent");
        assert!(!messages[0].read);
        assert!(messages[1].read);
    }
}
