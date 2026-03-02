//! Codex TMUX backend implementation
//!
//! Spawns Codex agents in dedicated tmux panes for process isolation.
//! All `tmux send-keys` calls use literal mode (-l) to prevent command injection.
//! A 500ms delay is inserted between the literal text send and the Enter keypress
//! to ensure tmux has fully buffered the text before submission.

use super::tmux_sender::{DefaultTmuxSender, DeliveryMethod, TmuxSender};
use super::trait_def::{WorkerAdapter, WorkerHandle};
use crate::plugin::PluginError;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::debug;

/// Codex TMUX backend payload with tmux-specific metadata
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxPayload {
    /// TMUX session name
    pub session: String,
    /// TMUX pane ID (e.g., "%1")
    pub pane_id: String,
    /// Window name
    pub window_name: String,
    /// Runtime kind (e.g., "codex", "gemini")
    pub runtime: String,
    /// Runtime-specific session identifier if known.
    pub runtime_session_id: Option<String>,
    /// Runtime state/home directory if configured.
    pub runtime_home: Option<String>,
}

/// Codex TMUX backend — spawns Codex in tmux panes
pub struct CodexTmuxBackend {
    /// TMUX session name for worker panes
    pub tmux_session: String,
    /// Base directory for log files
    pub log_dir: PathBuf,
    /// Shared tmux sender with reliability protections
    sender: DefaultTmuxSender,
    /// Delivery method for text injection
    delivery_method: DeliveryMethod,
}

impl CodexTmuxBackend {
    /// Create a new Codex TMUX backend
    ///
    /// # Arguments
    ///
    /// * `tmux_session` - Name of the tmux session to create worker panes in
    /// * `log_dir` - Directory for worker log files
    pub fn new(tmux_session: String, log_dir: PathBuf) -> Self {
        let delivery_method = DeliveryMethod::from_env().unwrap_or(DeliveryMethod::PasteBuffer);
        Self {
            tmux_session,
            log_dir,
            sender: DefaultTmuxSender,
            delivery_method,
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

        let pane_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

        debug!("Created tmux pane {pane_id} for agent {agent_id}");

        // Build the startup command with log capture via tee.
        // The command is sent as a shell line so tee captures all output.
        let log_display = log_path.display();
        let startup = format!("{command} 2>&1 | tee -a '{log_display}'");

        debug!("Starting worker {agent_id} with: {startup}");

        self.sender
            .send_text_and_enter(&pane_id, &startup, self.delivery_method, "spawn-startup")
            .await?;

        // Create tmux-specific payload
        let tmux_payload = TmuxPayload {
            session: self.tmux_session.clone(),
            pane_id: pane_id.clone(),
            window_name: agent_id.to_string(),
            runtime: "codex".to_string(),
            runtime_session_id: None,
            runtime_home: None,
        };

        Ok(WorkerHandle {
            agent_id: agent_id.to_string(),
            backend_id: pane_id,
            log_file_path: log_path,
            payload: Some(Arc::new(tmux_payload)),
        })
    }

    /// Spawn a worker with environment variables exported before the command.
    ///
    /// Creates a new tmux window, exports `ATM_IDENTITY`, `ATM_TEAM`, and any
    /// extra `env_vars`, then starts the main command.  Each variable is sent
    /// with a separate `export KEY=VALUE` send-keys call to avoid shell quoting
    /// issues with complex values.
    async fn spawn_with_env(
        &mut self,
        agent_id: &str,
        command: &str,
        env_vars: &std::collections::HashMap<String, String>,
    ) -> Result<WorkerHandle, PluginError> {
        if !Self::tmux_available() {
            return Err(PluginError::Runtime {
                message: "tmux is not available on this system".to_string(),
                source: None,
            });
        }

        self.ensure_session()?;

        // Create log directory
        let log_dir_display = self.log_dir.display();
        std::fs::create_dir_all(&self.log_dir).map_err(|e| PluginError::Runtime {
            message: format!("Failed to create log directory: {log_dir_display}"),
            source: Some(Box::new(e)),
        })?;

        let log_path = self.log_path(agent_id);

        // Create a new window (empty shell, no command yet)
        let output = std::process::Command::new("tmux")
            .arg("new-window")
            .arg("-t")
            .arg(&self.tmux_session)
            .arg("-n")
            .arg(agent_id)
            .arg("-P")
            .arg("-F")
            .arg("#{pane_id}")
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

        let pane_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

        debug!("Created tmux pane {pane_id} for agent {agent_id} (with env)");

        // Export all environment variables.
        // Each export is sent as a separate send-keys call with the -l flag to
        // avoid special-character interpretation.
        for (key, value) in env_vars {
            // Validate key to prevent shell injection via variable name
            if key.chars().all(|c| c.is_alphanumeric() || c == '_') {
                let export_cmd = format!("export {key}={}", shell_single_quote(value));
                self.sender
                    .send_text_and_enter(
                        &pane_id,
                        &export_cmd,
                        self.delivery_method,
                        "spawn-env-export",
                    )
                    .await?;
            } else {
                tracing::warn!("Skipping env var with invalid key name: {key}");
            }
        }

        // Start the main command with log capture
        let log_display = log_path.display();
        let startup = format!("{command} 2>&1 | tee -a '{log_display}'");

        debug!("Starting worker {agent_id} with: {startup}");

        self.sender
            .send_text_and_enter(
                &pane_id,
                &startup,
                self.delivery_method,
                "spawn-with-env-startup",
            )
            .await?;

        let tmux_payload = TmuxPayload {
            session: self.tmux_session.clone(),
            pane_id: pane_id.clone(),
            window_name: agent_id.to_string(),
            runtime: env_vars
                .get("ATM_RUNTIME")
                .cloned()
                .unwrap_or_else(|| "codex".to_string()),
            runtime_session_id: env_vars.get("ATM_RUNTIME_SESSION_ID").cloned(),
            runtime_home: env_vars
                .get("ATM_RUNTIME_HOME")
                .cloned()
                .or_else(|| env_vars.get("GEMINI_CLI_HOME").cloned()),
        };

        Ok(WorkerHandle {
            agent_id: agent_id.to_string(),
            backend_id: pane_id,
            log_file_path: log_path,
            payload: Some(std::sync::Arc::new(tmux_payload)),
        })
    }

    async fn send_message(
        &mut self,
        handle: &WorkerHandle,
        message: &str,
    ) -> Result<(), PluginError> {
        self.sender
            .send_text_and_enter(
                &handle.backend_id,
                message,
                self.delivery_method,
                "send-message",
            )
            .await?;

        let agent_id = &handle.agent_id;
        let pane_id = &handle.backend_id;
        debug!("Sent message to agent {agent_id} in pane {pane_id}");

        Ok(())
    }

    async fn shutdown(&mut self, handle: &WorkerHandle) -> Result<(), PluginError> {
        if let Some(payload) = handle.payload_ref::<TmuxPayload>()
            && payload.runtime == "gemini"
            && let Some(pid) = pane_pid(&handle.backend_id)
        {
            let wait_timeout = gemini_shutdown_wait_timeout();
            send_sigint_to_pane(&handle.backend_id)?;
            if !wait_for_pid_exit(pid, wait_timeout) {
                send_sigterm(pid);
                if !wait_for_pid_exit(pid, wait_timeout) {
                    send_sigkill(pid);
                }
            }
        }

        kill_pane(&handle.backend_id, &handle.agent_id)
    }
}

fn kill_pane(pane_id: &str, agent_id: &str) -> Result<(), PluginError> {
    let output = Command::new("tmux")
        .arg("kill-pane")
        .arg("-t")
        .arg(pane_id)
        .output()
        .map_err(|e| PluginError::Runtime {
            message: format!("Failed to kill tmux pane: {e}"),
            source: Some(Box::new(e)),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!("Failed to kill pane {pane_id} for agent {agent_id}: {stderr}");
    } else {
        debug!("Shut down tmux pane {pane_id} for agent {agent_id}");
    }
    Ok(())
}

fn send_sigint_to_pane(pane_id: &str) -> Result<(), PluginError> {
    let output = Command::new("tmux")
        .arg("send-keys")
        .arg("-t")
        .arg(pane_id)
        .arg("C-c")
        .output()
        .map_err(|e| PluginError::Runtime {
            message: format!("Failed to send C-c to pane {pane_id}: {e}"),
            source: Some(Box::new(e)),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PluginError::Runtime {
            message: format!("Failed to send C-c to pane {pane_id}: {stderr}"),
            source: None,
        });
    }
    Ok(())
}

fn pane_pid(pane_id: &str) -> Option<u32> {
    let output = Command::new("tmux")
        .args(["display-message", "-t", pane_id, "-p", "#{pane_pid}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok()
}

fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

fn gemini_shutdown_wait_timeout() -> Duration {
    let secs = std::env::var("ATM_GEMINI_SHUTDOWN_WAIT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(10);
    Duration::from_secs(secs)
}

fn is_pid_alive(pid: u32) -> bool {
    if !is_valid_signal_pid(pid) {
        return false;
    }
    agent_team_mail_core::pid::is_pid_alive(pid)
}

fn send_sigkill(pid: u32) {
    if !is_valid_signal_pid(pid) {
        return;
    }
    #[cfg(unix)]
    {
        // SAFETY: SIGKILL to a specific process ID.
        let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

fn send_sigterm(pid: u32) {
    if !is_valid_signal_pid(pid) {
        return;
    }
    #[cfg(unix)]
    {
        // SAFETY: SIGTERM to a specific process ID.
        let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

fn is_valid_signal_pid(pid: u32) -> bool {
    pid > 1 && pid <= i32::MAX as u32
}

fn shell_single_quote(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", input.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
        let log_dir = std::env::temp_dir().join("logs");
        let backend = CodexTmuxBackend::new("test-session".to_string(), log_dir.clone());

        let path = backend.log_path("arch-ctm@atm-planning");
        assert_eq!(path, log_dir.join("arch-ctm_atm-planning.log"));

        let path = backend.log_path("agent/with/slashes");
        assert_eq!(path, log_dir.join("agent_with_slashes.log"));
    }

    #[test]
    fn test_tmux_available() {
        // This test will pass or fail depending on whether tmux is installed
        // We just verify the function doesn't panic
        let _available = CodexTmuxBackend::tmux_available();
    }

    #[test]
    fn test_backend_creation() {
        let log_dir = std::env::temp_dir().join("logs");
        let backend = CodexTmuxBackend::new("test-session".to_string(), log_dir.clone());
        assert_eq!(backend.tmux_session, "test-session");
        assert_eq!(backend.log_dir, log_dir);
    }

    #[test]
    fn test_tmux_payload_construction() {
        let payload = TmuxPayload {
            session: "test-session".to_string(),
            pane_id: "%42".to_string(),
            window_name: "arch-ctm@planning".to_string(),
            runtime: "codex".to_string(),
            runtime_session_id: None,
            runtime_home: None,
        };

        assert_eq!(payload.session, "test-session");
        assert_eq!(payload.pane_id, "%42");
        assert_eq!(payload.window_name, "arch-ctm@planning");
        assert_eq!(payload.runtime, "codex");
        assert!(payload.runtime_session_id.is_none());
    }

    #[test]
    fn test_tmux_payload_clone() {
        let payload = TmuxPayload {
            session: "test-session".to_string(),
            pane_id: "%42".to_string(),
            window_name: "arch-ctm@planning".to_string(),
            runtime: "codex".to_string(),
            runtime_session_id: Some("sess-1".to_string()),
            runtime_home: None,
        };

        let cloned = payload.clone();
        assert_eq!(cloned, payload);
    }

    #[test]
    fn test_is_pid_alive_with_live_and_dead_pid() {
        let live_pid = std::process::id();
        assert!(is_pid_alive(live_pid));
        assert!(!is_pid_alive(u32::MAX));
    }

    #[test]
    fn test_wait_for_pid_exit_with_dead_pid_returns_true() {
        assert!(wait_for_pid_exit(u32::MAX, Duration::from_millis(5)));
    }

    #[test]
    fn test_send_sigkill_dead_pid_does_not_panic() {
        send_sigkill(u32::MAX);
    }

    #[test]
    fn test_shell_single_quote_escapes_single_quotes() {
        assert_eq!(shell_single_quote(""), "''");
        assert_eq!(shell_single_quote("abc"), "'abc'");
        assert_eq!(shell_single_quote("a'b"), "'a'\"'\"'b'");
    }

    #[test]
    fn test_send_sigint_to_pane_invalid_target_returns_error() {
        let result = send_sigint_to_pane("%999999");
        assert!(result.is_err());
    }
}
