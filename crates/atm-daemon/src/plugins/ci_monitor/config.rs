//! Configuration for the CI Monitor plugin

use crate::plugin::PluginError;
use super::types::CiRunConclusion;
use atm_core::toml;
use std::collections::HashMap;
use std::path::PathBuf;

/// Configuration for the CI Monitor plugin, parsed from [plugins.ci_monitor]
#[derive(Debug, Clone)]
pub struct CiMonitorConfig {
    /// Whether the plugin is enabled
    pub enabled: bool,
    /// Provider name (e.g., "github", "azure-pipelines")
    pub provider: String,
    /// Polling interval in seconds
    pub poll_interval_secs: u64,
    /// Repository owner/org (optional, auto-detected from git remote)
    pub owner: Option<String>,
    /// Repository name (optional, auto-detected from git remote)
    pub repo: Option<String>,
    /// Target team for posting CI notifications
    pub team: String,
    /// Synthetic agent name for posting messages
    pub agent: String,
    /// Branches to watch (empty = all branches)
    pub watched_branches: Vec<String>,
    /// Which conclusions trigger notifications
    pub notify_on: Vec<CiRunConclusion>,
    /// Additional provider libraries to load: provider_name -> library_path
    pub provider_libraries: HashMap<String, PathBuf>,
}

impl CiMonitorConfig {
    /// Parse configuration from TOML table
    ///
    /// # Arguments
    ///
    /// * `table` - The `[plugins.ci_monitor]` section from `.atm.toml`
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Config` if:
    /// - `team` is empty (required field)
    /// - `poll_interval_secs` is less than 10 (too aggressive)
    pub fn from_toml(table: &toml::Table) -> Result<Self, PluginError> {
        let enabled = table
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let provider = table
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("github")
            .to_string();

        let poll_interval_secs = table
            .get("poll_interval_secs")
            .and_then(|v| v.as_integer())
            .map(|v| v as u64)
            .unwrap_or(60);

        // Validate minimum poll interval
        if poll_interval_secs < 10 {
            return Err(PluginError::Config {
                message: "poll_interval_secs must be at least 10 seconds".to_string(),
            });
        }

        let owner = table
            .get("owner")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let repo = table
            .get("repo")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let team = table
            .get("team")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Validate team is required
        if team.is_empty() {
            return Err(PluginError::Config {
                message: "team is required in [plugins.ci_monitor]".to_string(),
            });
        }

        let agent = table
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("ci-monitor")
            .to_string();

        let watched_branches = table
            .get("watched_branches")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let notify_on = table
            .get("notify_on")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        v.as_str().and_then(|s| match s.to_lowercase().as_str() {
                            "failure" => Some(CiRunConclusion::Failure),
                            "timedout" | "timed_out" => Some(CiRunConclusion::TimedOut),
                            "cancelled" => Some(CiRunConclusion::Cancelled),
                            "action_required" => Some(CiRunConclusion::ActionRequired),
                            _ => None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_else(|| vec![CiRunConclusion::Failure, CiRunConclusion::TimedOut]);

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
            provider,
            poll_interval_secs,
            owner,
            repo,
            team,
            agent,
            watched_branches,
            notify_on,
            provider_libraries,
        })
    }
}

impl Default for CiMonitorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: "github".to_string(),
            poll_interval_secs: 60,
            owner: None,
            repo: None,
            team: String::new(),
            agent: "ci-monitor".to_string(),
            watched_branches: Vec::new(),
            notify_on: vec![CiRunConclusion::Failure, CiRunConclusion::TimedOut],
            provider_libraries: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = CiMonitorConfig::default();
        assert!(config.enabled);
        assert_eq!(config.provider, "github");
        assert_eq!(config.poll_interval_secs, 60);
        assert!(config.owner.is_none());
        assert!(config.repo.is_none());
        assert_eq!(config.team, "");
        assert_eq!(config.agent, "ci-monitor");
        assert!(config.watched_branches.is_empty());
        assert_eq!(
            config.notify_on,
            vec![CiRunConclusion::Failure, CiRunConclusion::TimedOut]
        );
    }

    #[test]
    fn test_config_from_toml_minimal() {
        let toml_str = r#"
team = "dev-team"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        assert!(config.enabled);
        assert_eq!(config.provider, "github");
        assert_eq!(config.poll_interval_secs, 60);
        assert_eq!(config.team, "dev-team");
        assert_eq!(config.agent, "ci-monitor");
    }

    #[test]
    fn test_config_from_toml_complete() {
        let toml_str = r#"
enabled = false
provider = "azure-pipelines"
poll_interval_secs = 120
owner = "myorg"
repo = "myrepo"
team = "qa-team"
agent = "ci-bot"
watched_branches = ["main", "develop"]
notify_on = ["failure", "timed_out", "cancelled"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        assert!(!config.enabled);
        assert_eq!(config.provider, "azure-pipelines");
        assert_eq!(config.poll_interval_secs, 120);
        assert_eq!(config.owner, Some("myorg".to_string()));
        assert_eq!(config.repo, Some("myrepo".to_string()));
        assert_eq!(config.team, "qa-team");
        assert_eq!(config.agent, "ci-bot");
        assert_eq!(config.watched_branches, vec!["main", "develop"]);
        assert_eq!(
            config.notify_on,
            vec![
                CiRunConclusion::Failure,
                CiRunConclusion::TimedOut,
                CiRunConclusion::Cancelled
            ]
        );
    }

    #[test]
    fn test_config_team_required() {
        let toml_str = r#"
enabled = true
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = CiMonitorConfig::from_toml(&table);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("team is required"));
    }

    #[test]
    fn test_config_poll_interval_minimum() {
        let toml_str = r#"
team = "dev-team"
poll_interval_secs = 5
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = CiMonitorConfig::from_toml(&table);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("at least 10 seconds"));
    }

    #[test]
    fn test_config_provider_libraries() {
        let toml_str = r#"
team = "dev-team"

[providers]
azure = "/opt/atm/providers/libatm_ci_azure.dylib"
gitlab = "~/.config/atm/providers/libatm_ci_gitlab.dylib"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        assert_eq!(config.provider_libraries.len(), 2);
        assert_eq!(
            config.provider_libraries.get("azure"),
            Some(&PathBuf::from("/opt/atm/providers/libatm_ci_azure.dylib"))
        );
        assert_eq!(
            config.provider_libraries.get("gitlab"),
            Some(&PathBuf::from("~/.config/atm/providers/libatm_ci_gitlab.dylib"))
        );
    }

    #[test]
    fn test_config_watched_branches() {
        let toml_str = r#"
team = "dev-team"
watched_branches = ["main", "release/*", "feature/important"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        assert_eq!(
            config.watched_branches,
            vec!["main", "release/*", "feature/important"]
        );
    }

    #[test]
    fn test_config_notify_on_parsing() {
        let toml_str = r#"
team = "dev-team"
notify_on = ["failure", "cancelled", "action_required"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        assert_eq!(
            config.notify_on,
            vec![
                CiRunConclusion::Failure,
                CiRunConclusion::Cancelled,
                CiRunConclusion::ActionRequired
            ]
        );
    }
}
