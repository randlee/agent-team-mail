//! Bridge plugin configuration parsing and validation

use atm_core::config::{BridgeConfig, HostnameRegistry};
use crate::plugin::PluginError;

/// Bridge plugin configuration (daemon-specific)
#[derive(Debug, Clone)]
pub struct BridgePluginConfig {
    /// Core bridge configuration from atm-core
    pub core: BridgeConfig,

    /// Hostname registry built from remotes
    pub registry: HostnameRegistry,

    /// Resolved local hostname
    pub local_hostname: String,
}

impl BridgePluginConfig {
    /// Parse bridge plugin config from TOML table
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Config parsing fails
    /// - Hostname collisions detected
    /// - Bridge is enabled but has no remotes configured
    pub fn from_toml(table: &toml::Table, system_hostname: &str) -> Result<Self, PluginError> {
        // Parse core bridge config
        let core: BridgeConfig = table
            .clone()
            .try_into()
            .map_err(|e| PluginError::Config {
                message: format!("Failed to parse bridge config: {e}"),
            })?;

        // Resolve local hostname (use config value or system hostname)
        let local_hostname = core
            .local_hostname
            .clone()
            .unwrap_or_else(|| system_hostname.to_string());

        // Build hostname registry from remotes
        let mut registry = HostnameRegistry::new();

        for remote in &core.remotes {
            registry.register(remote.clone()).map_err(|e| {
                PluginError::Config {
                    message: format!("Hostname registry error: {e}"),
                }
            })?;
        }

        // Validate: if bridge is enabled, must have at least one remote
        if core.enabled && registry.is_empty() {
            return Err(PluginError::Config {
                message: "Bridge is enabled but no remotes configured".to_string(),
            });
        }

        Ok(Self {
            core,
            registry,
            local_hostname,
        })
    }

    /// Check if bridge is enabled
    pub fn is_enabled(&self) -> bool {
        self.core.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_from_toml_minimal() {
        let toml_str = r#"
enabled = true

[[remotes]]
hostname = "desktop"
address = "user@desktop.local"
"#;

        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = BridgePluginConfig::from_toml(&table, "laptop").unwrap();

        assert!(config.is_enabled());
        assert_eq!(config.local_hostname, "laptop"); // Uses system hostname
        assert_eq!(config.registry.len(), 1);
        assert!(config.registry.is_known_hostname("desktop"));
    }

    #[test]
    fn test_config_from_toml_with_local_hostname() {
        let toml_str = r#"
enabled = true
local_hostname = "custom-name"

[[remotes]]
hostname = "server"
address = "user@server.local"
"#;

        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = BridgePluginConfig::from_toml(&table, "system-hostname").unwrap();

        assert_eq!(config.local_hostname, "custom-name"); // Uses config value
    }

    #[test]
    fn test_config_from_toml_with_aliases() {
        let toml_str = r#"
enabled = true

[[remotes]]
hostname = "server-1"
address = "user@server1.local"
aliases = ["s1", "primary"]

[[remotes]]
hostname = "server-2"
address = "user@server2.local"
aliases = ["s2", "backup"]
"#;

        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = BridgePluginConfig::from_toml(&table, "laptop").unwrap();

        assert_eq!(config.registry.len(), 2);
        assert!(config.registry.is_known_hostname("server-1"));
        assert!(config.registry.is_known_hostname("s1"));
        assert!(config.registry.is_known_hostname("primary"));
        assert!(config.registry.is_known_hostname("server-2"));
        assert!(config.registry.is_known_hostname("s2"));
        assert!(config.registry.is_known_hostname("backup"));
    }

    #[test]
    fn test_config_hostname_collision_error() {
        let toml_str = r#"
enabled = true

[[remotes]]
hostname = "duplicate"
address = "user@host1.local"

[[remotes]]
hostname = "duplicate"
address = "user@host2.local"
"#;

        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = BridgePluginConfig::from_toml(&table, "laptop");

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("already registered"));
    }

    #[test]
    fn test_config_alias_collision_error() {
        let toml_str = r#"
enabled = true

[[remotes]]
hostname = "server-1"
address = "user@host1.local"
aliases = ["common"]

[[remotes]]
hostname = "server-2"
address = "user@host2.local"
aliases = ["common"]
"#;

        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = BridgePluginConfig::from_toml(&table, "laptop");

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Alias already registered"));
    }

    #[test]
    fn test_config_enabled_without_remotes_error() {
        let toml_str = r#"
enabled = true
"#;

        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = BridgePluginConfig::from_toml(&table, "laptop");

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no remotes configured"));
    }

    #[test]
    fn test_config_disabled_without_remotes_ok() {
        let toml_str = r#"
enabled = false
"#;

        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let result = BridgePluginConfig::from_toml(&table, "laptop");

        // Disabled bridge doesn't need remotes
        assert!(result.is_ok());
        let config = result.unwrap();
        assert!(!config.is_enabled());
    }

    #[test]
    fn test_config_role_parsing() {
        let hub_toml = r#"
enabled = true
role = "hub"

[[remotes]]
hostname = "spoke1"
address = "user@spoke1.local"
"#;

        let table: toml::Table = toml::from_str(hub_toml).unwrap();
        let config = BridgePluginConfig::from_toml(&table, "hub-host").unwrap();

        assert_eq!(config.core.role, atm_core::config::BridgeRole::Hub);

        let spoke_toml = r#"
enabled = true
role = "spoke"

[[remotes]]
hostname = "hub"
address = "user@hub.local"
"#;

        let table: toml::Table = toml::from_str(spoke_toml).unwrap();
        let config = BridgePluginConfig::from_toml(&table, "spoke-host").unwrap();

        assert_eq!(config.core.role, atm_core::config::BridgeRole::Spoke);
    }

    #[test]
    fn test_config_sync_interval() {
        let toml_str = r#"
enabled = true
sync_interval_secs = 300

[[remotes]]
hostname = "remote"
address = "user@remote.local"
"#;

        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = BridgePluginConfig::from_toml(&table, "laptop").unwrap();

        assert_eq!(config.core.sync_interval_secs, 300);
    }
}
