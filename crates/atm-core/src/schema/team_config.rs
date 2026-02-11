//! Team configuration schema

use super::AgentMember;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Team configuration
///
/// Stored at `~/.claude/teams/{team_name}/config.json`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamConfig {
    /// Team name (matches directory name)
    pub name: String,

    /// Human-readable team purpose
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Unix timestamp in milliseconds when team was created
    pub created_at: u64,

    /// Lead agent ID (format: "team-lead@{team_name}")
    pub lead_agent_id: String,

    /// UUID of session that created the team
    pub lead_session_id: String,

    /// Array of team members (includes team lead as first member)
    pub members: Vec<AgentMember>,

    /// Unknown fields for forward compatibility
    #[serde(flatten)]
    pub unknown_fields: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_team_config_roundtrip_minimal() {
        let json = r#"{
            "name": "test-team",
            "createdAt": 1770765919076,
            "leadAgentId": "team-lead@test-team",
            "leadSessionId": "6075f866-f103-4be1-b2e9-8dbf66009eb9",
            "members": []
        }"#;

        let config: TeamConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "test-team");
        assert_eq!(config.created_at, 1770765919076);
        assert_eq!(config.lead_agent_id, "team-lead@test-team");
        assert_eq!(config.lead_session_id, "6075f866-f103-4be1-b2e9-8dbf66009eb9");
        assert!(config.description.is_none());
        assert!(config.members.is_empty());

        let serialized = serde_json::to_string(&config).unwrap();
        let reparsed: TeamConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(config.name, reparsed.name);
    }

    #[test]
    fn test_team_config_roundtrip_complete() {
        let json = r#"{
            "name": "test-team",
            "description": "Test team for agent coordination",
            "createdAt": 1770765919076,
            "leadAgentId": "team-lead@test-team",
            "leadSessionId": "6075f866-f103-4be1-b2e9-8dbf66009eb9",
            "members": [
                {
                    "agentId": "team-lead@test-team",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "claude-haiku-4-5-20251001",
                    "joinedAt": 1770765919076,
                    "tmuxPaneId": "",
                    "cwd": "/test",
                    "subscriptions": []
                },
                {
                    "agentId": "haiku-poet-1@test-team",
                    "name": "haiku-poet-1",
                    "agentType": "general-purpose",
                    "model": "claude-opus-4-6",
                    "prompt": "You are a creative haiku poet.",
                    "color": "blue",
                    "planModeRequired": false,
                    "joinedAt": 1770772206905,
                    "tmuxPaneId": "%14",
                    "cwd": "/test",
                    "subscriptions": [],
                    "backendType": "tmux",
                    "isActive": false
                }
            ]
        }"#;

        let config: TeamConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "test-team");
        assert_eq!(config.description, Some("Test team for agent coordination".to_string()));
        assert_eq!(config.members.len(), 2);
        assert_eq!(config.members[0].name, "team-lead");
        assert_eq!(config.members[1].name, "haiku-poet-1");
        assert_eq!(config.members[1].color, Some("blue".to_string()));

        let serialized = serde_json::to_string(&config).unwrap();
        let reparsed: TeamConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(config.members.len(), reparsed.members.len());
        assert_eq!(config.members[1].name, reparsed.members[1].name);
    }

    #[test]
    fn test_team_config_roundtrip_with_unknown_fields() {
        let json = r#"{
            "name": "test-team",
            "createdAt": 1770765919076,
            "leadAgentId": "team-lead@test-team",
            "leadSessionId": "6075f866-f103-4be1-b2e9-8dbf66009eb9",
            "members": [],
            "unknownField": "value",
            "futureFeature": {"nested": "data"}
        }"#;

        let config: TeamConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "test-team");
        assert_eq!(config.unknown_fields.len(), 2);
        assert!(config.unknown_fields.contains_key("unknownField"));
        assert!(config.unknown_fields.contains_key("futureFeature"));

        let serialized = serde_json::to_string(&config).unwrap();
        let reparsed: TeamConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(config.unknown_fields.len(), reparsed.unknown_fields.len());
        assert_eq!(
            config.unknown_fields.get("unknownField"),
            reparsed.unknown_fields.get("unknownField")
        );
    }

    #[test]
    fn test_team_config_from_real_example() {
        // From agent-team-api.md lines 764-828
        let json = r#"{
            "name": "test-team",
            "description": "Test team for agent coordination and workflow demonstration",
            "createdAt": 1770765919076,
            "leadAgentId": "team-lead@test-team",
            "leadSessionId": "6075f866-f103-4be1-b2e9-8dbf66009eb9",
            "members": [
                {
                    "agentId": "team-lead@test-team",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "claude-haiku-4-5-20251001",
                    "joinedAt": 1770765919076,
                    "tmuxPaneId": "",
                    "cwd": "/Users/randlee/Documents/github/agent-teams-test/test-workspace",
                    "subscriptions": []
                },
                {
                    "agentId": "haiku-poet-1@test-team",
                    "name": "haiku-poet-1",
                    "agentType": "general-purpose",
                    "model": "claude-opus-4-6",
                    "prompt": "You are a creative haiku poet. Wait for the team lead's broadcast message with a haiku composition request, then compose and share your best haiku with the team. Make it meaningful and poetic.",
                    "color": "blue",
                    "planModeRequired": false,
                    "joinedAt": 1770772206905,
                    "tmuxPaneId": "%14",
                    "cwd": "/Users/randlee/Documents/github/agent-teams-test/test-workspace",
                    "subscriptions": [],
                    "backendType": "tmux",
                    "isActive": false
                },
                {
                    "agentId": "haiku-poet-2@test-team",
                    "name": "haiku-poet-2",
                    "agentType": "general-purpose",
                    "model": "claude-opus-4-6",
                    "prompt": "You are a nature haiku specialist. Wait for the team lead's broadcast message with a haiku composition request, then compose and share a haiku about nature or software development. Make it vivid and memorable.",
                    "color": "green",
                    "planModeRequired": false,
                    "joinedAt": 1770772207583,
                    "tmuxPaneId": "%15",
                    "cwd": "/Users/randlee/Documents/github/agent-teams-test/test-workspace",
                    "subscriptions": [],
                    "backendType": "tmux",
                    "isActive": true
                },
                {
                    "agentId": "haiku-poet-3@test-team",
                    "name": "haiku-poet-3",
                    "agentType": "general-purpose",
                    "model": "claude-opus-4-6",
                    "prompt": "You are a tech haiku specialist. Wait for the team lead's broadcast message with a haiku composition request, then compose and share a haiku about agents, teams, or AI. Make it clever and insightful.",
                    "color": "yellow",
                    "planModeRequired": false,
                    "joinedAt": 1770772208362,
                    "tmuxPaneId": "%16",
                    "cwd": "/Users/randlee/Documents/github/agent-teams-test/test-workspace",
                    "subscriptions": [],
                    "backendType": "tmux",
                    "isActive": true
                }
            ]
        }"#;

        let config: TeamConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "test-team");
        assert_eq!(config.members.len(), 4);
        assert_eq!(config.members[0].name, "team-lead");
        assert_eq!(config.members[1].name, "haiku-poet-1");
        assert_eq!(config.members[2].name, "haiku-poet-2");
        assert_eq!(config.members[3].name, "haiku-poet-3");

        // Verify round-trip
        let serialized = serde_json::to_string_pretty(&config).unwrap();
        let reparsed: TeamConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(config.name, reparsed.name);
        assert_eq!(config.members.len(), reparsed.members.len());
    }
}
