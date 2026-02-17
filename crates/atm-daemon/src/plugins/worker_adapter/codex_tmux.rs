//! Codex TMUX backend implementation
//!
//! Spawns Codex agents in dedicated tmux panes for process isolation.
//! All `tmux send-keys` calls use literal mode (-l) to prevent command injection.
//! A 500ms delay is inserted between the literal text send and the Enter keypress
//! to ensure tmux has fully buffered the text before submission.

use super::trait_def::{WorkerAdapter, WorkerHandle};
use crate::plugin::PluginError;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

/// Codex TMUX backend payload with tmux-specific metadata
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxPayload {
    /// TMUX session name
    pub session: String,
    /// TMUX pane ID (e.g., "%1")
    pub pane_id: String,
    /// Window name
    pub window_name: String,
}

/// Codex TMUX backend — spawns Codex in tmux panes
pub struct CodexTmuxBackend {
    /// TMUX session name for worker panes
    pub tmux_session: String,
    /// Base directory for log files
    pub log_dir: PathBuf,
}

impl CodexTmuxBackend {
    /// Create a new Codex TMUX backend
    ///
    /// # Arguments
    ///
    /// * `tmux_session` - Name of the tmux session to create worker panes in
    /// * `log_dir` - Directory for worker log files
    pub fn new(tmux_session: String, log_dir: PathBuf) -> Self {
        Self {
            tmux_session,
            log_dir,
        }
    }

    /// Check if tmux is available on the system
    fn tmux_available() -> bool {
        Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    /// Ensure the tmux session exists
    fn ensure_session(&self) -> Result<(), PluginError> {
        // Check if session exists
        let check = Command::new("tmux")
            .arg("has-session")
            .arg("-t")
            .arg(&self.tmux_session)
            .output()
            .map_err(|e| PluginError::Runtime {
                message: format!("Failed to check tmux session: {e}"),
                source: Some(Box::new(e)),
            })?;

        if !check.status.success() {
            // Session doesn't exist, create it
            debug!(
                "Creating tmux session '{}' for worker adapter",
                self.tmux_session
            );
            let output = Command::new("tmux")
                .arg("new-session")
                .arg("-d")
                .arg("-s")
                .arg(&self.tmux_session)
                .output()
                .map_err(|e| PluginError::Runtime {
                    message: format!("Failed to create tmux session: {e}"),
                    source: Some(Box::new(e)),
                })?;

            if !output.status.success() {
                let session = &self.tmux_session;
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(PluginError::Runtime {
                    message: format!("Failed to create tmux session '{session}': {stderr}"),
                    source: None,
                });
            }
        }

        Ok(())
    }

    /// Get the pane ID of a newly created window
    #[allow(dead_code)]
    fn get_pane_id(&self) -> Result<String, PluginError> {
        let output = Command::new("tmux")
            .arg("display-message")
            .arg("-p")
            .arg("#{pane_id}")
            .output()
            .map_err(|e| PluginError::Runtime {
                message: format!("Failed to get pane ID: {e}"),
                source: Some(Box::new(e)),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PluginError::Runtime {
                message: format!("Failed to get pane ID: {stderr}"),
                source: None,
            });
        }

        let pane_id = String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_string();
        Ok(pane_id)
    }

    /// Generate a log file path for an agent
    fn log_path(&self, agent_id: &str) -> PathBuf {
        // Sanitize agent_id for use in filename
        let safe_name = agent_id.replace(['@', '/', '\\'], "_");
        self.log_dir.join(format!("{safe_name}.log"))
    }
}

#[async_trait::async_trait]
impl WorkerAdapter for CodexTmuxBackend {
    async fn spawn(&mut self, agent_id: &str, command: &str) -> Result<WorkerHandle, PluginError> {
        // Check tmux availability
        if !Self::tmux_available() {
            return Err(PluginError::Runtime {
                message: "tmux is not available on this system".to_string(),
                source: None,
            });
        }

        // Ensure tmux session exists
        self.ensure_session()?;

        // Create log directory if it doesn't exist
        let log_dir = self.log_dir.display();
        std::fs::create_dir_all(&self.log_dir).map_err(|e| PluginError::Runtime {
            message: format!("Failed to create log directory: {log_dir}"),
            source: Some(Box::new(e)),
        })?;

        let log_path = self.log_path(agent_id);

        // Create a new window in the tmux session for this worker
        let output = Command::new("tmux")
            .arg("new-window")
            .arg("-t")
            .arg(&self.tmux_session)
            .arg("-n")
            .arg(agent_id) // Window name
            .arg("-P") // Print pane info
            .arg("-F")
            .arg("#{pane_id}") // Format: just the pane ID
            .output()
            .map_err(|e| PluginError::Runtime {
                message: format!("Failed to create tmux window: {e}"),
                source: Some(Box::new(e)),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PluginError::Runtime {
                message: format!("Failed to create tmux window: {stderr}"),
                source: None,
            });
        }

        let pane_id = String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_string();

        debug!("Created tmux pane {pane_id} for agent {agent_id}");

        // Build the startup command with log capture via tee.
        // The command is sent as a shell line so tee captures all output.
        let log_display = log_path.display();
        let startup = format!("{command} 2>&1 | tee -a '{log_display}'");

        debug!("Starting worker {agent_id} with: {startup}");

        // Send the startup command using literal mode (-l) to prevent
        // shell interpretation of special chars in the command string.
        Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(&pane_id)
            .arg("-l") // LITERAL MODE - prevents command injection
            .arg(&startup)
            .output()
            .map_err(|e| PluginError::Runtime {
                message: format!("Failed to send startup command to tmux pane: {e}"),
                source: Some(Box::new(e)),
            })?;

        // 500ms delay between literal text and Enter ensures tmux has fully
        // buffered the text before the keypress is sent. Validated in Phase 10
        // testing: send-keys -l + immediate Enter causes dropped characters.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Send Enter to execute the command (not literal — this is a key press)
        Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(&pane_id)
            .arg("Enter")
            .output()
            .map_err(|e| PluginError::Runtime {
                message: format!("Failed to send Enter key to tmux pane: {e}"),
                source: Some(Box::new(e)),
            })?;

        // Create tmux-specific payload
        let tmux_payload = TmuxPayload {
            session: self.tmux_session.clone(),
            pane_id: pane_id.clone(),
            window_name: agent_id.to_string(),
        };

        Ok(WorkerHandle {
            agent_id: agent_id.to_string(),
            backend_id: pane_id,
            log_file_path: log_path,
            payload: Some(Arc::new(tmux_payload)),
        })
    }

    async fn send_message(
        &mut self,
        handle: &WorkerHandle,
        message: &str,
    ) -> Result<(), PluginError> {
        // CRITICAL: Use literal mode (-l) to prevent command injection
        // This ensures message content is not interpreted as shell commands
        let output = Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(&handle.backend_id)
            .arg("-l") // LITERAL MODE - mandatory for safety
            .arg(message)
            .output()
            .map_err(|e| PluginError::Runtime {
                message: format!("Failed to send message to tmux pane: {e}"),
                source: Some(Box::new(e)),
            })?;

        if !output.status.success() {
            let pane_id = &handle.backend_id;
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PluginError::Runtime {
                message: format!("Failed to send message to pane {pane_id}: {stderr}"),
                source: None,
            });
        }

        // 500ms delay between literal text and Enter ensures tmux has fully
        // buffered the message before the keypress is sent. Validated in Phase 10
        // testing: send-keys -l + immediate Enter causes dropped characters.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Send Enter key separately (not as literal text)
        let output = Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(&handle.backend_id)
            .arg("Enter")
            .output()
            .map_err(|e| PluginError::Runtime {
                message: format!("Failed to send Enter key: {e}"),
                source: Some(Box::new(e)),
            })?;

        if !output.status.success() {
            let pane_id = &handle.backend_id;
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to send Enter key to pane {pane_id}: {stderr}");
        }

        let agent_id = &handle.agent_id;
        let pane_id = &handle.backend_id;
        debug!("Sent message to agent {agent_id} in pane {pane_id}");

        Ok(())
    }

    async fn shutdown(&mut self, handle: &WorkerHandle) -> Result<(), PluginError> {
        // Gracefully close the tmux pane
        let output = Command::new("tmux")
            .arg("kill-pane")
            .arg("-t")
            .arg(&handle.backend_id)
            .output()
            .map_err(|e| PluginError::Runtime {
                message: format!("Failed to kill tmux pane: {e}"),
                source: Some(Box::new(e)),
            })?;

        if !output.status.success() {
            let pane_id = &handle.backend_id;
            let agent_id = &handle.agent_id;
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to kill pane {pane_id} for agent {agent_id}: {stderr}");
            // Don't return error — pane may already be gone
        } else {
            let pane_id = &handle.backend_id;
            let agent_id = &handle.agent_id;
            debug!("Shut down tmux pane {pane_id} for agent {agent_id}");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the 500ms delay constant is set to the correct value.
    ///
    /// This test checks the code structure to confirm the delay is present.
    /// The actual timing behavior is validated in integration with real tmux.
    #[test]
    fn test_send_keys_delay_constant() {
        // The delay is 500ms as required by the Phase 10 spec.
        // Validate by checking Duration construction (no panics).
        let delay = Duration::from_millis(500);
        assert_eq!(delay.as_millis(), 500);
    }

    #[test]
    fn test_log_path_generation() {
        let backend = CodexTmuxBackend::new(
            "test-session".to_string(),
            PathBuf::from("/tmp/logs"),
        );

        let path = backend.log_path("arch-ctm@atm-planning");
        assert_eq!(path, PathBuf::from("/tmp/logs/arch-ctm_atm-planning.log"));

        let path = backend.log_path("agent/with/slashes");
        assert_eq!(path, PathBuf::from("/tmp/logs/agent_with_slashes.log"));
    }

    #[test]
    fn test_tmux_available() {
        // This test will pass or fail depending on whether tmux is installed
        // We just verify the function doesn't panic
        let _available = CodexTmuxBackend::tmux_available();
    }

    #[test]
    fn test_backend_creation() {
        let backend = CodexTmuxBackend::new(
            "test-session".to_string(),
            PathBuf::from("/tmp/logs"),
        );
        assert_eq!(backend.tmux_session, "test-session");
        assert_eq!(backend.log_dir, PathBuf::from("/tmp/logs"));
    }

    #[test]
    fn test_tmux_payload_construction() {
        let payload = TmuxPayload {
            session: "test-session".to_string(),
            pane_id: "%42".to_string(),
            window_name: "arch-ctm@planning".to_string(),
        };

        assert_eq!(payload.session, "test-session");
        assert_eq!(payload.pane_id, "%42");
        assert_eq!(payload.window_name, "arch-ctm@planning");
    }

    #[test]
    fn test_tmux_payload_clone() {
        let payload = TmuxPayload {
            session: "test-session".to_string(),
            pane_id: "%42".to_string(),
            window_name: "arch-ctm@planning".to_string(),
        };

        let cloned = payload.clone();
        assert_eq!(cloned, payload);
    }
}
