//! Inbox message schema for agent team communication

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const IDLE_NOTIFICATION_TYPE: &str = "idle_notification";

/// Message in an agent's inbox
///
/// Messages are stored in `~/.claude/teams/{team_name}/inboxes/{agent_name}.json`
/// as an array of InboxMessage objects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxMessage {
    /// Sender agent name or 'team-lead'
    pub from: String,

    /// Sender team when the envelope crossed team boundaries or when the sender
    /// explicitly recorded its team in the message envelope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_team: Option<String>,

    /// Message content (markdown supported)
    #[serde(alias = "content")]
    pub text: String,

    /// ISO 8601 UTC timestamp
    pub timestamp: String,

    /// Whether the message has been read
    #[serde(default)]
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

impl InboxMessage {
    pub fn pending_ack_at(&self) -> Option<&str> {
        self.unknown_fields
            .get("pendingAckAt")
            .and_then(|value| value.as_str())
    }

    pub fn acknowledged_at(&self) -> Option<&str> {
        self.unknown_fields
            .get("acknowledgedAt")
            .and_then(|value| value.as_str())
    }

    pub fn is_acknowledged(&self) -> bool {
        self.acknowledged_at().is_some()
    }

    pub fn is_pending_action(&self) -> bool {
        !self.read || (self.pending_ack_at().is_some() && !self.is_acknowledged())
    }

    pub fn notification_type(&self) -> Option<&str> {
        self.unknown_fields
            .get("type")
            .and_then(|value| value.as_str())
    }

    pub fn is_idle_notification(&self) -> bool {
        self.notification_type() == Some(IDLE_NOTIFICATION_TYPE)
    }

    pub fn idle_notification_sender(&self) -> Option<&str> {
        self.unknown_fields
            .get("idleSender")
            .and_then(|value| value.as_str())
    }

    pub fn mark_idle_notification(&mut self, sender: impl Into<String>) {
        self.unknown_fields.insert(
            "type".to_string(),
            serde_json::Value::String(IDLE_NOTIFICATION_TYPE.to_string()),
        );
        self.unknown_fields.insert(
            "idleSender".to_string(),
            serde_json::Value::String(sender.into()),
        );
    }

    pub fn mark_pending_ack(&mut self, timestamp: impl Into<String>) {
        self.unknown_fields.insert(
            "pendingAckAt".to_string(),
            serde_json::Value::String(timestamp.into()),
        );
    }

    pub fn mark_acknowledged(&mut self, timestamp: impl Into<String>) {
        self.unknown_fields.remove("pendingAckAt");
        self.unknown_fields.insert(
            "acknowledgedAt".to_string(),
            serde_json::Value::String(timestamp.into()),
        );
    }
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
        assert!(msg.source_team.is_none());
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
        assert!(msg.source_team.is_none());
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
    fn test_inbox_message_roundtrip_with_source_team() {
        let json = r#"{
            "from": "team-lead",
            "source_team": "src-gen",
            "text": "Cross-team message",
            "timestamp": "2026-02-11T14:30:00.000Z",
            "read": false
        }"#;

        let msg: InboxMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.from, "team-lead");
        assert_eq!(msg.source_team.as_deref(), Some("src-gen"));

        let serialized = serde_json::to_string(&msg).unwrap();
        let reparsed: InboxMessage = serde_json::from_str(&serialized).unwrap();
        assert_eq!(reparsed.source_team.as_deref(), Some("src-gen"));
    }

    #[test]
    fn test_inbox_message_accepts_content_alias_and_missing_read() {
        let json = r#"{
            "from": "team-lead",
            "content": "Legacy content key",
            "timestamp": "2026-02-11T14:30:00.000Z"
        }"#;

        let msg: InboxMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.text, "Legacy content key");
        assert!(!msg.read, "missing read should default to false");
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

    #[test]
    fn test_acknowledged_at_roundtrip_via_unknown_fields() {
        let json = r#"{
            "from": "team-lead",
            "text": "Task assigned",
            "timestamp": "2026-02-11T14:30:00.000Z",
            "read": true,
            "pendingAckAt": "2026-02-11T14:30:30.000Z",
            "acknowledgedAt": "2026-02-11T14:31:00.000Z"
        }"#;

        let msg: InboxMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.acknowledged_at(), Some("2026-02-11T14:31:00.000Z"));
        assert_eq!(msg.pending_ack_at(), Some("2026-02-11T14:30:30.000Z"));
        assert!(msg.is_acknowledged());
        assert!(!msg.is_pending_action());

        let serialized = serde_json::to_string(&msg).unwrap();
        let reparsed: InboxMessage = serde_json::from_str(&serialized).unwrap();
        assert_eq!(reparsed.acknowledged_at(), Some("2026-02-11T14:31:00.000Z"));
    }

    #[test]
    fn test_mark_acknowledged_sets_unknown_field() {
        let mut msg = InboxMessage {
            from: "team-lead".to_string(),
            source_team: None,
            text: "Task assigned".to_string(),
            timestamp: "2026-02-11T14:30:00.000Z".to_string(),
            read: true,
            summary: None,
            message_id: Some("msg-1".to_string()),
            unknown_fields: HashMap::new(),
        };

        msg.mark_pending_ack("2026-02-11T14:30:30.000Z");
        msg.mark_acknowledged("2026-02-11T14:31:00.000Z");
        assert_eq!(msg.pending_ack_at(), None);
        assert_eq!(msg.acknowledged_at(), Some("2026-02-11T14:31:00.000Z"));
    }

    #[test]
    fn test_idle_notification_helpers_roundtrip() {
        let mut msg = InboxMessage {
            from: "daemon".to_string(),
            source_team: None,
            text: "[AGENT STATE] arch-ctm is now idle".to_string(),
            timestamp: "2026-02-11T14:30:00.000Z".to_string(),
            read: false,
            summary: Some("Agent arch-ctm → idle".to_string()),
            message_id: Some("msg-1".to_string()),
            unknown_fields: HashMap::new(),
        };

        msg.mark_idle_notification("arch-ctm");

        assert!(msg.is_idle_notification());
        assert_eq!(msg.idle_notification_sender(), Some("arch-ctm"));

        let serialized = serde_json::to_string(&msg).unwrap();
        let reparsed: InboxMessage = serde_json::from_str(&serialized).unwrap();
        assert!(reparsed.is_idle_notification());
        assert_eq!(reparsed.idle_notification_sender(), Some("arch-ctm"));
    }

    #[test]
    fn test_legacy_read_message_is_not_pending_without_pending_marker() {
        let msg: InboxMessage = serde_json::from_str(
            r#"{
                "from": "team-lead",
                "text": "Legacy",
                "timestamp": "2026-02-11T14:30:00.000Z",
                "read": true
            }"#,
        )
        .unwrap();

        assert!(!msg.is_pending_action());
        assert!(!msg.is_acknowledged());
    }
}
