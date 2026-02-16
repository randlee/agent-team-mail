//! Configuration for the CI Monitor plugin

use crate::plugin::PluginError;
use super::types::CiRunConclusion;
use agent_team_mail_core::toml;
use globset::{GlobSet, GlobSetBuilder};
use std::collections::HashMap;
use std::path::PathBuf;

/// Deduplication strategy for CI runs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupStrategy {
    /// Deduplicate per commit (notify once per commit+conclusion)
    PerCommit,
    /// Deduplicate per run (notify once per run_id+conclusion)
    PerRun,
}

/// Notification routing target for CI alerts
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyTarget {
    /// Agent name to notify
    pub agent: String,
    /// Team name (None = use config team)
    pub team: Option<String>,
}

impl NotifyTarget {
    /// Parse a notify target from a string in the format "agent" or "agent@team"
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Config` if the format is invalid (e.g., empty, multiple @)
    pub fn parse(s: &str) -> Result<Self, PluginError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(PluginError::Config {
                message: "notify_target cannot be empty".to_string(),
            });
        }

        let parts: Vec<&str> = s.split('@').collect();
        match parts.len() {
            1 => Ok(Self {
                agent: parts[0].to_string(),
                team: None,
            }),
            2 => {
                if parts[0].is_empty() || parts[1].is_empty() {
                    return Err(PluginError::Config {
                        message: format!("Invalid notify_target format: '{s}'"),
                    });
                }
                Ok(Self {
                    agent: parts[0].to_string(),
                    team: Some(parts[1].to_string()),
                })
            }
            _ => Err(PluginError::Config {
                message: format!("Invalid notify_target format (multiple @): '{s}'"),
            }),
        }
    }
}

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
    /// Deduplication strategy
    pub dedup_strategy: DedupStrategy,
    /// Deduplication cache TTL in hours
    pub dedup_ttl_hours: u64,
    /// Report directory for failure reports (JSON + Markdown)
    pub report_dir: PathBuf,
    /// Provider-specific configuration (passed to external providers)
    pub provider_config: Option<toml::Table>,
    /// Notification routing targets (empty = send as ci-monitor agent)
    pub notify_target: Vec<NotifyTarget>,
    /// Compiled glob matcher for watched_branches (None = match all)
    pub branch_matcher: Option<GlobSet>,
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

        let watched_branches: Vec<String> = table
            .get("watched_branches")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        // Compile glob patterns for branch matching
        let branch_matcher = if watched_branches.is_empty() {
            None
        } else {
            let mut builder = GlobSetBuilder::new();
            for pattern in &watched_branches {
                builder.add(
                    globset::Glob::new(pattern).map_err(|e| PluginError::Config {
                        message: format!("Invalid glob pattern '{}': {}", pattern, e),
                    })?,
                );
            }
            Some(builder.build().map_err(|e| PluginError::Config {
                message: format!("Failed to build glob set: {}", e),
            })?)
        };

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

        let dedup_strategy = table
            .get("dedup_strategy")
            .and_then(|v| v.as_str())
            .map(|s| match s.to_lowercase().as_str() {
                "per_run" => DedupStrategy::PerRun,
                _ => DedupStrategy::PerCommit,
            })
            .unwrap_or(DedupStrategy::PerCommit);

        let dedup_ttl_hours = table
            .get("dedup_ttl_hours")
            .and_then(|v| v.as_integer())
            .map(|v| v as u64)
            .unwrap_or(24);

        let report_dir = table
            .get("report_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("temp/atm/ci-monitor"));

        // Parse notify_target (can be single string or array of strings)
        let notify_target = match table.get("notify_target") {
            Some(toml::Value::String(s)) => vec![NotifyTarget::parse(s)?],
            Some(toml::Value::Array(arr)) => {
                let mut targets = Vec::new();
                for val in arr {
                    if let Some(s) = val.as_str() {
                        targets.push(NotifyTarget::parse(s)?);
                    }
                }
                targets
            }
            _ => Vec::new(),
        };

        // Extract provider_config for external providers
        let provider_config = table.clone();

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
            dedup_strategy,
            dedup_ttl_hours,
            report_dir,
            provider_config: Some(provider_config),
            notify_target,
            branch_matcher,
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
            dedup_strategy: DedupStrategy::PerCommit,
            dedup_ttl_hours: 24,
            report_dir: PathBuf::from("temp/atm/ci-monitor"),
            provider_config: None,
            notify_target: Vec::new(),
            branch_matcher: None,
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
        assert_eq!(config.dedup_strategy, DedupStrategy::PerCommit);
        assert_eq!(config.dedup_ttl_hours, 24);
        assert_eq!(config.report_dir, PathBuf::from("temp/atm/ci-monitor"));
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

    // Branch matching tests
    #[test]
    fn test_branch_matcher_empty_matches_all() {
        let toml_str = r#"
team = "dev-team"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        assert!(config.branch_matcher.is_none());
    }

    #[test]
    fn test_branch_matcher_exact_match() {
        let toml_str = r#"
team = "dev-team"
watched_branches = ["main"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        let matcher = config.branch_matcher.as_ref().unwrap();
        assert!(matcher.is_match("main"));
        assert!(!matcher.is_match("develop"));
    }

    #[test]
    fn test_branch_matcher_wildcard() {
        let toml_str = r#"
team = "dev-team"
watched_branches = ["*"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        let matcher = config.branch_matcher.as_ref().unwrap();
        assert!(matcher.is_match("main"));
        assert!(matcher.is_match("develop"));
        assert!(matcher.is_match("feature/test"));
    }

    #[test]
    fn test_branch_matcher_glob_pattern() {
        let toml_str = r#"
team = "dev-team"
watched_branches = ["release/*"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        let matcher = config.branch_matcher.as_ref().unwrap();
        assert!(matcher.is_match("release/v1.0"));
        assert!(matcher.is_match("release/v2.5"));
        assert!(!matcher.is_match("main"));
        // Note: * in globset matches path separators by default
        assert!(matcher.is_match("release/v1.0/hotfix"));
    }

    #[test]
    fn test_branch_matcher_nested_glob() {
        let toml_str = r#"
team = "dev-team"
watched_branches = ["feature/**"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        let matcher = config.branch_matcher.as_ref().unwrap();
        assert!(matcher.is_match("feature/test"));
        assert!(matcher.is_match("feature/deep/nested/branch"));
        assert!(!matcher.is_match("main"));
    }

    #[test]
    fn test_branch_matcher_multiple_patterns() {
        let toml_str = r#"
team = "dev-team"
watched_branches = ["main", "release/*", "feature/important"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        let matcher = config.branch_matcher.as_ref().unwrap();
        assert!(matcher.is_match("main"));
        assert!(matcher.is_match("release/v1.0"));
        assert!(matcher.is_match("feature/important"));
        assert!(!matcher.is_match("develop"));
    }

    #[test]
    fn test_branch_matcher_no_match() {
        let toml_str = r#"
team = "dev-team"
watched_branches = ["main"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        let matcher = config.branch_matcher.as_ref().unwrap();
        assert!(!matcher.is_match("develop"));
    }

    #[test]
    fn test_branch_matcher_invalid_pattern() {
        let toml_str = r#"
team = "dev-team"
watched_branches = ["[invalid"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = CiMonitorConfig::from_toml(&table);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Invalid glob pattern"));
        assert!(err.contains("[invalid"));
    }

    #[test]
    fn test_branch_matcher_case_sensitive() {
        let toml_str = r#"
team = "dev-team"
watched_branches = ["Main"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        let matcher = config.branch_matcher.as_ref().unwrap();
        assert!(matcher.is_match("Main"));
        assert!(!matcher.is_match("main"));
    }

    #[test]
    fn test_branch_matcher_question_mark_wildcard() {
        let toml_str = r#"
team = "dev-team"
watched_branches = ["v?.0"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        let matcher = config.branch_matcher.as_ref().unwrap();
        assert!(matcher.is_match("v1.0"));
        assert!(matcher.is_match("v2.0"));
        assert!(!matcher.is_match("v10.0"));
    }

    // Routing tests
    #[test]
    fn test_notify_target_single() {
        let toml_str = r#"
team = "dev-team"
notify_target = "team-lead"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        assert_eq!(config.notify_target.len(), 1);
        assert_eq!(config.notify_target[0].agent, "team-lead");
        assert!(config.notify_target[0].team.is_none());
    }

    #[test]
    fn test_notify_target_multiple() {
        let toml_str = r#"
team = "dev-team"
notify_target = ["team-lead", "dev-bot"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        assert_eq!(config.notify_target.len(), 2);
        assert_eq!(config.notify_target[0].agent, "team-lead");
        assert_eq!(config.notify_target[1].agent, "dev-bot");
    }

    #[test]
    fn test_notify_target_empty_default() {
        let toml_str = r#"
team = "dev-team"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        assert!(config.notify_target.is_empty());
    }

    #[test]
    fn test_notify_target_with_team() {
        let toml_str = r#"
team = "dev-team"
notify_target = "agent@other-team"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        assert_eq!(config.notify_target.len(), 1);
        assert_eq!(config.notify_target[0].agent, "agent");
        assert_eq!(config.notify_target[0].team, Some("other-team".to_string()));
    }

    #[test]
    fn test_notify_target_invalid_empty() {
        let toml_str = r#"
team = "dev-team"
notify_target = ""
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = CiMonitorConfig::from_toml(&table);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[test]
    fn test_notify_target_invalid_multiple_at() {
        let toml_str = r#"
team = "dev-team"
notify_target = "agent@team@extra"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = CiMonitorConfig::from_toml(&table);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("multiple @"));
    }

    // Config validation tests
    #[test]
    fn test_notify_target_parse_valid() {
        let target = NotifyTarget::parse("agent-name").unwrap();
        assert_eq!(target.agent, "agent-name");
        assert!(target.team.is_none());

        let target = NotifyTarget::parse("agent@team").unwrap();
        assert_eq!(target.agent, "agent");
        assert_eq!(target.team, Some("team".to_string()));
    }

    #[test]
    fn test_notify_target_parse_empty_parts() {
        let result = NotifyTarget::parse("@team");
        assert!(result.is_err());

        let result = NotifyTarget::parse("agent@");
        assert!(result.is_err());
    }

    #[test]
    fn test_config_round_trip() {
        let toml_str = r#"
team = "dev-team"
watched_branches = ["main", "release/*"]
notify_target = ["lead", "bot@other"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        assert_eq!(config.team, "dev-team");
        assert_eq!(config.watched_branches, vec!["main", "release/*"]);
        assert_eq!(config.notify_target.len(), 2);
        assert_eq!(config.notify_target[0].agent, "lead");
        assert_eq!(config.notify_target[1].agent, "bot");
        assert!(config.branch_matcher.is_some());
    }
}
