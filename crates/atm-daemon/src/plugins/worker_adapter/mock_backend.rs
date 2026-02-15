//! Mock worker backend for testing
//!
//! Provides a fake WorkerAdapter implementation that doesn't require tmux or Codex.
//! Used for integration tests on all platforms including Windows CI.

use super::trait_def::{WorkerAdapter, WorkerHandle};
use crate::plugin::PluginError;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::debug;

/// Call record for mock backend operations
#[derive(Debug, Clone)]
pub enum MockCall {
    Spawn { agent_id: String },
    SendMessage { agent_id: String, message: String },
    Shutdown { agent_id: String },
}

/// Shared state for mock backend
#[derive(Debug, Default)]
struct MockState {
    calls: Vec<MockCall>,
    spawned_workers: HashMap<String, WorkerHandle>,
    send_message_error: Option<String>,
    spawn_error: Option<String>,
    shutdown_error: Option<String>,
}

/// Mock worker backend for testing without real tmux/Codex
///
/// Records all operations and allows injection of errors for testing failure scenarios.
#[derive(Clone)]
pub struct MockTmuxBackend {
    state: Arc<Mutex<MockState>>,
    log_dir: PathBuf,
}

impl MockTmuxBackend {
    /// Create a new mock backend
    ///
    /// # Arguments
    ///
    /// * `log_dir` - Directory for fake log files
    pub fn new(log_dir: PathBuf) -> Self {
        Self {
            state: Arc::new(Mutex::new(MockState::default())),
            log_dir,
        }
    }

    /// Get all recorded calls
    pub fn get_calls(&self) -> Vec<MockCall> {
        self.state.lock().unwrap().calls.clone()
    }

    /// Clear all recorded calls
    pub fn clear_calls(&self) {
        self.state.lock().unwrap().calls.clear();
    }

    /// Inject a send_message error (next send will fail with this message)
    pub fn set_send_message_error(&self, error: Option<String>) {
        self.state.lock().unwrap().send_message_error = error;
    }

    /// Inject a spawn error (next spawn will fail with this message)
    pub fn set_spawn_error(&self, error: Option<String>) {
        self.state.lock().unwrap().spawn_error = error;
    }

    /// Inject a shutdown error (next shutdown will fail with this message)
    pub fn set_shutdown_error(&self, error: Option<String>) {
        self.state.lock().unwrap().shutdown_error = error;
    }

    /// Check if a worker was spawned
    pub fn is_spawned(&self, agent_id: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .spawned_workers
            .contains_key(agent_id)
    }

    /// Get count of spawned workers
    pub fn spawned_count(&self) -> usize {
        self.state.lock().unwrap().spawned_workers.len()
    }

    /// Write a mock response to a worker's log file
    ///
    /// Used to simulate worker responses for testing response capture.
    ///
    /// # Arguments
    ///
    /// * `agent_id` - Agent identifier
    /// * `response` - Response text to write to log file
    pub fn write_mock_response(&self, agent_id: &str, response: &str) -> std::io::Result<()> {
        let handle = {
            let state = self.state.lock().unwrap();
            state.spawned_workers.get(agent_id).cloned()
        };

        if let Some(handle) = handle {
            std::fs::write(&handle.log_file_path, response)?;
            debug!("Wrote mock response to {}", handle.log_file_path.display());
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl WorkerAdapter for MockTmuxBackend {
    async fn spawn(&mut self, agent_id: &str, _config: &str) -> Result<WorkerHandle, PluginError> {
        let mut state = self.state.lock().unwrap();

        // Check for injected error
        if let Some(error_msg) = &state.spawn_error {
            let error = error_msg.clone();
            state.spawn_error = None; // Clear after use
            return Err(PluginError::Runtime {
                message: error,
                source: None,
            });
        }

        // Record call
        state.calls.push(MockCall::Spawn {
            agent_id: agent_id.to_string(),
        });

        // Create fake log file
        std::fs::create_dir_all(&self.log_dir).map_err(|e| PluginError::Runtime {
            message: format!("Failed to create log directory: {e}"),
            source: Some(Box::new(e)),
        })?;

        let safe_name = agent_id.replace(['@', '/', '\\'], "_");
        let log_path = self.log_dir.join(format!("{safe_name}.log"));

        // Create empty log file
        std::fs::write(&log_path, "").map_err(|e| PluginError::Runtime {
            message: format!("Failed to create log file: {e}"),
            source: Some(Box::new(e)),
        })?;

        let handle = WorkerHandle {
            agent_id: agent_id.to_string(),
            tmux_pane_id: format!("mock-pane-{agent_id}"),
            log_file_path: log_path,
        };

        state.spawned_workers.insert(agent_id.to_string(), handle.clone());

        debug!("Mock backend spawned worker for {agent_id}");
        Ok(handle)
    }

    async fn send_message(
        &mut self,
        handle: &WorkerHandle,
        message: &str,
    ) -> Result<(), PluginError> {
        let mut state = self.state.lock().unwrap();

        // Check for injected error
        if let Some(error_msg) = &state.send_message_error {
            let error = error_msg.clone();
            state.send_message_error = None; // Clear after use
            return Err(PluginError::Runtime {
                message: error,
                source: None,
            });
        }

        // Record call
        state.calls.push(MockCall::SendMessage {
            agent_id: handle.agent_id.clone(),
            message: message.to_string(),
        });

        debug!("Mock backend sent message to {}", handle.agent_id);
        Ok(())
    }

    async fn shutdown(&mut self, handle: &WorkerHandle) -> Result<(), PluginError> {
        let mut state = self.state.lock().unwrap();

        // Check for injected error
        if let Some(error_msg) = &state.shutdown_error {
            let error = error_msg.clone();
            state.shutdown_error = None; // Clear after use
            return Err(PluginError::Runtime {
                message: error,
                source: None,
            });
        }

        // Record call
        state.calls.push(MockCall::Shutdown {
            agent_id: handle.agent_id.clone(),
        });

        // Remove from spawned workers
        state.spawned_workers.remove(&handle.agent_id);

        debug!("Mock backend shut down worker {}", handle.agent_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_mock_backend_spawn() {
        let temp_dir = TempDir::new().unwrap();
        let mut backend = MockTmuxBackend::new(temp_dir.path().to_path_buf());

        let handle = backend.spawn("test-agent", "{}").await.unwrap();

        assert_eq!(handle.agent_id, "test-agent");
        assert_eq!(handle.tmux_pane_id, "mock-pane-test-agent");
        assert!(handle.log_file_path.exists());
        assert!(backend.is_spawned("test-agent"));
        assert_eq!(backend.spawned_count(), 1);

        let calls = backend.get_calls();
        assert_eq!(calls.len(), 1);
        matches!(calls[0], MockCall::Spawn { .. });
    }

    #[tokio::test]
    async fn test_mock_backend_send_message() {
        let temp_dir = TempDir::new().unwrap();
        let mut backend = MockTmuxBackend::new(temp_dir.path().to_path_buf());

        let handle = backend.spawn("test-agent", "{}").await.unwrap();
        backend.clear_calls();

        backend
            .send_message(&handle, "Hello, agent!")
            .await
            .unwrap();

        let calls = backend.get_calls();
        assert_eq!(calls.len(), 1);
        if let MockCall::SendMessage { agent_id, message } = &calls[0] {
            assert_eq!(agent_id, "test-agent");
            assert_eq!(message, "Hello, agent!");
        } else {
            panic!("Expected SendMessage call");
        }
    }

    #[tokio::test]
    async fn test_mock_backend_shutdown() {
        let temp_dir = TempDir::new().unwrap();
        let mut backend = MockTmuxBackend::new(temp_dir.path().to_path_buf());

        let handle = backend.spawn("test-agent", "{}").await.unwrap();
        assert!(backend.is_spawned("test-agent"));

        backend.shutdown(&handle).await.unwrap();

        assert!(!backend.is_spawned("test-agent"));
        assert_eq!(backend.spawned_count(), 0);
    }

    #[tokio::test]
    async fn test_mock_backend_error_injection() {
        let temp_dir = TempDir::new().unwrap();
        let mut backend = MockTmuxBackend::new(temp_dir.path().to_path_buf());

        // Test spawn error
        backend.set_spawn_error(Some("Mock spawn failure".to_string()));
        let result = backend.spawn("test-agent", "{}").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Mock spawn failure"));

        // Test send_message error
        let handle = backend.spawn("test-agent", "{}").await.unwrap();
        backend.set_send_message_error(Some("Mock send failure".to_string()));
        let result = backend.send_message(&handle, "test").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Mock send failure"));

        // Test shutdown error
        backend.set_shutdown_error(Some("Mock shutdown failure".to_string()));
        let result = backend.shutdown(&handle).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Mock shutdown failure"));
    }

    #[tokio::test]
    async fn test_write_mock_response() {
        let temp_dir = TempDir::new().unwrap();
        let mut backend = MockTmuxBackend::new(temp_dir.path().to_path_buf());

        let handle = backend.spawn("test-agent", "{}").await.unwrap();

        backend
            .write_mock_response("test-agent", "Mock response text")
            .unwrap();

        let content = std::fs::read_to_string(&handle.log_file_path).unwrap();
        assert_eq!(content, "Mock response text");
    }
}
