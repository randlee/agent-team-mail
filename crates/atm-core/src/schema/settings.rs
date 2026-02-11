//! Settings schema for Claude Code configuration

use super::Permissions;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Claude Code settings
///
/// Stored in:
/// - User scope: `~/.claude/settings.json`
/// - Project scope: `.claude/settings.json`
/// - Local scope: `.claude/settings.local.json`
/// - Managed scope: System-specific locations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsJson {
    /// JSON schema reference
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,

    /// Permissions configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Permissions>,

    /// Environment variables
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    /// Unknown fields for forward compatibility
    /// Includes: hooks, model, status line, plugin settings, etc.
    #[serde(flatten)]
    pub unknown_fields: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_roundtrip_minimal() {
        let json = r#"{}"#;

        let settings: SettingsJson = serde_json::from_str(json).unwrap();
        assert!(settings.schema.is_none());
        assert!(settings.permissions.is_none());
        assert!(settings.env.is_empty());

        let serialized = serde_json::to_string(&settings).unwrap();
        let reparsed: SettingsJson = serde_json::from_str(&serialized).unwrap();
        assert!(reparsed.schema.is_none());
    }

    #[test]
    fn test_settings_roundtrip_complete() {
        let json = r#"{
            "$schema": "https://json.schemastore.org/claude-code-settings.json",
            "permissions": {
                "allow": ["Bash(npm run lint)", "Read(~/.zshrc)"],
                "deny": ["Bash(curl *)", "Read(./secrets/**)"]
            },
            "env": {
                "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
                "NODE_ENV": "development"
            }
        }"#;

        let settings: SettingsJson = serde_json::from_str(json).unwrap();
        assert_eq!(
            settings.schema,
            Some("https://json.schemastore.org/claude-code-settings.json".to_string())
        );
        assert!(settings.permissions.is_some());
        let perms = settings.permissions.as_ref().unwrap();
        assert_eq!(perms.allow.len(), 2);
        assert_eq!(perms.deny.len(), 2);
        assert_eq!(settings.env.len(), 2);
        assert_eq!(settings.env.get("CLAUDE_CODE_ENABLE_TELEMETRY").unwrap(), "1");

        let serialized = serde_json::to_string(&settings).unwrap();
        let reparsed: SettingsJson = serde_json::from_str(&serialized).unwrap();
        assert_eq!(settings.schema, reparsed.schema);
        assert_eq!(settings.env.len(), reparsed.env.len());
    }

    #[test]
    fn test_settings_roundtrip_with_unknown_fields() {
        let json = r#"{
            "$schema": "https://json.schemastore.org/claude-code-settings.json",
            "permissions": {
                "allow": ["Bash(test)"]
            },
            "env": {
                "TEST": "value"
            },
            "hooks": {
                "pre-commit": "npm test"
            },
            "model": "claude-opus-4-6",
            "unknownField": "value",
            "futureFeature": {
                "nested": "data"
            }
        }"#;

        let settings: SettingsJson = serde_json::from_str(json).unwrap();
        assert!(settings.schema.is_some());
        assert!(settings.permissions.is_some());
        assert_eq!(settings.env.len(), 1);
        // hooks, model, unknownField, futureFeature should be in unknown_fields
        assert!(settings.unknown_fields.contains_key("hooks"));
        assert!(settings.unknown_fields.contains_key("model"));
        assert!(settings.unknown_fields.contains_key("unknownField"));
        assert!(settings.unknown_fields.contains_key("futureFeature"));

        let serialized = serde_json::to_string(&settings).unwrap();
        let reparsed: SettingsJson = serde_json::from_str(&serialized).unwrap();
        assert_eq!(settings.unknown_fields.len(), reparsed.unknown_fields.len());
        assert_eq!(
            settings.unknown_fields.get("model"),
            reparsed.unknown_fields.get("model")
        );
        assert_eq!(
            settings.unknown_fields.get("hooks"),
            reparsed.unknown_fields.get("hooks")
        );
    }

    #[test]
    fn test_settings_from_documentation_example() {
        // From agent-team-api.md lines 933-945
        let json = r#"{
            "$schema": "https://json.schemastore.org/claude-code-settings.json",
            "permissions": {
                "allow": ["Bash(npm run lint)", "Read(~/.zshrc)"],
                "deny": ["Bash(curl *)", "Read(./secrets/**)"]
            },
            "env": {
                "CLAUDE_CODE_ENABLE_TELEMETRY": "1"
            }
        }"#;

        let settings: SettingsJson = serde_json::from_str(json).unwrap();
        assert_eq!(
            settings.schema,
            Some("https://json.schemastore.org/claude-code-settings.json".to_string())
        );
        assert!(settings.permissions.is_some());
        assert_eq!(settings.env.len(), 1);

        // Verify round-trip
        let serialized = serde_json::to_string_pretty(&settings).unwrap();
        let reparsed: SettingsJson = serde_json::from_str(&serialized).unwrap();
        assert_eq!(settings.schema, reparsed.schema);
    }

    #[test]
    fn test_settings_missing_optional_fields() {
        let json = r#"{"env": {"TEST": "1"}}"#;

        let settings: SettingsJson = serde_json::from_str(json).unwrap();
        assert!(settings.schema.is_none());
        assert!(settings.permissions.is_none());
        assert_eq!(settings.env.len(), 1);
    }
}
