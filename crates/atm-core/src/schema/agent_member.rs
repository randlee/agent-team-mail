//! Agent member schema for team configuration

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Agent member in a team
///
/// Represents a single agent in the team's member list within the team config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMember {
    /// Unique agent identifier (format: "{name}@{team_name}")
    pub agent_id: String,

    /// Agent instance name (unique within team)
    pub name: String,

    /// Agent capability type (e.g., "general-purpose", "Explore", "Plan")
    pub agent_type: String,

    /// Claude model identifier
    pub model: String,

    /// Custom prompt for specialization (null for team-lead)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,

    /// UI color code (e.g., "blue", "green", "yellow")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,

    /// Whether plan mode is required
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_mode_required: Option<bool>,

    /// Unix timestamp in milliseconds when agent joined
    pub joined_at: u64,

    /// Terminal pane ID (empty string if no terminal)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_pane_id: Option<String>,

    /// Current working directory of agent
    pub cwd: String,

    /// Notification subscriptions (usually empty array)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<serde_json::Value>,

    /// Backend type (e.g., "tmux", empty if not running)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_type: Option<String>,

    /// Whether agent is currently running
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,

    /// Unix timestamp in milliseconds of last activity (message sent, message read)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active: Option<u64>,

    /// Unknown fields for forward compatibility
    #[serde(flatten)]
    pub unknown_fields: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_member_roundtrip_team_lead() {
        let json = r#"{
            "agentId": "team-lead@test-team",
            "name": "team-lead",
            "agentType": "general-purpose",
            "model": "claude-haiku-4-5-20251001",
            "joinedAt": 1770765919076,
            "tmuxPaneId": "",
            "cwd": "/Users/randlee/Documents/github/test",
            "subscriptions": []
        }"#;

        let member: AgentMember = serde_json::from_str(json).unwrap();
        assert_eq!(member.agent_id, "team-lead@test-team");
        assert_eq!(member.name, "team-lead");
        assert_eq!(member.agent_type, "general-purpose");
        assert_eq!(member.model, "claude-haiku-4-5-20251001");
        assert_eq!(member.joined_at, 1770765919076);
        assert_eq!(member.cwd, "/Users/randlee/Documents/github/test");
        assert!(member.prompt.is_none());
        assert!(member.color.is_none());
        assert!(member.subscriptions.is_empty());

        let serialized = serde_json::to_string(&member).unwrap();
        let reparsed: AgentMember = serde_json::from_str(&serialized).unwrap();
        assert_eq!(member.agent_id, reparsed.agent_id);
    }

    #[test]
    fn test_agent_member_roundtrip_spawned_agent() {
        let json = r#"{
            "agentId": "haiku-poet-1@test-team",
            "name": "haiku-poet-1",
            "agentType": "general-purpose",
            "model": "claude-opus-4-6",
            "prompt": "You are a creative haiku poet.",
            "color": "blue",
            "planModeRequired": false,
            "joinedAt": 1770772206905,
            "tmuxPaneId": "%14",
            "cwd": "/Users/randlee/Documents/github/test",
            "subscriptions": [],
            "backendType": "tmux",
            "isActive": false
        }"#;

        let member: AgentMember = serde_json::from_str(json).unwrap();
        assert_eq!(member.agent_id, "haiku-poet-1@test-team");
        assert_eq!(member.name, "haiku-poet-1");
        assert_eq!(member.prompt, Some("You are a creative haiku poet.".to_string()));
        assert_eq!(member.color, Some("blue".to_string()));
        assert_eq!(member.plan_mode_required, Some(false));
        assert_eq!(member.tmux_pane_id, Some("%14".to_string()));
        assert_eq!(member.backend_type, Some("tmux".to_string()));
        assert_eq!(member.is_active, Some(false));

        let serialized = serde_json::to_string(&member).unwrap();
        let reparsed: AgentMember = serde_json::from_str(&serialized).unwrap();
        assert_eq!(member.prompt, reparsed.prompt);
        assert_eq!(member.color, reparsed.color);
    }

    #[test]
    fn test_agent_member_roundtrip_with_unknown_fields() {
        let json = r#"{
            "agentId": "test-agent@test-team",
            "name": "test-agent",
            "agentType": "general-purpose",
            "model": "claude-sonnet-4-5-20250929",
            "joinedAt": 1770765919076,
            "cwd": "/test",
            "unknownField": "value",
            "futureFeature": {"nested": "data"}
        }"#;

        let member: AgentMember = serde_json::from_str(json).unwrap();
        assert_eq!(member.agent_id, "test-agent@test-team");
        assert_eq!(member.unknown_fields.len(), 2);
        assert!(member.unknown_fields.contains_key("unknownField"));
        assert!(member.unknown_fields.contains_key("futureFeature"));

        let serialized = serde_json::to_string(&member).unwrap();
        let reparsed: AgentMember = serde_json::from_str(&serialized).unwrap();
        assert_eq!(member.unknown_fields.len(), reparsed.unknown_fields.len());
        assert_eq!(
            member.unknown_fields.get("unknownField"),
            reparsed.unknown_fields.get("unknownField")
        );
    }

    #[test]
    fn test_agent_member_missing_optional_fields() {
        let json = r#"{
            "agentId": "test@team",
            "name": "test",
            "agentType": "general-purpose",
            "model": "claude-opus-4-6",
            "joinedAt": 1770765919076,
            "cwd": "/test"
        }"#;

        let member: AgentMember = serde_json::from_str(json).unwrap();
        assert!(member.prompt.is_none());
        assert!(member.color.is_none());
        assert!(member.plan_mode_required.is_none());
        assert!(member.tmux_pane_id.is_none());
        assert!(member.backend_type.is_none());
        assert!(member.is_active.is_none());
        assert!(member.subscriptions.is_empty());
    }
}
