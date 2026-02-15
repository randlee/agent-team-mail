//! WorkerAdapter trait definition
//!
//! The WorkerAdapter trait abstracts over different worker backends
//! (Codex TMUX, SSH, Docker, etc.) to provide a uniform interface for
//! spawning and managing async agent workers.

use crate::plugin::PluginError;
use std::path::PathBuf;

/// Handle to a running worker process
#[derive(Debug, Clone)]
pub struct WorkerHandle {
    /// Agent identifier (e.g., "arch-ctm@atm-planning")
    pub agent_id: String,
    /// TMUX pane identifier (e.g., "%1", "%2")
    pub tmux_pane_id: String,
    /// Path to the worker's log file
    pub log_file_path: PathBuf,
}

/// Trait for worker backends (Codex TMUX, SSH, Docker, etc.)
///
/// Implementors must handle:
/// - Process isolation (tmux panes, containers, etc.)
/// - Log file management
/// - Message delivery
/// - Graceful shutdown
#[async_trait::async_trait]
pub trait WorkerAdapter: Send + Sync {
    /// Spawn a new worker for the given agent
    ///
    /// # Arguments
    ///
    /// * `agent_id` - Full agent identifier (e.g., "arch-ctm@atm-planning")
    /// * `config` - Backend-specific configuration (JSON or similar)
    ///
    /// # Returns
    ///
    /// A WorkerHandle for the spawned worker
    ///
    /// # Errors
    ///
    /// Returns PluginError::Runtime if spawn fails
    async fn spawn(&mut self, agent_id: &str, config: &str) -> Result<WorkerHandle, PluginError>;

    /// Send a message to a running worker
    ///
    /// # Arguments
    ///
    /// * `handle` - Handle to the worker
    /// * `message` - Message text to deliver
    ///
    /// # Returns
    ///
    /// Ok(()) if message was delivered, Err otherwise
    ///
    /// # Errors
    ///
    /// Returns PluginError::Runtime if delivery fails
    async fn send_message(
        &mut self,
        handle: &WorkerHandle,
        message: &str,
    ) -> Result<(), PluginError>;

    /// Gracefully shut down a worker
    ///
    /// # Arguments
    ///
    /// * `handle` - Handle to the worker to shut down
    ///
    /// # Returns
    ///
    /// Ok(()) if shutdown succeeded, Err otherwise
    ///
    /// # Errors
    ///
    /// Returns PluginError::Runtime if shutdown fails
    async fn shutdown(&mut self, handle: &WorkerHandle) -> Result<(), PluginError>;
}
