//! Configuration for the Worker Adapter plugin

use crate::plugin::PluginError;
use agent_team_mail_core::toml;
use std::collections::HashMap;
use std::path::PathBuf;

/// Default startup command for worker agents
pub const DEFAULT_COMMAND: &str = "codex --yolo";

/// Default nudge message template.
///
/// `{count}` is replaced with the number of unread messages.
pub const DEFAULT_NUDGE_TEXT: &str =
    "You have {count} unread ATM messages. Run: atm read";

/// Default nudge cooldown in seconds (30 seconds between nudges per agent).
pub const DEFAULT_NUDGE_COOLDOWN_SECS: u64 = 30;

/// Configuration for the NudgeEngine.
///
/// Controls automatic nudging of idle agents that have unread inbox messages.
/// Nudging uses `tmux send-keys` (Unix only) and is gated on agent state.
#[derive(Debug, Clone)]
pub struct NudgeConfig {
    /// Whether the nudge engine is enabled (default: true).
    pub enabled: bool,
    /// Minimum seconds between nudges for the same agent (default: 30).
    pub cooldown_secs: u64,
    /// Message template sent to the agent pane via send-keys.
    ///
    /// `{count}` is replaced with the number of unread messages.
    pub text_template: String,
}

impl Default for NudgeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cooldown_secs: DEFAULT_NUDGE_COOLDOWN_SECS,
            text_template: DEFAULT_NUDGE_TEXT.to_string(),
        }
    }
}

impl NudgeConfig {
    /// Parse nudge configuration from an optional `[workers.nudge]` TOML subtable.
    ///
    /// Missing keys fall back to defaults.
    pub fn from_toml(table: Option<&toml::Value>) -> Self {
        let Some(t) = table.and_then(|v| v.as_table()) else {
            return Self::default();
        };

        let enabled = t
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let cooldown_secs = t
            .get("cooldown_secs")
            .and_then(|v| v.as_integer())
            .map(|i| i as u64)
            .unwrap_or(DEFAULT_NUDGE_COOLDOWN_SECS);

        let text_template = t
            .get("text_template")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_NUDGE_TEXT)
            .to_string();

        Self {
            enabled,
            cooldown_secs,
            text_template,
        }
    }
}

/// Per-agent configuration
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Whether this agent is enabled for worker adapter
    pub enabled: bool,
    /// Team member name for this agent (required, used as runtime identity)
    pub member_name: String,
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
            member_name: String::new(),
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
    /// Claude team name (required when enabled = true)
    pub team_name: String,
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
    /// Nudge engine configuration
    pub nudge: NudgeConfig,
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

    /// Validate team name
    ///
    /// # Arguments
    ///
    /// * `team_name` - Team name to validate
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Config` if team name is invalid
    pub fn validate_team_name(team_name: &str) -> Result<(), PluginError> {
        if team_name.is_empty() {
            return Err(PluginError::Config {
                message: "Team name cannot be empty when workers are enabled".to_string(),
            });
        }

        if team_name.contains('\n') || team_name.contains('\r') {
            return Err(PluginError::Config {
                message: format!("Invalid team name '{team_name}': cannot contain newlines"),
            });
        }

        Ok(())
    }

    /// Validate member name
    ///
    /// # Arguments
    ///
    /// * `member_name` - Member name to validate
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Config` if member name is invalid
    pub fn validate_member_name(member_name: &str) -> Result<(), PluginError> {
        if member_name.is_empty() {
            return Err(PluginError::Config {
                message: "Member name cannot be empty".to_string(),
            });
        }

        if member_name.contains('\n') || member_name.contains('\r') {
            return Err(PluginError::Config {
                message: format!("Invalid member name '{member_name}': cannot contain newlines"),
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

    /// Validate startup command
    ///
    /// Ensures the command is non-empty when workers are enabled.
    /// Logs a warning for commands containing shell-chaining operators.
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Config` if command is empty
    pub fn validate_command(command: &str) -> Result<(), PluginError> {
        if command.trim().is_empty() {
            return Err(PluginError::Config {
                message: "Worker startup command cannot be empty".to_string(),
            });
        }

        // Warn about shell-chaining operators (informational, not a hard error)
        for pattern in &["&&", "||", ";", "$("] {
            if command.contains(pattern) {
                tracing::warn!(
                    "Worker command contains shell operator '{pattern}': {command}. \
                     Ensure this is intentional."
                );
                break;
            }
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

    /// Resolve the startup command for an agent by config key.
    /// Per-agent command takes priority over the default.
    pub fn resolve_command(&self, config_key: &str) -> &str {
        self.agents
            .get(config_key)
            .and_then(|a| a.command.as_deref())
            .unwrap_or(&self.command)
    }

    /// Get member_name for a config key
    pub fn get_member_name(&self, config_key: &str) -> Option<&str> {
        self.agents.get(config_key).map(|a| a.member_name.as_str())
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

        // If enabled, team_name and command are required
        if self.enabled {
            Self::validate_team_name(&self.team_name)?;
            Self::validate_command(&self.command)?;
        }

        // Track member_names to check for duplicates
        let mut member_names = std::collections::HashSet::new();

        // Validate each agent
        for (agent_name, agent_config) in &self.agents {
            Self::validate_agent_name(agent_name)?;
            Self::validate_member_name(&agent_config.member_name)?;
            Self::validate_concurrency_policy(&agent_config.concurrency_policy)?;

            // Validate per-agent command override if present
            if let Some(cmd) = &agent_config.command {
                Self::validate_command(cmd)?;
            }

            // Check for duplicate member_names
            if !member_names.insert(&agent_config.member_name) {
                return Err(PluginError::Config {
                    message: format!(
                        "Duplicate member_name '{}' found. Each agent must have a unique member_name.",
                        agent_config.member_name
                    ),
                });
            }
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

        let team_name = table
            .get("team_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
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

        // Log directory: default to ~/.config/atm/worker-logs
        // When ATM_HOME is set, use it directly (test-friendly)
        let default_log_dir = if let Ok(atm_home) = std::env::var("ATM_HOME") {
            PathBuf::from(atm_home).join("worker-logs")
        } else {
            agent_team_mail_core::home::get_home_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".config/atm/worker-logs")
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

        // Parse nudge configuration from [workers.nudge]
        let nudge = NudgeConfig::from_toml(table.get("nudge"));

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
                        member_name: agent_table
                            .get("member_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
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
            team_name,
            command,
            tmux_session,
            log_dir,
            inactivity_timeout_ms,
            health_check_interval_secs,
            max_restart_attempts,
            restart_backoff_secs,
            shutdown_timeout_secs,
            nudge,
            agents,
        };

        // Validate the configuration
        config.validate()?;

        Ok(config)
    }
}

impl Default for WorkersConfig {
    fn default() -> Self {
        // When ATM_HOME is set, use it directly (test-friendly)
        let default_log_dir = if let Ok(atm_home) = std::env::var("ATM_HOME") {
            PathBuf::from(atm_home).join("worker-logs")
        } else {
            agent_team_mail_core::home::get_home_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".config/atm/worker-logs")
        };

        Self {
            enabled: false,
            backend: "codex-tmux".to_string(),
            team_name: String::new(),
            command: DEFAULT_COMMAND.to_string(),
            tmux_session: "atm-workers".to_string(),
            log_dir: default_log_dir,
            inactivity_timeout_ms: 5 * 60 * 1000,
            health_check_interval_secs: 30,
            max_restart_attempts: 3,
            restart_backoff_secs: 5,
            shutdown_timeout_secs: 10,
            nudge: NudgeConfig::default(),
            agents: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nudge_config_default() {
        let nudge = NudgeConfig::default();
        assert!(nudge.enabled);
        assert_eq!(nudge.cooldown_secs, DEFAULT_NUDGE_COOLDOWN_SECS);
        assert!(nudge.text_template.contains("{count}"));
    }

    #[test]
    fn test_nudge_config_from_toml_none() {
        let nudge = NudgeConfig::from_toml(None);
        assert!(nudge.enabled);
        assert_eq!(nudge.cooldown_secs, 30);
    }

    #[test]
    fn test_nudge_config_from_toml_custom() {
        let toml_str = r#"
enabled = false
cooldown_secs = 60
text_template = "You have {count} messages waiting."
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let value = toml::Value::Table(table);
        let nudge = NudgeConfig::from_toml(Some(&value));
        assert!(!nudge.enabled);
        assert_eq!(nudge.cooldown_secs, 60);
        assert_eq!(nudge.text_template, "You have {count} messages waiting.");
    }

    #[test]
    fn test_nudge_config_parsed_from_workers_table() {
        let toml_str = r#"
enabled = true
team_name = "test-team"
[nudge]
cooldown_secs = 45
enabled = true
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = WorkersConfig::from_toml(&table).unwrap();
        assert_eq!(config.nudge.cooldown_secs, 45);
        assert!(config.nudge.enabled);
    }

    #[test]
    fn test_config_default() {
        let config = WorkersConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.backend, "codex-tmux");
        assert_eq!(config.tmux_session, "atm-workers");
        // Nudge config should have defaults
        assert!(config.nudge.enabled);
        assert_eq!(config.nudge.cooldown_secs, 30);
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
team_name = "test-team"
tmux_session = "my-workers"
log_dir = "/var/log/atm-workers"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = WorkersConfig::from_toml(&table).unwrap();

        assert!(config.enabled);
        assert_eq!(config.backend, "codex-tmux");
        assert_eq!(config.team_name, "test-team");
        assert_eq!(config.tmux_session, "my-workers");
        assert_eq!(config.log_dir, PathBuf::from("/var/log/atm-workers"));
    }

    #[test]
    fn test_config_from_toml_partial() {
        let toml_str = r#"
enabled = true
team_name = "test-team"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = WorkersConfig::from_toml(&table).unwrap();

        assert!(config.enabled);
        assert_eq!(config.backend, "codex-tmux"); // default
        assert_eq!(config.team_name, "test-team");
        assert_eq!(config.tmux_session, "atm-workers"); // default
    }

    #[test]
    fn test_config_invalid_backend() {
        let toml_str = r#"
enabled = true
backend = "unsupported-backend"
team_name = "test-team"
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
team_name = "test-team"
tmux_session = "atm-workers"
[agents."test-agent"]
member_name = "test-member"
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
team_name = "test-team"
[agents."test-agent"]
member_name = "test-member"
concurrency_policy = "invalid-policy"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = WorkersConfig::from_toml(&table);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_config_missing_team_name_when_enabled() {
        let toml_str = r#"
enabled = true
backend = "codex-tmux"
[agents."test-agent"]
member_name = "test-member"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = WorkersConfig::from_toml(&table);
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("Team name cannot be empty"));
        }
    }

    #[test]
    fn test_validate_config_missing_member_name() {
        let toml_str = r#"
enabled = true
backend = "codex-tmux"
team_name = "test-team"
[agents."test-agent"]
enabled = true
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = WorkersConfig::from_toml(&table);
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("Member name cannot be empty"));
        }
    }

    #[test]
    fn test_validate_config_duplicate_member_names() {
        let toml_str = r#"
enabled = true
backend = "codex-tmux"
team_name = "test-team"
[agents."agent1"]
member_name = "duplicate"
[agents."agent2"]
member_name = "duplicate"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = WorkersConfig::from_toml(&table);
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("Duplicate member_name"));
        }
    }

    #[test]
    fn test_validate_team_name() {
        assert!(WorkersConfig::validate_team_name("valid-team").is_ok());
        assert!(WorkersConfig::validate_team_name("").is_err());
        assert!(WorkersConfig::validate_team_name("team\nname").is_err());
    }

    #[test]
    fn test_validate_member_name() {
        assert!(WorkersConfig::validate_member_name("arch-ctm").is_ok());
        assert!(WorkersConfig::validate_member_name("dev-1").is_ok());
        assert!(WorkersConfig::validate_member_name("").is_err());
        assert!(WorkersConfig::validate_member_name("member\nname").is_err());
    }

    #[test]
    fn test_get_member_name() {
        let toml_str = r#"
enabled = true
backend = "codex-tmux"
team_name = "test-team"
[agents."architect"]
member_name = "arch-ctm"
[agents."developer"]
member_name = "dev-1"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = WorkersConfig::from_toml(&table).unwrap();

        assert_eq!(config.get_member_name("architect"), Some("arch-ctm"));
        assert_eq!(config.get_member_name("developer"), Some("dev-1"));
        assert_eq!(config.get_member_name("nonexistent"), None);
    }
}
