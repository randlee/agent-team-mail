//! Configuration for the Worker Adapter plugin

use crate::plugin::PluginError;
use atm_core::toml;
use std::collections::HashMap;
use std::path::PathBuf;

/// Per-agent configuration
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Whether this agent is enabled for worker adapter
    pub enabled: bool,
    /// Prompt template for message formatting
    pub prompt_template: String,
    /// Concurrency policy: "queue" (default), "reject", or "concurrent"
    pub concurrency_policy: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            prompt_template: "{message}".to_string(),
            concurrency_policy: "queue".to_string(),
        }
    }
}

/// Configuration for the Worker Adapter plugin, parsed from [workers]
#[derive(Debug, Clone)]
pub struct WorkersConfig {
    /// Whether the worker adapter is enabled
    pub enabled: bool,
    /// Backend type (currently only "codex-tmux" is supported)
    pub backend: String,
    /// TMUX session name for worker panes
    pub tmux_session: String,
    /// Directory for worker log files
    pub log_dir: PathBuf,
    /// Inactivity timeout in milliseconds (default: 5 minutes)
    pub inactivity_timeout_ms: u64,
    /// Per-agent configuration
    pub agents: HashMap<String, AgentConfig>,
}

impl WorkersConfig {
    /// Parse configuration from TOML table
    ///
    /// # Arguments
    ///
    /// * `table` - The `[workers]` section from `daemon.toml`
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Config` if parsing fails or required fields are missing
    pub fn from_toml(table: &toml::Table) -> Result<Self, PluginError> {
        let enabled = table
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let backend = table
            .get("backend")
            .and_then(|v| v.as_str())
            .unwrap_or("codex-tmux")
            .to_string();

        let tmux_session = table
            .get("tmux_session")
            .and_then(|v| v.as_str())
            .unwrap_or("atm-workers")
            .to_string();

        // Log directory: default to ~/.config/atm/worker-logs or ATM_HOME/worker-logs
        let default_log_dir = if let Ok(atm_home) = std::env::var("ATM_HOME") {
            PathBuf::from(atm_home).join("worker-logs")
        } else {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("atm")
                .join("worker-logs")
        };

        let log_dir = table
            .get("log_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or(default_log_dir);

        let inactivity_timeout_ms = table
            .get("inactivity_timeout_ms")
            .and_then(|v| v.as_integer())
            .map(|i| i as u64)
            .unwrap_or(5 * 60 * 1000); // 5 minutes default

        // Parse per-agent configuration
        let mut agents = HashMap::new();
        if let Some(agents_table) = table.get("agents").and_then(|v| v.as_table()) {
            for (agent_name, agent_value) in agents_table {
                let agent_config = if let Some(agent_table) = agent_value.as_table() {
                    AgentConfig {
                        enabled: agent_table
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true),
                        prompt_template: agent_table
                            .get("prompt_template")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{message}")
                            .to_string(),
                        concurrency_policy: agent_table
                            .get("concurrency_policy")
                            .and_then(|v| v.as_str())
                            .unwrap_or("queue")
                            .to_string(),
                    }
                } else {
                    AgentConfig::default()
                };
                agents.insert(agent_name.clone(), agent_config);
            }
        }

        // Validate backend
        if backend != "codex-tmux" {
            return Err(PluginError::Config {
                message: format!("Unsupported worker backend: '{backend}'. Currently only 'codex-tmux' is supported."),
            });
        }

        Ok(Self {
            enabled,
            backend,
            tmux_session,
            log_dir,
            inactivity_timeout_ms,
            agents,
        })
    }
}

impl Default for WorkersConfig {
    fn default() -> Self {
        let default_log_dir = if let Ok(atm_home) = std::env::var("ATM_HOME") {
            PathBuf::from(atm_home).join("worker-logs")
        } else {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("atm")
                .join("worker-logs")
        };

        Self {
            enabled: false,
            backend: "codex-tmux".to_string(),
            tmux_session: "atm-workers".to_string(),
            log_dir: default_log_dir,
            inactivity_timeout_ms: 5 * 60 * 1000,
            agents: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = WorkersConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.backend, "codex-tmux");
        assert_eq!(config.tmux_session, "atm-workers");
    }

    #[test]
    fn test_config_from_toml_minimal() {
        let toml_str = r#""#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = WorkersConfig::from_toml(&table).unwrap();

        assert!(!config.enabled);
        assert_eq!(config.backend, "codex-tmux");
        assert_eq!(config.tmux_session, "atm-workers");
    }

    #[test]
    fn test_config_from_toml_complete() {
        let toml_str = r#"
enabled = true
backend = "codex-tmux"
tmux_session = "my-workers"
log_dir = "/var/log/atm-workers"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = WorkersConfig::from_toml(&table).unwrap();

        assert!(config.enabled);
        assert_eq!(config.backend, "codex-tmux");
        assert_eq!(config.tmux_session, "my-workers");
        assert_eq!(config.log_dir, PathBuf::from("/var/log/atm-workers"));
    }

    #[test]
    fn test_config_from_toml_partial() {
        let toml_str = r#"
enabled = true
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = WorkersConfig::from_toml(&table).unwrap();

        assert!(config.enabled);
        assert_eq!(config.backend, "codex-tmux"); // default
        assert_eq!(config.tmux_session, "atm-workers"); // default
    }

    #[test]
    fn test_config_invalid_backend() {
        let toml_str = r#"
enabled = true
backend = "unsupported-backend"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = WorkersConfig::from_toml(&table);

        assert!(result.is_err());
        if let Err(PluginError::Config { message }) = result {
            assert!(message.contains("Unsupported worker backend"));
            assert!(message.contains("unsupported-backend"));
        } else {
            panic!("Expected Config error");
        }
    }

    #[test]
    fn test_config_from_toml_invalid_types_use_defaults() {
        let toml_str = r#"
enabled = "yes"
backend = 123
tmux_session = false
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = WorkersConfig::from_toml(&table).unwrap();

        // Invalid types fall back to defaults
        assert!(!config.enabled); // default
        assert_eq!(config.backend, "codex-tmux"); // default
        assert_eq!(config.tmux_session, "atm-workers"); // default
    }

    #[test]
    fn test_config_atm_home_env() {
        unsafe {
            std::env::set_var("ATM_HOME", "/custom/atm");
        }
        let config = WorkersConfig::default();
        assert_eq!(config.log_dir, PathBuf::from("/custom/atm/worker-logs"));
        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }
}
