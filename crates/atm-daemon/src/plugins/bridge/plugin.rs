//! Bridge plugin implementation

use super::config::BridgePluginConfig;
use super::self_write_filter::SelfWriteFilter;
use super::sync::SyncEngine;
use crate::plugin::{Capability, Plugin, PluginContext, PluginError, PluginMetadata};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Bridge plugin — synchronizes agent inbox queues across machines
pub struct BridgePlugin {
    /// Plugin configuration (populated during init)
    config: Option<BridgePluginConfig>,

    /// Sync engine (populated during init if enabled)
    sync_engine: Option<Arc<Mutex<SyncEngine>>>,

    /// Self-write filter to prevent feedback loops
    /// TODO: Wire up with watcher events once event handling is implemented
    #[allow(dead_code)]
    self_write_filter: Arc<Mutex<SelfWriteFilter>>,

    /// Team directory
    team_dir: Option<std::path::PathBuf>,
}

impl BridgePlugin {
    /// Create a new Bridge plugin instance
    pub fn new() -> Self {
        Self {
            config: None,
            sync_engine: None,
            self_write_filter: Arc::new(Mutex::new(SelfWriteFilter::default())),
            team_dir: None,
        }
    }
}

impl Default for BridgePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for BridgePlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "bridge",
            version: "0.1.0",
            description: "Cross-computer queue synchronization via SSH/SFTP",
            capabilities: vec![Capability::EventListener],
        }
    }

    async fn init(&mut self, ctx: &PluginContext) -> Result<(), PluginError> {
        // Parse config from context
        let config_table = ctx.plugin_config("bridge");

        let config = if let Some(table) = config_table {
            BridgePluginConfig::from_toml(table, &ctx.system.hostname)?
        } else {
            // No config section - create disabled default
            let default_table: toml::Table = toml::from_str("enabled = false")
                .map_err(|e| PluginError::Config {
                    message: format!("Failed to create default config: {e}"),
                })?;
            BridgePluginConfig::from_toml(&default_table, &ctx.system.hostname)?
        };

        if !config.is_enabled() {
            info!("Bridge plugin disabled in config");
            self.config = Some(config);
            return Ok(());
        }

        // Log bridge configuration
        info!(
            "Bridge plugin initialized: hostname={}, role={:?}, remotes={}",
            config.local_hostname,
            config.core.role,
            config.registry.len()
        );

        debug!("Bridge sync interval: {} seconds", config.core.sync_interval_secs);

        // Log registered remotes
        for remote in config.registry.remotes() {
            debug!(
                "Registered remote: {} ({}) with {} alias(es)",
                remote.hostname,
                remote.address,
                remote.aliases.len()
            );
        }

        // Create sync engine with mock transport for now
        // TODO: Sprint 8.3 will replace this with SSH transport
        let transport = Arc::new(super::mock_transport::MockTransport::new()) as Arc<dyn super::transport::Transport>;

        // Get team directory from mail service
        let team_dir = ctx.mail.teams_root().join(&ctx.system.default_team);

        // Cleanup stale temp files on startup
        if let Err(e) = super::team_config_sync::cleanup_stale_tmp_files(&team_dir).await {
            warn!("Failed to cleanup stale temp files: {}", e);
        }

        let sync_engine = SyncEngine::new(
            Arc::new(config.clone()),
            transport,
            team_dir.clone(),
        )
        .await
        .map_err(|e| PluginError::Runtime {
            message: format!("Failed to create sync engine: {e}"),
            source: None,
        })?;

        self.sync_engine = Some(Arc::new(Mutex::new(sync_engine)));
        self.team_dir = Some(team_dir);
        self.config = Some(config);

        Ok(())
    }

    async fn run(&mut self, cancel: CancellationToken) -> Result<(), PluginError> {
        let config = self.config.as_ref().ok_or_else(|| PluginError::Runtime {
            message: "Plugin not initialized".to_string(),
            source: None,
        })?;

        // If disabled, just wait for cancellation
        if !config.is_enabled() {
            cancel.cancelled().await;
            return Ok(());
        }

        let sync_engine = self.sync_engine.as_ref().ok_or_else(|| PluginError::Runtime {
            message: "Sync engine not initialized".to_string(),
            source: None,
        })?;

        info!("Bridge plugin running with sync interval: {} seconds", config.core.sync_interval_secs);

        let mut interval = tokio::time::interval(Duration::from_secs(config.core.sync_interval_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("Bridge plugin stopping");
                    break;
                }
                _ = interval.tick() => {
                    debug!("Running sync cycle");

                    let mut engine = sync_engine.lock().await;
                    match engine.sync_cycle().await {
                        Ok(stats) => {
                            if stats.messages_pushed > 0 || stats.messages_pulled > 0 || stats.errors > 0 {
                                info!(
                                    "Sync cycle: pushed={}, pulled={}, errors={}",
                                    stats.messages_pushed, stats.messages_pulled, stats.errors
                                );
                            }
                        }
                        Err(e) => {
                            error!("Sync cycle failed: {}", e);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), PluginError> {
        if let Some(config) = &self.config
            && config.is_enabled()
        {
            info!("Bridge plugin shutting down");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atm_core::config::Config;
    use atm_core::context::{Platform, SystemContext};
    use crate::plugin::MailService;
    use crate::roster::RosterService;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn create_test_context(config: Config) -> PluginContext {
        let temp_dir = TempDir::new().unwrap();
        let teams_root = temp_dir.path().to_path_buf();

        let system = SystemContext::new(
            "test-hostname".to_string(),
            Platform::Linux,
            PathBuf::from("/tmp/.claude"),
            "0.1.0".to_string(),
            "test-team".to_string(),
        );

        let mail = MailService::new(teams_root.clone());
        let roster = RosterService::new(teams_root);

        PluginContext::new(
            Arc::new(system),
            Arc::new(mail),
            Arc::new(config),
            Arc::new(roster),
        )
    }

    #[test]
    fn test_plugin_metadata() {
        let plugin = BridgePlugin::new();
        let metadata = plugin.metadata();

        assert_eq!(metadata.name, "bridge");
        assert_eq!(metadata.version, "0.1.0");
        assert!(metadata.description.contains("Cross-computer"));
        assert!(metadata.capabilities.contains(&Capability::EventListener));
    }

    #[tokio::test]
    async fn test_plugin_init_disabled() {
        let mut plugin = BridgePlugin::new();

        let toml_str = r#"
[plugins.bridge]
enabled = false
"#;

        let config: Config = toml::from_str(toml_str).unwrap();
        let ctx = create_test_context(config);

        let result = plugin.init(&ctx).await;
        assert!(result.is_ok());
        assert!(plugin.config.is_some());
        assert!(!plugin.config.as_ref().unwrap().is_enabled());
    }

    #[tokio::test]
    async fn test_plugin_init_no_config_section() {
        let mut plugin = BridgePlugin::new();

        let config = Config::default();
        let ctx = create_test_context(config);

        let result = plugin.init(&ctx).await;
        assert!(result.is_ok());
        assert!(plugin.config.is_some());
        assert!(!plugin.config.as_ref().unwrap().is_enabled());
    }

    #[tokio::test]
    async fn test_plugin_init_enabled_with_remotes() {
        let mut plugin = BridgePlugin::new();

        let toml_str = r#"
[plugins.bridge]
enabled = true
local_hostname = "test-laptop"
role = "spoke"
sync_interval_secs = 120

[[plugins.bridge.remotes]]
hostname = "hub"
address = "user@hub.local"
aliases = ["main-hub"]
"#;

        let config: Config = toml::from_str(toml_str).unwrap();
        let ctx = create_test_context(config);

        let result = plugin.init(&ctx).await;
        assert!(result.is_ok());

        let plugin_config = plugin.config.as_ref().unwrap();
        assert!(plugin_config.is_enabled());
        assert_eq!(plugin_config.local_hostname, "test-laptop");
        assert_eq!(plugin_config.core.sync_interval_secs, 120);
        assert_eq!(plugin_config.registry.len(), 1);
        assert!(plugin_config.registry.is_known_hostname("hub"));
        assert!(plugin_config.registry.is_known_hostname("main-hub"));
    }

    #[tokio::test]
    async fn test_plugin_init_enabled_without_remotes_error() {
        let mut plugin = BridgePlugin::new();

        let toml_str = r#"
[plugins.bridge]
enabled = true
"#;

        let config: Config = toml::from_str(toml_str).unwrap();
        let ctx = create_test_context(config);

        let result = plugin.init(&ctx).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no remotes configured"));
    }

    #[tokio::test]
    async fn test_plugin_init_hostname_collision_error() {
        let mut plugin = BridgePlugin::new();

        let toml_str = r#"
[plugins.bridge]
enabled = true

[[plugins.bridge.remotes]]
hostname = "server"
address = "user@server1.local"

[[plugins.bridge.remotes]]
hostname = "server"
address = "user@server2.local"
"#;

        let config: Config = toml::from_str(toml_str).unwrap();
        let ctx = create_test_context(config);

        let result = plugin.init(&ctx).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("already registered"));
    }

    #[tokio::test]
    async fn test_plugin_run_disabled_waits_for_cancel() {
        let mut plugin = BridgePlugin::new();

        let config = Config::default();
        let ctx = create_test_context(config);

        plugin.init(&ctx).await.unwrap();

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        let run_handle = tokio::spawn(async move {
            plugin.run(cancel_clone).await
        });

        // Cancel immediately
        cancel.cancel();

        let result = run_handle.await.unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_plugin_run_enabled_waits_for_cancel() {
        let mut plugin = BridgePlugin::new();

        let toml_str = r#"
[plugins.bridge]
enabled = true

[[plugins.bridge.remotes]]
hostname = "remote"
address = "user@remote.local"
"#;

        let config: Config = toml::from_str(toml_str).unwrap();
        let ctx = create_test_context(config);

        plugin.init(&ctx).await.unwrap();

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        let run_handle = tokio::spawn(async move {
            plugin.run(cancel_clone).await
        });

        // Cancel after short delay
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        cancel.cancel();

        let result = run_handle.await.unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_plugin_shutdown() {
        let mut plugin = BridgePlugin::new();

        let toml_str = r#"
[plugins.bridge]
enabled = true

[[plugins.bridge.remotes]]
hostname = "remote"
address = "user@remote.local"
"#;

        let config: Config = toml::from_str(toml_str).unwrap();
        let ctx = create_test_context(config);

        plugin.init(&ctx).await.unwrap();

        let result = plugin.shutdown().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_plugin_default() {
        let plugin = BridgePlugin::default();
        assert!(plugin.config.is_none());
    }
}
