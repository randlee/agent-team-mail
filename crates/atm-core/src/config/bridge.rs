//! Bridge plugin configuration types

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Bridge plugin configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    /// Enable bridge plugin
    #[serde(default)]
    pub enabled: bool,

    /// Local hostname (defaults to system hostname if not specified)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_hostname: Option<String>,

    /// Bridge role (hub or spoke)
    #[serde(default)]
    pub role: BridgeRole,

    /// Sync interval in seconds
    #[serde(default = "default_sync_interval")]
    pub sync_interval_secs: u64,

    /// Remote hosts configuration
    #[serde(default)]
    pub remotes: Vec<RemoteConfig>,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            local_hostname: None,
            role: BridgeRole::Spoke,
            sync_interval_secs: default_sync_interval(),
            remotes: Vec::new(),
        }
    }
}

fn default_sync_interval() -> u64 {
    60 // Default: sync every 60 seconds
}

/// Bridge role
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BridgeRole {
    /// Hub role - synchronization point for all spokes
    Hub,
    /// Spoke role - connects to hub for synchronization
    #[default]
    Spoke,
}

/// Remote host configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteConfig {
    /// Remote hostname (canonical name)
    pub hostname: String,

    /// SSH connection string (e.g., "user@host:port" or "user@host")
    pub address: String,

    /// Path to SSH private key (optional, uses default SSH key if not specified)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_key_path: Option<String>,

    /// Aliases for this remote (optional)
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// Hostname registry - manages hostname to remote mapping with alias resolution
#[derive(Debug, Clone)]
pub struct HostnameRegistry {
    /// Map of canonical hostname to RemoteConfig
    remotes: HashMap<String, RemoteConfig>,

    /// Map of alias to canonical hostname
    aliases: HashMap<String, String>,
}

impl HostnameRegistry {
    /// Create a new empty hostname registry
    pub fn new() -> Self {
        Self {
            remotes: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    /// Register a remote host
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Hostname already registered
    /// - Any alias already registered
    pub fn register(&mut self, remote: RemoteConfig) -> Result<(), String> {
        let canonical = remote.hostname.to_lowercase();

        // Check for hostname collision
        if self.remotes.contains_key(&canonical) {
            return Err(format!("Hostname already registered: {}", remote.hostname));
        }

        // Check for alias collisions
        for alias in &remote.aliases {
            let alias_lower = alias.to_lowercase();
            if self.aliases.contains_key(&alias_lower) {
                return Err(format!("Alias already registered: {alias}"));
            }
            // Also check if alias collides with an existing hostname
            if self.remotes.contains_key(&alias_lower) {
                return Err(format!("Alias '{alias}' collides with existing hostname"));
            }
        }

        // Register all aliases
        for alias in &remote.aliases {
            let alias_lower = alias.to_lowercase();
            self.aliases.insert(alias_lower, canonical.clone());
        }

        // Register the remote
        self.remotes.insert(canonical, remote);

        Ok(())
    }

    /// Resolve an alias to its canonical hostname
    ///
    /// Returns the canonical hostname if the input is an alias,
    /// or the input itself if it's already a canonical hostname.
    /// Returns None if the name is not known.
    pub fn resolve_alias(&self, name: &str) -> Option<&str> {
        let name_lower = name.to_lowercase();

        // Check if it's an alias first
        if let Some(canonical_lower) = self.aliases.get(&name_lower) {
            // Return the original hostname from the RemoteConfig (preserves case)
            return self.remotes.get(canonical_lower).map(|r| r.hostname.as_str());
        }

        // Check if it's a canonical hostname
        if let Some(remote) = self.remotes.get(&name_lower) {
            return Some(remote.hostname.as_str());
        }

        None
    }

    /// Check if a hostname (canonical or alias) is known
    pub fn is_known_hostname(&self, name: &str) -> bool {
        self.resolve_alias(name).is_some()
    }

    /// Get remote configuration by canonical hostname
    pub fn get_remote(&self, hostname: &str) -> Option<&RemoteConfig> {
        let hostname_lower = hostname.to_lowercase();
        self.remotes.get(&hostname_lower)
    }

    /// Get all registered remotes
    pub fn remotes(&self) -> impl Iterator<Item = &RemoteConfig> {
        self.remotes.values()
    }

    /// Check if registry is empty
    pub fn is_empty(&self) -> bool {
        self.remotes.is_empty()
    }

    /// Count of registered remotes
    pub fn len(&self) -> usize {
        self.remotes.len()
    }
}

impl Default for HostnameRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bridge_config_defaults() {
        let config = BridgeConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.role, BridgeRole::Spoke);
        assert_eq!(config.sync_interval_secs, 60);
        assert!(config.remotes.is_empty());
    }

    #[test]
    fn test_bridge_config_serialization() {
        let config = BridgeConfig {
            enabled: true,
            local_hostname: Some("test-host".to_string()),
            role: BridgeRole::Hub,
            sync_interval_secs: 120,
            remotes: vec![RemoteConfig {
                hostname: "remote1".to_string(),
                address: "user@remote1.local".to_string(),
                ssh_key_path: Some("/path/to/key".to_string()),
                aliases: vec!["r1".to_string()],
            }],
        };

        let toml_str = toml::to_string(&config).unwrap();
        let deserialized: BridgeConfig = toml::from_str(&toml_str).unwrap();

        assert_eq!(config.enabled, deserialized.enabled);
        assert_eq!(config.local_hostname, deserialized.local_hostname);
        assert_eq!(config.role, deserialized.role);
        assert_eq!(config.sync_interval_secs, deserialized.sync_interval_secs);
        assert_eq!(config.remotes.len(), deserialized.remotes.len());
    }

    #[test]
    fn test_bridge_config_from_toml_minimal() {
        let toml_str = r#"
enabled = true
"#;

        let config: BridgeConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.role, BridgeRole::Spoke);
        assert_eq!(config.sync_interval_secs, 60);
    }

    #[test]
    fn test_bridge_config_from_toml_full() {
        let toml_str = r#"
enabled = true
local_hostname = "my-laptop"
role = "hub"
sync_interval_secs = 300

[[remotes]]
hostname = "desktop"
address = "user@desktop.local"
ssh_key_path = "/home/user/.ssh/id_rsa"
aliases = ["desk", "main-desktop"]

[[remotes]]
hostname = "server"
address = "user@server.example.com:2222"
"#;

        let config: BridgeConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.local_hostname, Some("my-laptop".to_string()));
        assert_eq!(config.role, BridgeRole::Hub);
        assert_eq!(config.sync_interval_secs, 300);
        assert_eq!(config.remotes.len(), 2);

        let remote1 = &config.remotes[0];
        assert_eq!(remote1.hostname, "desktop");
        assert_eq!(remote1.address, "user@desktop.local");
        assert_eq!(remote1.ssh_key_path, Some("/home/user/.ssh/id_rsa".to_string()));
        assert_eq!(remote1.aliases, vec!["desk", "main-desktop"]);

        let remote2 = &config.remotes[1];
        assert_eq!(remote2.hostname, "server");
        assert_eq!(remote2.address, "user@server.example.com:2222");
        assert!(remote2.ssh_key_path.is_none());
        assert!(remote2.aliases.is_empty());
    }

    #[test]
    fn test_hostname_registry_register() {
        let mut registry = HostnameRegistry::new();

        let remote = RemoteConfig {
            hostname: "test-host".to_string(),
            address: "user@test-host".to_string(),
            ssh_key_path: None,
            aliases: vec!["alias1".to_string(), "alias2".to_string()],
        };

        assert!(registry.register(remote).is_ok());
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_hostname_registry_collision() {
        let mut registry = HostnameRegistry::new();

        let remote1 = RemoteConfig {
            hostname: "test-host".to_string(),
            address: "user@test-host".to_string(),
            ssh_key_path: None,
            aliases: Vec::new(),
        };

        let remote2 = RemoteConfig {
            hostname: "test-host".to_string(),
            address: "user@another-host".to_string(),
            ssh_key_path: None,
            aliases: Vec::new(),
        };

        assert!(registry.register(remote1).is_ok());
        let result = registry.register(remote2);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already registered"));
    }

    #[test]
    fn test_hostname_registry_alias_collision() {
        let mut registry = HostnameRegistry::new();

        let remote1 = RemoteConfig {
            hostname: "host1".to_string(),
            address: "user@host1".to_string(),
            ssh_key_path: None,
            aliases: vec!["common-alias".to_string()],
        };

        let remote2 = RemoteConfig {
            hostname: "host2".to_string(),
            address: "user@host2".to_string(),
            ssh_key_path: None,
            aliases: vec!["common-alias".to_string()],
        };

        assert!(registry.register(remote1).is_ok());
        let result = registry.register(remote2);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Alias already registered"));
    }

    #[test]
    fn test_hostname_registry_alias_hostname_collision() {
        let mut registry = HostnameRegistry::new();

        let remote1 = RemoteConfig {
            hostname: "existing-host".to_string(),
            address: "user@host1".to_string(),
            ssh_key_path: None,
            aliases: Vec::new(),
        };

        let remote2 = RemoteConfig {
            hostname: "host2".to_string(),
            address: "user@host2".to_string(),
            ssh_key_path: None,
            aliases: vec!["existing-host".to_string()],
        };

        assert!(registry.register(remote1).is_ok());
        let result = registry.register(remote2);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("collides with existing hostname"));
    }

    #[test]
    fn test_hostname_registry_resolve_alias() {
        let mut registry = HostnameRegistry::new();

        let remote = RemoteConfig {
            hostname: "canonical-name".to_string(),
            address: "user@host".to_string(),
            ssh_key_path: None,
            aliases: vec!["alias1".to_string(), "alias2".to_string()],
        };

        registry.register(remote).unwrap();

        // Resolve aliases
        assert_eq!(registry.resolve_alias("alias1"), Some("canonical-name"));
        assert_eq!(registry.resolve_alias("alias2"), Some("canonical-name"));

        // Canonical name resolves to itself
        assert_eq!(registry.resolve_alias("canonical-name"), Some("canonical-name"));

        // Unknown name returns None
        assert_eq!(registry.resolve_alias("unknown"), None);
    }

    #[test]
    fn test_hostname_registry_case_insensitive() {
        let mut registry = HostnameRegistry::new();

        let remote = RemoteConfig {
            hostname: "Test-Host".to_string(),
            address: "user@host".to_string(),
            ssh_key_path: None,
            aliases: vec!["MyAlias".to_string()],
        };

        registry.register(remote).unwrap();

        // Case-insensitive lookups
        assert_eq!(registry.resolve_alias("test-host"), Some("Test-Host"));
        assert_eq!(registry.resolve_alias("TEST-HOST"), Some("Test-Host"));
        assert_eq!(registry.resolve_alias("myalias"), Some("Test-Host"));
        assert_eq!(registry.resolve_alias("MYALIAS"), Some("Test-Host"));
    }

    #[test]
    fn test_hostname_registry_is_known() {
        let mut registry = HostnameRegistry::new();

        let remote = RemoteConfig {
            hostname: "known-host".to_string(),
            address: "user@host".to_string(),
            ssh_key_path: None,
            aliases: vec!["alias".to_string()],
        };

        registry.register(remote).unwrap();

        assert!(registry.is_known_hostname("known-host"));
        assert!(registry.is_known_hostname("alias"));
        assert!(!registry.is_known_hostname("unknown"));
    }

    #[test]
    fn test_hostname_registry_get_remote() {
        let mut registry = HostnameRegistry::new();

        let remote = RemoteConfig {
            hostname: "test-host".to_string(),
            address: "user@test.local".to_string(),
            ssh_key_path: Some("/path/to/key".to_string()),
            aliases: vec!["alias".to_string()],
        };

        registry.register(remote).unwrap();

        let retrieved = registry.get_remote("test-host");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().hostname, "test-host");
        assert_eq!(retrieved.unwrap().address, "user@test.local");

        // Alias lookup requires resolve first
        let canonical = registry.resolve_alias("alias");
        assert!(canonical.is_some());
        let retrieved_via_alias = registry.get_remote(canonical.unwrap());
        assert!(retrieved_via_alias.is_some());
        assert_eq!(retrieved_via_alias.unwrap().hostname, "test-host");
    }

    #[test]
    fn test_hostname_registry_empty() {
        let registry = HostnameRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);

        let mut registry = HostnameRegistry::new();
        let remote = RemoteConfig {
            hostname: "host".to_string(),
            address: "user@host".to_string(),
            ssh_key_path: None,
            aliases: Vec::new(),
        };
        registry.register(remote).unwrap();

        assert!(!registry.is_empty());
        assert_eq!(registry.len(), 1);
    }
}
