//! Configuration for the Issues plugin

use crate::plugin::PluginError;
use atm_core::toml;

/// Configuration for the Issues plugin, parsed from [plugins.issues]
#[derive(Debug, Clone)]
pub struct IssuesConfig {
    /// Whether the plugin is enabled
    pub enabled: bool,
    /// Polling interval in seconds
    pub poll_interval: u64,
    /// Issue labels to filter on (empty = all)
    pub labels: Vec<String>,
    /// Assignee usernames to filter on (empty = all)
    pub assignees: Vec<String>,
    /// Target team for posting issue notifications
    pub team: String,
    /// Synthetic agent name for posting messages
    pub agent: String,
}

impl IssuesConfig {
    /// Parse configuration from TOML table
    ///
    /// # Arguments
    ///
    /// * `table` - The `[plugins.issues]` section from `.atm.toml`
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Config` if parsing fails
    pub fn from_toml(table: &toml::Table) -> Result<Self, PluginError> {
        let enabled = table
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let poll_interval = table
            .get("poll_interval")
            .and_then(|v| v.as_integer())
            .map(|v| v as u64)
            .unwrap_or(300);

        let labels = table
            .get("labels")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let assignees = table
            .get("assignees")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let team = table
            .get("team")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let agent = table
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("issues-bot")
            .to_string();

        Ok(Self {
            enabled,
            poll_interval,
            labels,
            assignees,
            team,
            agent,
        })
    }

}

impl Default for IssuesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval: 300,
            labels: Vec::new(),
            assignees: Vec::new(),
            team: String::new(),
            agent: "issues-bot".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = IssuesConfig::default();
        assert!(config.enabled);
        assert_eq!(config.poll_interval, 300);
        assert!(config.labels.is_empty());
        assert!(config.assignees.is_empty());
        assert_eq!(config.team, "");
        assert_eq!(config.agent, "issues-bot");
    }

    #[test]
    fn test_config_from_toml_minimal() {
        let toml_str = r#""#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = IssuesConfig::from_toml(&table).unwrap();

        assert!(config.enabled);
        assert_eq!(config.poll_interval, 300);
        assert!(config.labels.is_empty());
        assert!(config.assignees.is_empty());
        assert_eq!(config.team, "");
        assert_eq!(config.agent, "issues-bot");
    }

    #[test]
    fn test_config_from_toml_complete() {
        let toml_str = r#"
enabled = false
poll_interval = 600
labels = ["bug", "agent-task"]
assignees = ["alice", "bob"]
team = "dev-team"
agent = "issue-tracker"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = IssuesConfig::from_toml(&table).unwrap();

        assert!(!config.enabled);
        assert_eq!(config.poll_interval, 600);
        assert_eq!(config.labels, vec!["bug", "agent-task"]);
        assert_eq!(config.assignees, vec!["alice", "bob"]);
        assert_eq!(config.team, "dev-team");
        assert_eq!(config.agent, "issue-tracker");
    }

    #[test]
    fn test_config_from_toml_partial() {
        let toml_str = r#"
enabled = true
labels = ["urgent"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = IssuesConfig::from_toml(&table).unwrap();

        assert!(config.enabled);
        assert_eq!(config.poll_interval, 300); // default
        assert_eq!(config.labels, vec!["urgent"]);
        assert!(config.assignees.is_empty()); // default
        assert_eq!(config.team, ""); // default
        assert_eq!(config.agent, "issues-bot"); // default
    }

    #[test]
    fn test_config_from_toml_invalid_types_use_defaults() {
        let toml_str = r#"
enabled = "yes"
poll_interval = "300"
labels = "not-an-array"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = IssuesConfig::from_toml(&table).unwrap();

        // Invalid types fall back to defaults
        assert!(config.enabled); // default
        assert_eq!(config.poll_interval, 300); // default
        assert!(config.labels.is_empty()); // default
    }
}
