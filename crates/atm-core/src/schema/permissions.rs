//! Permissions schema for Claude Code settings

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Permissions configuration
///
/// Controls tool access and file reads in Claude Code settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Permissions {
    /// Allowed operations (e.g., "Bash(npm run lint)", "Read(~/.zshrc)")
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,

    /// Denied operations
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,

    /// Operations requiring user confirmation
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ask: Vec<String>,

    /// Unknown fields for forward compatibility
    #[serde(flatten)]
    pub unknown_fields: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permissions_roundtrip_minimal() {
        let json = r#"{}"#;

        let perms: Permissions = serde_json::from_str(json).unwrap();
        assert!(perms.allow.is_empty());
        assert!(perms.deny.is_empty());
        assert!(perms.ask.is_empty());

        let serialized = serde_json::to_string(&perms).unwrap();
        let reparsed: Permissions = serde_json::from_str(&serialized).unwrap();
        assert!(reparsed.allow.is_empty());
    }

    #[test]
    fn test_permissions_roundtrip_complete() {
        let json = r#"{
            "allow": ["Bash(npm run lint)", "Read(~/.zshrc)"],
            "deny": ["Bash(curl *)", "Read(./secrets/**)"],
            "ask": ["Bash(rm -rf *)"]
        }"#;

        let perms: Permissions = serde_json::from_str(json).unwrap();
        assert_eq!(perms.allow.len(), 2);
        assert_eq!(perms.allow[0], "Bash(npm run lint)");
        assert_eq!(perms.deny.len(), 2);
        assert_eq!(perms.deny[0], "Bash(curl *)");
        assert_eq!(perms.ask.len(), 1);
        assert_eq!(perms.ask[0], "Bash(rm -rf *)");

        let serialized = serde_json::to_string(&perms).unwrap();
        let reparsed: Permissions = serde_json::from_str(&serialized).unwrap();
        assert_eq!(perms.allow.len(), reparsed.allow.len());
        assert_eq!(perms.deny.len(), reparsed.deny.len());
        assert_eq!(perms.ask.len(), reparsed.ask.len());
    }

    #[test]
    fn test_permissions_roundtrip_with_unknown_fields() {
        let json = r#"{
            "allow": ["Bash(npm test)"],
            "unknownField": "value",
            "futureFeature": {"nested": "data"}
        }"#;

        let perms: Permissions = serde_json::from_str(json).unwrap();
        assert_eq!(perms.allow.len(), 1);
        assert_eq!(perms.unknown_fields.len(), 2);
        assert!(perms.unknown_fields.contains_key("unknownField"));
        assert!(perms.unknown_fields.contains_key("futureFeature"));

        let serialized = serde_json::to_string(&perms).unwrap();
        let reparsed: Permissions = serde_json::from_str(&serialized).unwrap();
        assert_eq!(perms.unknown_fields.len(), reparsed.unknown_fields.len());
        assert_eq!(
            perms.unknown_fields.get("unknownField"),
            reparsed.unknown_fields.get("unknownField")
        );
    }

    #[test]
    fn test_permissions_missing_optional_fields() {
        let json = r#"{"allow": ["Bash(test)"]}"#;

        let perms: Permissions = serde_json::from_str(json).unwrap();
        assert_eq!(perms.allow.len(), 1);
        assert!(perms.deny.is_empty());
        assert!(perms.ask.is_empty());
    }
}
