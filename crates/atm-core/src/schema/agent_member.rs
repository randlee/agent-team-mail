//! Agent member schema for team configuration

use crate::model_registry::ModelId;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// Backend type for an agent member.
///
/// Distinguishes between Claude Code agents, Codex agents, Gemini agents,
/// generic external agents, and human participants.  The `Human` variant
/// carries a required username.
///
/// # Serialisation
///
/// All variants serialise/deserialise via their display string so they can be
/// stored in `config.json` without schema-breaking changes.
///
/// # Examples
///
/// ```rust
/// use agent_team_mail_core::schema::agent_member::BackendType;
/// use std::str::FromStr;
///
/// assert_eq!(BackendType::from_str("codex").unwrap(), BackendType::Codex);
/// assert_eq!(
///     BackendType::from_str("human:randlee").unwrap(),
///     BackendType::Human("randlee".to_string())
/// );
/// assert!(BackendType::from_str("human:").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendType {
    /// A Claude Code agent (tmux or in-process).
    ClaudeCode,
    /// A Codex (OpenAI) agent.
    Codex,
    /// A Gemini agent.
    Gemini,
    /// A generic external agent not covered by other variants.
    External,
    /// A human participant identified by username.
    ///
    /// Serialised as `"human:<username>"`.  The username is required;
    /// `"human:"` alone is rejected.
    Human(String),
}

impl fmt::Display for BackendType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendType::ClaudeCode => write!(f, "claude-code"),
            BackendType::Codex => write!(f, "codex"),
            BackendType::Gemini => write!(f, "gemini"),
            BackendType::External => write!(f, "external"),
            BackendType::Human(username) => write!(f, "human:{username}"),
        }
    }
}

impl FromStr for BackendType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude-code" => Ok(BackendType::ClaudeCode),
            "codex" => Ok(BackendType::Codex),
            "gemini" => Ok(BackendType::Gemini),
            "external" => Ok(BackendType::External),
            s if s.starts_with("human:") => {
                let username = &s["human:".len()..];
                if username.is_empty() {
                    Err("'human:' requires a username (e.g., 'human:randlee')".to_string())
                } else {
                    Ok(BackendType::Human(username.to_string()))
                }
            }
            other => Err(format!(
                "Unknown backend type '{other}'. Valid values: claude-code, codex, gemini, external, human:<username>"
            )),
        }
    }
}

impl Serialize for BackendType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for BackendType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        BackendType::from_str(&s).map_err(serde::de::Error::custom)
    }
}

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

    /// Claude model identifier (legacy string; preserved for Claude Code compatibility)
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

    /// Backend type from the Claude Code API (e.g., "tmux", "in-process").
    ///
    /// This field is set by Claude Code itself.  For external agents added
    /// via `atm teams add-member`, use [`external_backend_type`] instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_type: Option<String>,

    /// Whether agent is currently running
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,

    /// Unix timestamp in milliseconds of last activity (message sent, message read)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active: Option<u64>,

    /// Session or agent ID for external agents.
    ///
    /// Stored at `add-member` time or auto-updated by the daemon when a
    /// `session-start` hook event arrives for a matching agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Typed backend for external agents (E.6 semantics).
    ///
    /// Separate from [`backend_type`] to avoid conflicting with the string
    /// values used by the Claude Code API (e.g., `"tmux"`).
    #[serde(rename = "externalBackendType", default, skip_serializing_if = "Option::is_none")]
    pub external_backend_type: Option<BackendType>,

    /// Validated model identifier for external agents.
    ///
    /// Separate from [`model`] (which is preserved for Claude Code
    /// compatibility).  Set by `add-member --model` with registry validation.
    #[serde(rename = "externalModel", default, skip_serializing_if = "Option::is_none")]
    pub external_model: Option<ModelId>,

    /// Unknown fields for forward compatibility
    #[serde(flatten)]
    pub unknown_fields: HashMap<String, serde_json::Value>,
}

impl AgentMember {
    /// Return the effective backend type for this member.
    ///
    /// Prefers [`external_backend_type`] (typed, E.6 semantics) over
    /// [`backend_type`] (legacy string from Claude Code).  If neither is set
    /// returns `None`.
    pub fn effective_backend_type(&self) -> Option<BackendType> {
        if let Some(ref bt) = self.external_backend_type {
            return Some(bt.clone());
        }
        // Fall back to parsing the legacy string field
        if let Some(ref bt_str) = self.backend_type {
            return BackendType::from_str(bt_str).ok();
        }
        None
    }
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
        assert!(member.session_id.is_none());
        assert!(member.external_backend_type.is_none());
        assert!(member.external_model.is_none());
    }

    #[test]
    fn test_agent_member_external_fields_roundtrip() {
        let json = r#"{
            "agentId": "arch-ctm@atm-dev",
            "name": "arch-ctm",
            "agentType": "codex",
            "model": "gpt5.3-codex",
            "joinedAt": 1770765919076,
            "cwd": "/workspace",
            "sessionId": "uuid-1234",
            "externalBackendType": "codex",
            "externalModel": "gpt5.3-codex"
        }"#;

        let member: AgentMember = serde_json::from_str(json).unwrap();
        assert_eq!(member.session_id.as_deref(), Some("uuid-1234"));
        assert_eq!(member.external_backend_type, Some(BackendType::Codex));
        assert_eq!(member.external_model, Some(ModelId::Gpt53Codex));

        let serialized = serde_json::to_string(&member).unwrap();
        let reparsed: AgentMember = serde_json::from_str(&serialized).unwrap();
        assert_eq!(reparsed.session_id, member.session_id);
        assert_eq!(reparsed.external_backend_type, member.external_backend_type);
        assert_eq!(reparsed.external_model, member.external_model);
    }

    #[test]
    fn test_effective_backend_type_prefers_external() {
        let mut member = make_minimal_member();
        member.backend_type = Some("tmux".to_string());
        member.external_backend_type = Some(BackendType::Codex);
        // external_backend_type takes precedence
        assert_eq!(member.effective_backend_type(), Some(BackendType::Codex));
    }

    #[test]
    fn test_effective_backend_type_falls_back_to_legacy() {
        let mut member = make_minimal_member();
        member.backend_type = Some("claude-code".to_string());
        member.external_backend_type = None;
        assert_eq!(
            member.effective_backend_type(),
            Some(BackendType::ClaudeCode)
        );
    }

    #[test]
    fn test_effective_backend_type_none_when_both_absent() {
        let member = make_minimal_member();
        assert!(member.effective_backend_type().is_none());
    }

    #[test]
    fn test_effective_backend_type_unparseable_legacy_returns_none() {
        let mut member = make_minimal_member();
        member.backend_type = Some("tmux".to_string()); // "tmux" is not a BackendType variant
        member.external_backend_type = None;
        // "tmux" does not map to any BackendType variant → falls back to None
        assert!(member.effective_backend_type().is_none());
    }

    // ── BackendType tests ─────────────────────────────────────────────────

    #[test]
    fn backend_type_known_variants_roundtrip() {
        let cases = [
            (BackendType::ClaudeCode, "claude-code"),
            (BackendType::Codex, "codex"),
            (BackendType::Gemini, "gemini"),
            (BackendType::External, "external"),
        ];
        for (variant, s) in &cases {
            assert_eq!(variant.to_string(), *s);
            assert_eq!(BackendType::from_str(s).unwrap(), *variant);
        }
    }

    #[test]
    fn backend_type_human_with_username() {
        let bt = BackendType::from_str("human:randlee").unwrap();
        assert_eq!(bt, BackendType::Human("randlee".to_string()));
        assert_eq!(bt.to_string(), "human:randlee");
    }

    #[test]
    fn backend_type_human_without_username_rejected() {
        let err = BackendType::from_str("human:").unwrap_err();
        assert!(err.contains("requires a username"), "error was: {err}");
    }

    #[test]
    fn backend_type_unknown_string_rejected() {
        let err = BackendType::from_str("alien-ai").unwrap_err();
        assert!(err.contains("Unknown backend type"), "error was: {err}");
    }

    #[test]
    fn backend_type_serde_roundtrip() {
        let bt = BackendType::Human("alice".to_string());
        let json = serde_json::to_string(&bt).unwrap();
        assert_eq!(json, r#""human:alice""#);
        let parsed: BackendType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, bt);
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    fn make_minimal_member() -> AgentMember {
        AgentMember {
            agent_id: "test@team".to_string(),
            name: "test".to_string(),
            agent_type: "general-purpose".to_string(),
            model: "unknown".to_string(),
            prompt: None,
            color: None,
            plan_mode_required: None,
            joined_at: 0,
            tmux_pane_id: None,
            cwd: "/".to_string(),
            subscriptions: Vec::new(),
            backend_type: None,
            is_active: None,
            last_active: None,
            session_id: None,
            external_backend_type: None,
            external_model: None,
            unknown_fields: HashMap::new(),
        }
    }
}
