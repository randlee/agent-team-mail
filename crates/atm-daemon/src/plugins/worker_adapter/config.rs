//! Configuration for the Worker Adapter plugin

use crate::plugin::PluginError;
use atm_core::toml;
use std::collections::HashMap;
use std::path::PathBuf;

/// Default startup command for worker agents
pub const DEFAULT_COMMAND: &str = "codex --yolo";

/// Per-agent configuration
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Whether this agent is enabled for worker adapter
    pub enabled: bool,
    /// Startup command override (if None, uses WorkersConfig.command)
    pub command: Option<String>,
    /// Prompt template for message formatting
    pub prompt_template: String,
    /// Concurrency policy: "queue" (default), "reject", or "concurrent"
    pub concurrency_policy: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            command: None,
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
    /// Default startup command for workers (default: "codex --yolo")
    /// Per-agent override via agents.<name>.command
    pub command: String,
    /// TMUX session name for worker panes
    pub tmux_session: String,
    /// Directory for worker log files
    pub log_dir: PathBuf,
    /// Inactivity timeout in milliseconds (default: 5 minutes)
    pub inactivity_timeout_ms: u64,
    /// Health check interval in seconds (default: 30)
    pub health_check_interval_secs: u64,
    /// Maximum restart attempts before giving up (default: 3)
    pub max_restart_attempts: u32,
    /// Backoff duration between restarts in seconds (default: 5)
    pub restart_backoff_secs: u64,
    /// Graceful shutdown timeout in seconds (default: 10)
    pub shutdown_timeout_secs: u64,
    /// Per-agent configuration
    pub agents: HashMap<String, AgentConfig>,
}

impl WorkersConfig {
    /// Validate backend name
    ///
    /// # Arguments
    ///
    /// * `backend` - Backend name to validate
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Config` if backend is not supported
    pub fn validate_backend(backend: &str) -> Result<(), PluginError> {
        if backend != "codex-tmux" {
            return Err(PluginError::Config {
                message: format!(
                    "Unsupported worker backend: '{backend}'. Currently only 'codex-tmux' is supported."
                ),
            });
        }
        Ok(())
    }

    /// Validate tmux session name
    ///
    /// # Arguments
    ///
    /// * `session_name` - Session name to validate
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Config` if session name is invalid
    pub fn validate_tmux_session(session_name: &str) -> Result<(), PluginError> {
        if session_name.is_empty() {
            return Err(PluginError::Config {
                message: "TMUX session name cannot be empty".to_string(),
            });
        }

        // TMUX session names cannot contain colons, dots, or periods
        if session_name.contains(':') || session_name.contains('.') {
            return Err(PluginError::Config {
                message: format!(
                    "Invalid TMUX session name '{session_name}': cannot contain ':' or '.'"
                ),
            });
        }

        Ok(())
    }

    /// Validate agent name
    ///
    /// # Arguments
    ///
    /// * `agent_name` - Agent name to validate
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Config` if agent name is invalid
    pub fn validate_agent_name(agent_name: &str) -> Result<(), PluginError> {
        if agent_name.is_empty() {
            return Err(PluginError::Config {
                message: "Agent name cannot be empty".to_string(),
            });
        }

        // Agent names should follow the pattern: name@team or just name
        // We'll be lenient here and just check for basic sanity
        if agent_name.contains('\n') || agent_name.contains('\r') {
            return Err(PluginError::Config {
                message: format!("Invalid agent name '{agent_name}': cannot contain newlines"),
            });
        }

        Ok(())
    }

    /// Validate concurrency policy
    ///
    /// # Arguments
    ///
    /// * `policy` - Concurrency policy to validate
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Config` if policy is invalid
    pub fn validate_concurrency_policy(policy: &str) -> Result<(), PluginError> {
        match policy {
            "queue" | "reject" | "concurrent" => Ok(()),
            _ => Err(PluginError::Config {
                message: format!(
                    "Invalid concurrency policy '{policy}'. Must be 'queue', 'reject', or 'concurrent'"
                ),
            }),
        }
    }

    /// Resolve the startup command for an agent.
    /// Per-agent command takes priority over the default.
    pub fn resolve_command(&self, agent_name: &str) -> &str {
        self.agents
            .get(agent_name)
            .and_then(|a| a.command.as_deref())
            .unwrap_or(&self.command)
    }

    /// Validate the entire configuration
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Config` if any validation fails
    pub fn validate(&self) -> Result<(), PluginError> {
        // Validate backend
        Self::validate_backend(&self.backend)?;

        // Validate tmux session
        Self::validate_tmux_session(&self.tmux_session)?;

        // Validate each agent
        for (agent_name, agent_config) in &self.agents {
            Self::validate_agent_name(agent_name)?;
            Self::validate_concurrency_policy(&agent_config.concurrency_policy)?;
        }

        Ok(())
    }

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

        let command = table
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_COMMAND)
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

        let health_check_interval_secs = table
            .get("health_check_interval_secs")
            .and_then(|v| v.as_integer())
            .map(|i| i as u64)
            .unwrap_or(30); // 30 seconds default

        let max_restart_attempts = table
            .get("max_restart_attempts")
            .and_then(|v| v.as_integer())
            .map(|i| i as u32)
            .unwrap_or(3); // 3 attempts default

        let restart_backoff_secs = table
            .get("restart_backoff_secs")
            .and_then(|v| v.as_integer())
            .map(|i| i as u64)
            .unwrap_or(5); // 5 seconds default

        let shutdown_timeout_secs = table
            .get("shutdown_timeout_secs")
            .and_then(|v| v.as_integer())
            .map(|i| i as u64)
            .unwrap_or(10); // 10 seconds default

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
                        command: agent_table
                            .get("command")
                            .and_then(|v| v.as_str())
                            .map(String::from),
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

        let config = Self {
            enabled,
            backend,
            command,
            tmux_session,
            log_dir,
            inactivity_timeout_ms,
            health_check_interval_secs,
            max_restart_attempts,
            restart_backoff_secs,
            shutdown_timeout_secs,
            agents,
        };

        // Validate the configuration
        config.validate()?;

        Ok(config)
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
            command: DEFAULT_COMMAND.to_string(),
            tmux_session: "atm-workers".to_string(),
            log_dir: default_log_dir,
            inactivity_timeout_ms: 5 * 60 * 1000,
            health_check_interval_secs: 30,
            max_restart_attempts: 3,
            restart_backoff_secs: 5,
            shutdown_timeout_secs: 10,
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

    #[test]
    fn test_validate_backend_valid() {
        assert!(WorkersConfig::validate_backend("codex-tmux").is_ok());
    }

    #[test]
    fn test_validate_backend_invalid() {
        let result = WorkersConfig::validate_backend("invalid-backend");
        assert!(result.is_err());
        if let Err(PluginError::Config { message }) = result {
            assert!(message.contains("Unsupported worker backend"));
            assert!(message.contains("invalid-backend"));
        } else {
            panic!("Expected Config error");
        }
    }

    #[test]
    fn test_validate_tmux_session_valid() {
        assert!(WorkersConfig::validate_tmux_session("atm-workers").is_ok());
        assert!(WorkersConfig::validate_tmux_session("my_session").is_ok());
        assert!(WorkersConfig::validate_tmux_session("session123").is_ok());
    }

    #[test]
    fn test_validate_tmux_session_empty() {
        let result = WorkersConfig::validate_tmux_session("");
        assert!(result.is_err());
        if let Err(PluginError::Config { message }) = result {
            assert!(message.contains("cannot be empty"));
        } else {
            panic!("Expected Config error");
        }
    }

    #[test]
    fn test_validate_tmux_session_invalid_chars() {
        let result = WorkersConfig::validate_tmux_session("session:name");
        assert!(result.is_err());
        if let Err(PluginError::Config { message }) = result {
            assert!(message.contains("cannot contain ':' or '.'"));
        } else {
            panic!("Expected Config error");
        }

        let result = WorkersConfig::validate_tmux_session("session.name");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_agent_name_valid() {
        assert!(WorkersConfig::validate_agent_name("agent1").is_ok());
        assert!(WorkersConfig::validate_agent_name("arch-ctm@atm-planning").is_ok());
        assert!(WorkersConfig::validate_agent_name("my_agent").is_ok());
    }

    #[test]
    fn test_validate_agent_name_empty() {
        let result = WorkersConfig::validate_agent_name("");
        assert!(result.is_err());
        if let Err(PluginError::Config { message }) = result {
            assert!(message.contains("cannot be empty"));
        } else {
            panic!("Expected Config error");
        }
    }

    #[test]
    fn test_validate_agent_name_newlines() {
        let result = WorkersConfig::validate_agent_name("agent\nname");
        assert!(result.is_err());
        if let Err(PluginError::Config { message }) = result {
            assert!(message.contains("cannot contain newlines"));
        } else {
            panic!("Expected Config error");
        }
    }

    #[test]
    fn test_validate_concurrency_policy_valid() {
        assert!(WorkersConfig::validate_concurrency_policy("queue").is_ok());
        assert!(WorkersConfig::validate_concurrency_policy("reject").is_ok());
        assert!(WorkersConfig::validate_concurrency_policy("concurrent").is_ok());
    }

    #[test]
    fn test_validate_concurrency_policy_invalid() {
        let result = WorkersConfig::validate_concurrency_policy("invalid");
        assert!(result.is_err());
        if let Err(PluginError::Config { message }) = result {
            assert!(message.contains("Invalid concurrency policy"));
            assert!(message.contains("invalid"));
        } else {
            panic!("Expected Config error");
        }
    }

    #[test]
    fn test_validate_full_config() {
        let toml_str = r#"
enabled = true
backend = "codex-tmux"
tmux_session = "atm-workers"
[agents."test-agent"]
enabled = true
concurrency_policy = "queue"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = WorkersConfig::from_toml(&table).unwrap();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_config_invalid_tmux_session() {
        let toml_str = r#"
enabled = true
backend = "codex-tmux"
tmux_session = "invalid:session"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = WorkersConfig::from_toml(&table);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_config_invalid_policy() {
        let toml_str = r#"
enabled = true
backend = "codex-tmux"
[agents."test-agent"]
concurrency_policy = "invalid-policy"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = WorkersConfig::from_toml(&table);
        assert!(result.is_err());
    }
}
