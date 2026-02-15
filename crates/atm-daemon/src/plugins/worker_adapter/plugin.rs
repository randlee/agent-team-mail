//! Worker Adapter plugin implementation

use super::codex_tmux::CodexTmuxBackend;
use super::config::WorkersConfig;
use super::trait_def::{WorkerAdapter, WorkerHandle};
use crate::plugin::{Capability, Plugin, PluginContext, PluginError, PluginMetadata};
use atm_core::schema::InboxMessage;
use std::collections::HashMap;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// Worker Adapter plugin — manages async agent teammates in tmux panes
pub struct WorkerAdapterPlugin {
    /// Plugin configuration from [workers]
    config: WorkersConfig,
    /// The worker backend (Codex TMUX, SSH, Docker, etc.)
    backend: Option<Box<dyn WorkerAdapter>>,
    /// Active worker handles
    workers: HashMap<String, WorkerHandle>,
    /// Cached context for runtime use
    ctx: Option<PluginContext>,
}

impl WorkerAdapterPlugin {
    /// Create a new Worker Adapter plugin instance
    pub fn new() -> Self {
        Self {
            config: WorkersConfig::default(),
            backend: None,
            workers: HashMap::new(),
            ctx: None,
        }
    }
}

impl Default for WorkerAdapterPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for WorkerAdapterPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "worker_adapter",
            version: "0.1.0",
            description: "Manages async agent teammates in isolated tmux panes",
            capabilities: vec![
                Capability::EventListener,
                Capability::AdvertiseMembers,
                Capability::InjectMessages,
            ],
        }
    }

    async fn init(&mut self, ctx: &PluginContext) -> Result<(), PluginError> {
        // Parse config from context
        let config_table = ctx.plugin_config("workers");
        self.config = if let Some(table) = config_table {
            WorkersConfig::from_toml(table)?
        } else {
            WorkersConfig::default()
        };

        // If disabled, skip backend setup
        if !self.config.enabled {
            self.ctx = Some(ctx.clone());
            return Ok(());
        }

        // Create the appropriate backend based on config
        let backend: Box<dyn WorkerAdapter> = match self.config.backend.as_str() {
            "codex-tmux" => {
                debug!("Initializing Codex TMUX backend");
                Box::new(CodexTmuxBackend::new(
                    self.config.tmux_session.clone(),
                    self.config.log_dir.clone(),
                ))
            }
            other => {
                return Err(PluginError::Config {
                    message: format!("Unsupported worker backend: '{other}'"),
                });
            }
        };

        self.backend = Some(backend);

        // Store context for runtime use
        self.ctx = Some(ctx.clone());

        debug!("Worker Adapter plugin initialized with {} backend", self.config.backend);

        Ok(())
    }

    async fn run(&mut self, cancel: CancellationToken) -> Result<(), PluginError> {
        // If disabled or no backend, just wait for cancellation
        if !self.config.enabled || self.backend.is_none() {
            cancel.cancelled().await;
            return Ok(());
        }

        // In Sprint 7.1, we just wait for cancellation
        // Sprint 7.2 will implement the event loop for message routing
        debug!("Worker Adapter plugin running (waiting for cancellation)");
        cancel.cancelled().await;

        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), PluginError> {
        // Shut down all active workers
        if let Some(backend) = &mut self.backend {
            for (agent_id, handle) in self.workers.drain() {
                debug!("Shutting down worker for agent {}", agent_id);
                if let Err(e) = backend.shutdown(&handle).await {
                    eprintln!("Failed to shut down worker for {agent_id}: {e}");
                }
            }
        }

        Ok(())
    }

    async fn handle_message(&mut self, _msg: &InboxMessage) -> Result<(), PluginError> {
        // Sprint 7.2 will implement message routing
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = WorkerAdapterPlugin::new();
        let metadata = plugin.metadata();

        assert_eq!(metadata.name, "worker_adapter");
        assert_eq!(metadata.version, "0.1.0");
        assert!(metadata.description.contains("async agent teammates"));

        assert!(metadata.capabilities.contains(&Capability::EventListener));
        assert!(metadata
            .capabilities
            .contains(&Capability::AdvertiseMembers));
        assert!(metadata.capabilities.contains(&Capability::InjectMessages));
    }

    #[test]
    fn test_plugin_default() {
        let plugin = WorkerAdapterPlugin::default();
        assert!(plugin.backend.is_none());
        assert!(plugin.ctx.is_none());
        assert!(plugin.workers.is_empty());
    }
}
