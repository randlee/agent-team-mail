//! Configuration types

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Complete configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Core configuration
    #[serde(default)]
    pub core: CoreConfig,
    /// Display configuration
    #[serde(default)]
    pub display: DisplayConfig,
    /// Messaging configuration
    #[serde(default)]
    pub messaging: MessagingConfig,
    /// Retention configuration
    #[serde(default)]
    pub retention: RetentionConfig,
    /// Plugin-specific configuration sections: [plugins.<name>]
    #[serde(default)]
    pub plugins: HashMap<String, toml::Table>,
}

/// Core configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreConfig {
    /// Default team name
    pub default_team: String,
    /// Sender identity
    pub identity: String,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            default_team: "default".to_string(),
            identity: "human".to_string(),
        }
    }
}

/// Display configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayConfig {
    /// Output format
    pub format: OutputFormat,
    /// Enable colored output
    pub color: bool,
    /// Timestamp format
    pub timestamps: TimestampFormat,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            format: OutputFormat::Text,
            color: true,
            timestamps: TimestampFormat::Relative,
        }
    }
}

/// Output format
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// Plain text output
    Text,
    /// JSON output
    Json,
}

/// Messaging configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessagingConfig {
    /// Custom call-to-action text for offline recipients.
    /// If set to empty string, disables prepend entirely.
    #[serde(default)]
    pub offline_action: Option<String>,
}

/// Timestamp display format
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimestampFormat {
    /// Relative (e.g., "2 minutes ago")
    Relative,
    /// Absolute (e.g., "2:30 PM")
    Absolute,
    /// ISO 8601 (e.g., "2026-02-10T14:30:00Z")
    Iso8601,
}

/// Retention configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// Maximum message age (duration string: "7d", "24h", "30d")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age: Option<String>,
    /// Maximum message count per inbox
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_count: Option<usize>,
    /// Cleanup strategy: "delete" or "archive"
    #[serde(default = "default_strategy")]
    pub strategy: CleanupStrategy,
    /// Archive directory path (default: ~/.config/atm/archive/)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_dir: Option<String>,
    /// Enable daemon periodic retention (default: false)
    #[serde(default)]
    pub enabled: bool,
    /// Interval in seconds between daemon retention runs (default: 300 = 5 minutes)
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            max_age: None,
            max_count: None,
            strategy: CleanupStrategy::Delete,
            archive_dir: None,
            enabled: false,
            interval_secs: default_interval_secs(),
        }
    }
}

/// Cleanup strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CleanupStrategy {
    /// Delete old messages without archiving
    Delete,
    /// Archive old messages before removing from inbox
    Archive,
}

fn default_strategy() -> CleanupStrategy {
    CleanupStrategy::Delete
}

fn default_interval_secs() -> u64 {
    300 // 5 minutes
}

impl Config {
    /// Get a plugin's configuration section by name.
    /// Returns None if the plugin has no config section.
    pub fn plugin_config(&self, name: &str) -> Option<&toml::Table> {
        self.plugins.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = Config::default();
        assert_eq!(config.core.default_team, "default");
        assert_eq!(config.core.identity, "human");
        assert_eq!(config.display.format, OutputFormat::Text);
        assert!(config.display.color);
        assert_eq!(config.display.timestamps, TimestampFormat::Relative);
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let toml_str = toml::to_string(&config).unwrap();
        let deserialized: Config = toml::from_str(&toml_str).unwrap();

        assert_eq!(config.core.default_team, deserialized.core.default_team);
        assert_eq!(config.core.identity, deserialized.core.identity);
        assert_eq!(config.display.format, deserialized.display.format);
    }

    #[test]
    fn test_plugin_config_round_trip() {
        let toml_str = r#"
[core]
default_team = "test-team"
identity = "test-user"

[plugins.issues]
enabled = true
poll_interval = 60
labels = ["bug", "enhancement"]

[plugins.ci-monitor]
enabled = true
workflow = "ci.yml"
"#;

        let config: Config = toml::from_str(toml_str).unwrap();

        // Verify plugins were deserialized
        assert!(config.plugins.contains_key("issues"));
        assert!(config.plugins.contains_key("ci-monitor"));

        // Round-trip through serialization
        let reserialized = toml::to_string(&config).unwrap();
        let config2: Config = toml::from_str(&reserialized).unwrap();

        assert_eq!(config.plugins.len(), config2.plugins.len());
    }

    #[test]
    fn test_plugin_config_accessor() {
        let toml_str = r#"
[core]
default_team = "test-team"
identity = "test-user"

[plugins.issues]
enabled = true
poll_interval = 60
"#;

        let config: Config = toml::from_str(toml_str).unwrap();

        // Test successful lookup
        let issues_config = config.plugin_config("issues");
        assert!(issues_config.is_some());

        let table = issues_config.unwrap();
        assert!(table.contains_key("enabled"));
        assert!(table.contains_key("poll_interval"));
    }

    #[test]
    fn test_plugin_config_missing() {
        let config = Config::default();

        // Test lookup of non-existent plugin
        assert!(config.plugin_config("nonexistent").is_none());
    }

    #[test]
    fn test_plugin_config_empty() {
        let toml_str = r#"
[core]
default_team = "test-team"
identity = "test-user"
"#;

        let config: Config = toml::from_str(toml_str).unwrap();

        // Config without [plugins] section should have empty HashMap
        assert!(config.plugins.is_empty());
    }

    #[test]
    fn test_retention_config_defaults() {
        let config = RetentionConfig::default();
        assert_eq!(config.max_age, None);
        assert_eq!(config.max_count, None);
        assert_eq!(config.strategy, CleanupStrategy::Delete);
        assert_eq!(config.archive_dir, None);
        assert!(!config.enabled, "Retention should be disabled by default");
        assert_eq!(config.interval_secs, 300, "Default interval should be 5 minutes");
    }

    #[test]
    fn test_retention_config_serialization() {
        let toml_str = r#"
[retention]
max_age = "7d"
max_count = 1000
strategy = "archive"
archive_dir = "/tmp/archive"
enabled = true
interval_secs = 600
"#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.retention.max_age, Some("7d".to_string()));
        assert_eq!(config.retention.max_count, Some(1000));
        assert_eq!(config.retention.strategy, CleanupStrategy::Archive);
        assert_eq!(config.retention.archive_dir, Some("/tmp/archive".to_string()));
        assert!(config.retention.enabled);
        assert_eq!(config.retention.interval_secs, 600);
    }

    #[test]
    fn test_retention_config_backward_compatibility() {
        // Old config without enabled and interval_secs should still parse
        let toml_str = r#"
[retention]
max_age = "7d"
strategy = "delete"
"#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.retention.max_age, Some("7d".to_string()));
        assert_eq!(config.retention.strategy, CleanupStrategy::Delete);
        // New fields should use defaults
        assert!(!config.retention.enabled);
        assert_eq!(config.retention.interval_secs, 300);
    }

    #[test]
    fn test_retention_config_partial() {
        let toml_str = r#"
[retention]
enabled = true
interval_secs = 120
"#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.retention.enabled);
        assert_eq!(config.retention.interval_secs, 120);
        assert_eq!(config.retention.max_age, None);
        assert_eq!(config.retention.max_count, None);
        assert_eq!(config.retention.strategy, CleanupStrategy::Delete);
    }
}
