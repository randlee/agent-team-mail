use super::traits::ErasedPlugin;
use super::{Capability, Plugin, PluginContext, PluginError, PluginMetadata, PluginState};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Type alias for a plugin wrapped in Arc<Mutex<>> for concurrent access
pub type SharedPlugin = Arc<Mutex<Box<dyn ErasedPlugin>>>;

/// Entry in the registry tracking a plugin and its state
struct PluginEntry {
    plugin: Box<dyn ErasedPlugin>,
    state: PluginState,
}

/// Manages plugin lifecycle and discovery
pub struct PluginRegistry {
    plugins: Vec<PluginEntry>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    /// Register a plugin. It starts in Created state.
    pub fn register<P: Plugin + 'static>(&mut self, plugin: P) {
        self.plugins.push(PluginEntry {
            plugin: Box::new(plugin),
            state: PluginState::Created,
        });
    }

    /// Initialize all registered plugins
    pub async fn init_all(&mut self, ctx: &PluginContext) -> Result<(), PluginError> {
        for entry in &mut self.plugins {
            entry.plugin.init(ctx).await?;
            entry.state = PluginState::Initialized;
        }
        Ok(())
    }

    /// Get plugin metadata and state by name
    pub fn get_by_name(&self, name: &str) -> Option<(PluginMetadata, PluginState)> {
        self.plugins
            .iter()
            .find(|e| e.plugin.metadata().name == name)
            .map(|e| (e.plugin.metadata(), e.state))
    }

    /// Get metadata for all plugins with a given capability
    pub fn get_by_capability(&self, cap: &Capability) -> Vec<(PluginMetadata, PluginState)> {
        self.plugins
            .iter()
            .filter(|e| e.plugin.metadata().capabilities.contains(cap))
            .map(|e| (e.plugin.metadata(), e.state))
            .collect()
    }

    /// Number of registered plugins
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Get the state of a plugin by name
    pub fn state_of(&self, name: &str) -> Option<PluginState> {
        self.plugins
            .iter()
            .find(|e| e.plugin.metadata().name == name)
            .map(|e| e.state)
    }

    /// Update the state of a plugin by name
    pub fn set_state(&mut self, name: &str, state: PluginState) -> bool {
        if let Some(entry) = self.plugins.iter_mut().find(|e| e.plugin.metadata().name == name) {
            entry.state = state;
            true
        } else {
            false
        }
    }

    /// Take all plugins out of the registry for task spawning.
    /// Each plugin is wrapped in Arc<Mutex<>> for safe concurrent access.
    /// Transitions all plugins to Running state.
    pub fn take_plugins(&mut self) -> Vec<(PluginMetadata, SharedPlugin)> {
        self.plugins
            .drain(..)
            .map(|mut entry| {
                entry.state = PluginState::Running;
                let metadata = entry.plugin.metadata();
                (metadata, Arc::new(Mutex::new(entry.plugin)))
            })
            .collect()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}
