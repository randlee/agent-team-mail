//! Configuration types

use serde::{Deserialize, Serialize};

/// Complete configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Core configuration
    #[serde(default)]
    pub core: CoreConfig,
    /// Display configuration
    #[serde(default)]
    pub display: DisplayConfig,
    /// Retention configuration
    #[serde(default)]
    pub retention: RetentionConfig,
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
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            max_age: None,
            max_count: None,
            strategy: CleanupStrategy::Delete,
            archive_dir: None,
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
}
