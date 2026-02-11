//! Configuration discovery and resolution

use super::types::{Config, OutputFormat};
use crate::schema::SettingsJson;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Configuration error
#[derive(Debug, Error)]
pub enum ConfigError {
    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// TOML parsing error
    #[error("TOML parsing error: {0}")]
    TomlParse(#[from] toml::de::Error),

    /// JSON parsing error
    #[error("JSON parsing error: {0}")]
    JsonParse(#[from] serde_json::Error),

    /// Configuration not found
    #[error("Configuration not found")]
    NotFound,
}

/// Command-line overrides for configuration
#[derive(Debug, Default, Clone)]
pub struct ConfigOverrides {
    /// Override default team
    pub team: Option<String>,
    /// Override identity
    pub identity: Option<String>,
    /// Override output format
    pub format: Option<OutputFormat>,
    /// Override color setting
    pub color: Option<bool>,
    /// Path to config file override
    pub config_path: Option<PathBuf>,
}

/// Resolve configuration from all sources
///
/// Priority (highest to lowest):
/// 1. Command-line overrides
/// 2. Environment variables
/// 3. Repo-local config (.atm.toml in current dir or git root)
/// 4. Global config (~/.config/atm/config.toml)
/// 5. Defaults
pub fn resolve_config(
    overrides: &ConfigOverrides,
    current_dir: &Path,
    home_dir: &Path,
) -> Result<Config, ConfigError> {
    let mut config = Config::default();

    // 4. Try global config
    let global_config_path = home_dir.join(".config/atm/config.toml");
    if global_config_path.exists() {
        if let Ok(file_config) = load_config_file(&global_config_path) {
            merge_config(&mut config, file_config);
        } else {
            eprintln!("Warning: Failed to parse global config at {global_config_path:?}");
        }
    }

    // 3. Try repo-local config (current dir or git root)
    if let Some(repo_config) = find_repo_local_config(current_dir) {
        if let Ok(file_config) = load_config_file(&repo_config) {
            merge_config(&mut config, file_config);
        } else {
            eprintln!("Warning: Failed to parse repo config at {repo_config:?}");
        }
    }

    // 2. Apply environment variables
    apply_env_overrides(&mut config);

    // 1. Apply command-line overrides
    apply_cli_overrides(&mut config, overrides);

    Ok(config)
}

/// Find repo-local config file
///
/// Searches current directory and parent directories up to git root
fn find_repo_local_config(current_dir: &Path) -> Option<PathBuf> {
    let mut dir = current_dir;

    loop {
        let config_path = dir.join(".atm.toml");
        if config_path.exists() {
            return Some(config_path);
        }

        // Stop at git root
        if dir.join(".git").exists() {
            break;
        }

        // Move to parent
        dir = dir.parent()?;
    }

    None
}

/// Load config from a TOML file
fn load_config_file(path: &Path) -> Result<Config, ConfigError> {
    let contents = std::fs::read_to_string(path)?;
    let config: Config = toml::from_str(&contents)?;
    Ok(config)
}

/// Merge file config into base config
fn merge_config(base: &mut Config, file: Config) {
    // Merge core config
    base.core.default_team = file.core.default_team;
    base.core.identity = file.core.identity;

    // Merge display config
    base.display.format = file.display.format;
    base.display.color = file.display.color;
    base.display.timestamps = file.display.timestamps;

    // Merge messaging config
    if file.messaging.offline_action.is_some() {
        base.messaging.offline_action = file.messaging.offline_action;
    }
}

/// Apply environment variable overrides
fn apply_env_overrides(config: &mut Config) {
    if let Ok(team) = std::env::var("ATM_TEAM") {
        config.core.default_team = team;
    }

    if let Ok(identity) = std::env::var("ATM_IDENTITY") {
        config.core.identity = identity;
    }

    if std::env::var("ATM_NO_COLOR").is_ok() {
        config.display.color = false;
    }
}

/// Apply command-line overrides
fn apply_cli_overrides(config: &mut Config, overrides: &ConfigOverrides) {
    if let Some(ref team) = overrides.team {
        config.core.default_team = team.clone();
    }

    if let Some(ref identity) = overrides.identity {
        config.core.identity = identity.clone();
    }

    if let Some(format) = overrides.format {
        config.display.format = format;
    }

    if let Some(color) = overrides.color {
        config.display.color = color;
    }
}

/// Resolve Claude Code settings.json
///
/// Precedence (highest to lowest):
/// 1. Managed policy (if exists)
/// 2. CLI args (if provided) - path override
/// 3. .claude/settings.local.json (repo-local)
/// 4. .claude/settings.json (repo-local)
/// 5. ~/.claude/settings.json (global)
///
/// Returns None if no settings file found or if parsing fails.
/// Logs warnings for parse errors but continues.
pub fn resolve_settings(
    settings_path_override: Option<&Path>,
    current_dir: &Path,
    home_dir: &Path,
) -> Option<SettingsJson> {
    // 2. CLI args override
    if let Some(path) = settings_path_override
        && let Some(settings) = try_load_settings(path)
    {
        return Some(settings);
    }

    // 3. Repo-local settings.local.json
    let local_path = current_dir.join(".claude/settings.local.json");
    if let Some(settings) = try_load_settings(&local_path) {
        return Some(settings);
    }

    // 4. Repo-local settings.json
    let repo_path = current_dir.join(".claude/settings.json");
    if let Some(settings) = try_load_settings(&repo_path) {
        return Some(settings);
    }

    // 5. Global settings.json
    let global_path = home_dir.join(".claude/settings.json");
    if let Some(settings) = try_load_settings(&global_path) {
        return Some(settings);
    }

    // 1. Managed policy (TODO: implement when policy paths are known)
    // For now, we skip this as we don't know the platform-specific paths

    None
}

/// Try to load and parse a settings file
fn try_load_settings(path: &Path) -> Option<SettingsJson> {
    if !path.exists() {
        return None;
    }

    match std::fs::read_to_string(path) {
        Ok(contents) => match serde_json::from_str(&contents) {
            Ok(settings) => Some(settings),
            Err(e) => {
                eprintln!("Warning: Failed to parse settings at {path:?}: {e}");
                None
            }
        },
        Err(e) => {
            eprintln!("Warning: Failed to read settings at {path:?}: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::TimestampFormat;
    use serial_test::serial;
    use std::env;

    #[test]
    #[serial]
    fn test_config_defaults() {
        // Clean up environment variables from other tests
        unsafe {
            env::remove_var("ATM_TEAM");
            env::remove_var("ATM_IDENTITY");
            env::remove_var("ATM_NO_COLOR");
        }

        let temp_dir = std::env::temp_dir();
        let overrides = ConfigOverrides::default();

        let config = resolve_config(&overrides, &temp_dir, &temp_dir).unwrap();

        assert_eq!(config.core.default_team, "default");
        assert_eq!(config.core.identity, "human");
        assert_eq!(config.display.format, OutputFormat::Text);
        assert!(config.display.color);
    }

    #[test]
    #[serial]
    fn test_env_overrides() {
        let temp_dir = std::env::temp_dir();
        let overrides = ConfigOverrides::default();

        unsafe {
            env::set_var("ATM_TEAM", "test-team");
            env::set_var("ATM_IDENTITY", "test-user");
        }

        let config = resolve_config(&overrides, &temp_dir, &temp_dir).unwrap();

        assert_eq!(config.core.default_team, "test-team");
        assert_eq!(config.core.identity, "test-user");

        unsafe {
            env::remove_var("ATM_TEAM");
            env::remove_var("ATM_IDENTITY");
        }
    }

    #[test]
    fn test_cli_overrides() {
        let temp_dir = std::env::temp_dir();
        let overrides = ConfigOverrides {
            team: Some("cli-team".to_string()),
            identity: Some("cli-user".to_string()),
            format: Some(OutputFormat::Json),
            color: Some(false),
            config_path: None,
        };

        let config = resolve_config(&overrides, &temp_dir, &temp_dir).unwrap();

        assert_eq!(config.core.default_team, "cli-team");
        assert_eq!(config.core.identity, "cli-user");
        assert_eq!(config.display.format, OutputFormat::Json);
        assert!(!config.display.color);
    }

    #[test]
    #[serial]
    fn test_no_color_env() {
        let temp_dir = std::env::temp_dir();
        let overrides = ConfigOverrides::default();

        unsafe {
            env::set_var("ATM_NO_COLOR", "1");
        }

        let config = resolve_config(&overrides, &temp_dir, &temp_dir).unwrap();
        assert!(!config.display.color);

        unsafe {
            env::remove_var("ATM_NO_COLOR");
        }
    }

    #[test]
    fn test_settings_resolution_none() {
        let temp_dir = std::env::temp_dir();
        let nonexistent = temp_dir.join("nonexistent");

        let settings = resolve_settings(None, &nonexistent, &nonexistent);
        assert!(settings.is_none());
    }

    #[test]
    fn test_config_file_parse() {
        let temp_dir = std::env::temp_dir();
        let config_path = temp_dir.join("test-config.toml");

        let toml_content = r#"
[core]
default_team = "file-team"
identity = "file-user"

[display]
format = "json"
color = false
timestamps = "iso8601"
        "#;

        std::fs::write(&config_path, toml_content).unwrap();

        let config = load_config_file(&config_path).unwrap();
        assert_eq!(config.core.default_team, "file-team");
        assert_eq!(config.core.identity, "file-user");
        assert_eq!(config.display.format, OutputFormat::Json);
        assert!(!config.display.color);
        assert_eq!(config.display.timestamps, TimestampFormat::Iso8601);

        std::fs::remove_file(&config_path).ok();
    }

    #[test]
    fn test_malformed_config_handled_gracefully() {
        let temp_dir = std::env::temp_dir();
        let config_path = temp_dir.join("malformed-config.toml");

        // Invalid TOML
        std::fs::write(&config_path, "invalid toml [[[").unwrap();

        let result = load_config_file(&config_path);
        assert!(result.is_err());

        std::fs::remove_file(&config_path).ok();
    }
}
