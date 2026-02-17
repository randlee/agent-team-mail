//! WorkerAdapter trait definition
//!
//! The WorkerAdapter trait abstracts over different worker backends
//! (Codex TMUX, SSH, Docker, etc.) to provide a uniform interface for
//! spawning and managing async agent workers.

use crate::plugin::PluginError;
use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

/// Handle to a running worker process
#[derive(Debug)]
pub struct WorkerHandle {
    /// Agent identifier (e.g., "arch-ctm@atm-planning")
    pub agent_id: String,
    /// Backend-assigned process identifier (e.g., tmux pane "%1", container ID, SSH session)
    pub backend_id: String,
    /// Path to the worker's log file
    pub log_file_path: PathBuf,
    /// Backend-specific typed payload (e.g., tmux session info, container metadata)
    ///
    /// Uses Arc so that cloning the handle preserves the payload.
    pub payload: Option<Arc<dyn Any + Send + Sync>>,
}

impl WorkerHandle {
    /// Get a reference to the payload as a specific type
    ///
    /// # Returns
    ///
    /// `Some(&T)` if payload exists and is of type T, `None` otherwise
    ///
    /// # Examples
    ///
    /// ```ignore
    /// #[derive(Debug)]
    /// struct TmuxPayload { session: String, pane_id: String }
    ///
    /// if let Some(tmux) = handle.payload_ref::<TmuxPayload>() {
    ///     println!("Session: {}, Pane: {}", tmux.session, tmux.pane_id);
    /// }
    /// ```
    pub fn payload_ref<T: 'static>(&self) -> Option<&T> {
        self.payload.as_ref()?.downcast_ref::<T>()
    }

}

impl Clone for WorkerHandle {
    fn clone(&self) -> Self {
        Self {
            agent_id: self.agent_id.clone(),
            backend_id: self.backend_id.clone(),
            log_file_path: self.log_file_path.clone(),
            // Arc::clone preserves the payload
            payload: self.payload.clone(),
        }
    }
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
    /// * `command` - Startup command to run in the worker (e.g., "codex --yolo")
    ///
    /// # Returns
    ///
    /// A WorkerHandle for the spawned worker
    ///
    /// # Errors
    ///
    /// Returns PluginError::Runtime if spawn fails
    async fn spawn(&mut self, agent_id: &str, command: &str) -> Result<WorkerHandle, PluginError>;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct TestPayload {
        value: String,
        count: u32,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct OtherPayload {
        data: Vec<u8>,
    }

    #[test]
    fn test_payload_ref_with_correct_type() {
        let payload = TestPayload {
            value: "test".to_string(),
            count: 42,
        };

        let handle = WorkerHandle {
            agent_id: "test-agent".to_string(),
            backend_id: "backend-1".to_string(),
            log_file_path: PathBuf::from("/tmp/test.log"),
            payload: Some(Arc::new(payload)),
        };

        let retrieved = handle.payload_ref::<TestPayload>();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().value, "test");
        assert_eq!(retrieved.unwrap().count, 42);
    }

    #[test]
    fn test_payload_ref_with_wrong_type() {
        let payload = TestPayload {
            value: "test".to_string(),
            count: 42,
        };

        let handle = WorkerHandle {
            agent_id: "test-agent".to_string(),
            backend_id: "backend-1".to_string(),
            log_file_path: PathBuf::from("/tmp/test.log"),
            payload: Some(Arc::new(payload)),
        };

        // Try to retrieve with wrong type
        let retrieved = handle.payload_ref::<OtherPayload>();
        assert!(retrieved.is_none());
    }

    #[test]
    fn test_payload_ref_with_none() {
        let handle = WorkerHandle {
            agent_id: "test-agent".to_string(),
            backend_id: "backend-1".to_string(),
            log_file_path: PathBuf::from("/tmp/test.log"),
            payload: None,
        };

        let retrieved = handle.payload_ref::<TestPayload>();
        assert!(retrieved.is_none());
    }

    #[test]
    fn test_clone_preserves_payload() {
        let payload = TestPayload {
            value: "test".to_string(),
            count: 42,
        };

        let handle = WorkerHandle {
            agent_id: "test-agent".to_string(),
            backend_id: "backend-1".to_string(),
            log_file_path: PathBuf::from("/tmp/test.log"),
            payload: Some(Arc::new(payload)),
        };

        let cloned = handle.clone();

        // Both original and clone have payload
        assert!(handle.payload.is_some());
        assert!(cloned.payload.is_some());

        // Payloads are equal (Arc clones share the same data)
        let original_payload = handle.payload_ref::<TestPayload>().unwrap();
        let cloned_payload = cloned.payload_ref::<TestPayload>().unwrap();
        assert_eq!(original_payload.value, cloned_payload.value);
        assert_eq!(original_payload.count, cloned_payload.count);

        // Other fields are cloned
        assert_eq!(cloned.agent_id, "test-agent");
        assert_eq!(cloned.backend_id, "backend-1");
        assert_eq!(cloned.log_file_path, PathBuf::from("/tmp/test.log"));
    }
}
