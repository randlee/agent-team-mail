//! Configuration for the Issues plugin

use crate::plugin::PluginError;
use agent_team_mail_core::toml;
use std::collections::HashMap;
use std::path::PathBuf;

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
    /// Override which provider to use (instead of auto-detecting from git remote)
    pub provider: Option<String>,
    /// Additional provider libraries to load: provider_name -> library_path
    pub provider_libraries: HashMap<String, PathBuf>,
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

        let provider = table
            .get("provider")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let provider_libraries = table
            .get("providers")
            .and_then(|v| v.as_table())
            .map(|providers_table| {
                providers_table
                    .iter()
                    .filter_map(|(name, value)| {
                        value
                            .as_str()
                            .map(|path_str| (name.clone(), PathBuf::from(path_str)))
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            enabled,
            poll_interval,
            labels,
            assignees,
            team,
            agent,
            provider,
            provider_libraries,
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
            provider: None,
            provider_libraries: HashMap::new(),
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

    #[test]
    fn test_config_provider_override() {
        let toml_str = r#"
provider = "github"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = IssuesConfig::from_toml(&table).unwrap();

        assert_eq!(config.provider, Some("github".to_string()));
        assert!(config.provider_libraries.is_empty());
    }

    #[test]
    fn test_config_provider_libraries() {
        let toml_str = r#"
[providers]
gitlab = "/opt/atm/providers/libatm_provider_gitlab.dylib"
jira = "~/.config/atm/providers/libatm_provider_jira.dylib"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = IssuesConfig::from_toml(&table).unwrap();

        assert_eq!(config.provider, None);
        assert_eq!(config.provider_libraries.len(), 2);
        assert_eq!(
            config.provider_libraries.get("gitlab"),
            Some(&PathBuf::from(
                "/opt/atm/providers/libatm_provider_gitlab.dylib"
            ))
        );
        assert_eq!(
            config.provider_libraries.get("jira"),
            Some(&PathBuf::from(
                "~/.config/atm/providers/libatm_provider_jira.dylib"
            ))
        );
    }

    #[test]
    fn test_config_provider_and_libraries() {
        let toml_str = r#"
provider = "gitlab"

[providers]
gitlab = "/path/to/gitlab.dylib"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = IssuesConfig::from_toml(&table).unwrap();

        assert_eq!(config.provider, Some("gitlab".to_string()));
        assert_eq!(config.provider_libraries.len(), 1);
    }
}
