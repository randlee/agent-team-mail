//! Worker lifecycle management — startup, health checks, crash recovery, shutdown

use super::config::WorkersConfig;
use super::trait_def::{WorkerAdapter, WorkerHandle};
use crate::plugin::PluginError;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

/// Maximum log file size before rotation (10 MB)
const MAX_LOG_SIZE: u64 = 10 * 1024 * 1024;

/// Worker state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    /// Worker is running normally
    Running,
    /// Worker has crashed
    Crashed,
    /// Worker is being restarted
    Restarting,
    /// Worker is idle (no active requests)
    Idle,
}

impl std::fmt::Display for WorkerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Crashed => write!(f, "crashed"),
            Self::Restarting => write!(f, "restarting"),
            Self::Idle => write!(f, "idle"),
        }
    }
}

/// Worker metadata for lifecycle tracking
#[derive(Debug, Clone)]
pub struct WorkerMetadata {
    /// Current state
    pub state: WorkerState,
    /// Number of restart attempts
    pub restart_count: u32,
    /// Last health check time
    pub last_health_check: Instant,
    /// Worker spawn time
    pub spawn_time: Instant,
}

impl Default for WorkerMetadata {
    fn default() -> Self {
        Self {
            state: WorkerState::Idle,
            restart_count: 0,
            last_health_check: Instant::now(),
            spawn_time: Instant::now(),
        }
    }
}

/// Lifecycle manager for worker processes
pub struct LifecycleManager {
    /// Worker metadata indexed by agent ID
    metadata: HashMap<String, WorkerMetadata>,
    /// Health check interval in seconds
    health_check_interval: u64,
    /// Maximum restart attempts before giving up
    max_restart_attempts: u32,
    /// Backoff duration between restart attempts (seconds)
    restart_backoff_secs: u64,
}

impl LifecycleManager {
    /// Create a new lifecycle manager with default settings
    pub fn new() -> Self {
        Self {
            metadata: HashMap::new(),
            health_check_interval: 30, // 30 seconds
            max_restart_attempts: 3,
            restart_backoff_secs: 5,
        }
    }

    /// Create a lifecycle manager from config
    pub fn from_config(config: &WorkersConfig) -> Self {
        // Extract lifecycle settings from config
        Self {
            metadata: HashMap::new(),
            health_check_interval: config.health_check_interval_secs,
            max_restart_attempts: config.max_restart_attempts,
            restart_backoff_secs: config.restart_backoff_secs,
        }
    }

    /// Register a newly spawned worker
    pub fn register_worker(&mut self, agent_id: &str) {
        let metadata = WorkerMetadata {
            state: WorkerState::Running,
            restart_count: 0,
            last_health_check: Instant::now(),
            spawn_time: Instant::now(),
        };
        self.metadata.insert(agent_id.to_string(), metadata);
        debug!("Registered worker for agent {agent_id}");
    }

    /// Get worker state
    pub fn get_state(&self, agent_id: &str) -> Option<WorkerState> {
        self.metadata.get(agent_id).map(|m| m.state)
    }

    /// Set worker state
    pub fn set_state(&mut self, agent_id: &str, state: WorkerState) {
        if let Some(metadata) = self.metadata.get_mut(agent_id) {
            metadata.state = state;
            debug!("Worker {agent_id} state changed to {state}");
        }
    }

    /// Check if a worker needs a health check
    pub fn needs_health_check(&self, agent_id: &str) -> bool {
        if let Some(metadata) = self.metadata.get(agent_id) {
            let elapsed = metadata.last_health_check.elapsed();
            elapsed.as_secs() >= self.health_check_interval
        } else {
            false
        }
    }

    /// Update last health check time
    pub fn update_health_check(&mut self, agent_id: &str) {
        if let Some(metadata) = self.metadata.get_mut(agent_id) {
            metadata.last_health_check = Instant::now();
        }
    }

    /// Check if worker can be restarted
    pub fn can_restart(&self, agent_id: &str) -> bool {
        if let Some(metadata) = self.metadata.get(agent_id) {
            metadata.restart_count < self.max_restart_attempts
        } else {
            true
        }
    }

    /// Increment restart counter
    pub fn increment_restart_count(&mut self, agent_id: &str) {
        if let Some(metadata) = self.metadata.get_mut(agent_id) {
            metadata.restart_count += 1;
            debug!(
                "Worker {agent_id} restart count: {}/{}",
                metadata.restart_count, self.max_restart_attempts
            );
        }
    }

    /// Reset restart counter (after successful recovery)
    pub fn reset_restart_count(&mut self, agent_id: &str) {
        if let Some(metadata) = self.metadata.get_mut(agent_id) {
            metadata.restart_count = 0;
            debug!("Worker {agent_id} restart count reset");
        }
    }

    /// Get backoff duration for restart
    pub fn get_backoff_duration(&self, agent_id: &str) -> Duration {
        if let Some(metadata) = self.metadata.get(agent_id) {
            // Exponential backoff: 5s, 10s, 20s, ...
            let multiplier = 2_u64.pow(metadata.restart_count);
            Duration::from_secs(self.restart_backoff_secs * multiplier)
        } else {
            Duration::from_secs(self.restart_backoff_secs)
        }
    }

    /// Remove worker from tracking
    pub fn unregister_worker(&mut self, agent_id: &str) {
        self.metadata.remove(agent_id);
        debug!("Unregistered worker for agent {agent_id}");
    }

    /// Get all worker states for status reporting
    pub fn get_all_states(&self) -> HashMap<String, WorkerState> {
        self.metadata
            .iter()
            .map(|(id, meta)| (id.clone(), meta.state))
            .collect()
    }
}

impl Default for LifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Auto-start configured workers on daemon init
///
/// # Arguments
///
/// * `backend` - Worker backend to use for spawning
/// * `config` - Worker configuration
/// * `lifecycle` - Lifecycle manager to track workers
/// * `workers` - Map to store worker handles
///
/// # Errors
///
/// Returns error if any worker fails to spawn
pub async fn auto_start_workers(
    backend: &mut dyn WorkerAdapter,
    config: &WorkersConfig,
    lifecycle: &mut LifecycleManager,
    workers: &mut HashMap<String, WorkerHandle>,
) -> Result<(), PluginError> {
    info!("Auto-starting configured workers");

    for (agent_id, agent_config) in &config.agents {
        if !agent_config.enabled {
            debug!("Skipping disabled agent {agent_id}");
            continue;
        }

        info!("Starting worker for agent {agent_id}");
        match backend.spawn(agent_id, "{}").await {
            Ok(handle) => {
                lifecycle.register_worker(agent_id);
                workers.insert(agent_id.clone(), handle);
                info!("Worker {agent_id} started successfully");
            }
            Err(e) => {
                error!("Failed to start worker {agent_id}: {e}");
                // Continue with other workers even if one fails
            }
        }
    }

    info!("Auto-start complete: {} workers running", workers.len());
    Ok(())
}

/// Check health of a worker by verifying tmux pane exists
///
/// # Arguments
///
/// * `handle` - Worker handle to check
///
/// # Returns
///
/// `true` if worker is healthy, `false` if crashed
pub async fn check_worker_health(handle: &WorkerHandle) -> bool {
    // Check if tmux pane still exists using `tmux has-session`
    let output = std::process::Command::new("tmux")
        .arg("list-panes")
        .arg("-a")
        .arg("-F")
        .arg("#{pane_id}")
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let panes = String::from_utf8_lossy(&output.stdout);
            panes.contains(&handle.tmux_pane_id)
        }
        Ok(output) => {
            warn!(
                "tmux list-panes returned non-zero status for {}: {}",
                handle.agent_id,
                String::from_utf8_lossy(&output.stderr)
            );
            false
        }
        Err(e) => {
            error!("Failed to check tmux pane for {}: {e}", handle.agent_id);
            false
        }
    }
}

/// Restart a crashed worker with backoff
///
/// # Arguments
///
/// * `agent_id` - Agent ID to restart
/// * `backend` - Worker backend
/// * `lifecycle` - Lifecycle manager
/// * `workers` - Workers map
///
/// # Errors
///
/// Returns error if restart fails
pub async fn restart_worker(
    agent_id: &str,
    backend: &mut dyn WorkerAdapter,
    lifecycle: &mut LifecycleManager,
    workers: &mut HashMap<String, WorkerHandle>,
) -> Result<(), PluginError> {
    if !lifecycle.can_restart(agent_id) {
        error!(
            "Worker {agent_id} exceeded max restart attempts, giving up"
        );
        lifecycle.set_state(agent_id, WorkerState::Crashed);
        return Err(PluginError::Runtime {
            message: format!("Worker {agent_id} exceeded max restart attempts"),
            source: None,
        });
    }

    lifecycle.set_state(agent_id, WorkerState::Restarting);
    lifecycle.increment_restart_count(agent_id);

    // Apply backoff before restart
    let backoff = lifecycle.get_backoff_duration(agent_id);
    warn!(
        "Worker {agent_id} crashed, restarting after {}s backoff",
        backoff.as_secs()
    );
    sleep(backoff).await;

    // Remove old handle if exists
    workers.remove(agent_id);

    // Spawn new worker
    match backend.spawn(agent_id, "{}").await {
        Ok(handle) => {
            workers.insert(agent_id.to_string(), handle);
            lifecycle.set_state(agent_id, WorkerState::Running);
            lifecycle.update_health_check(agent_id);
            info!("Worker {agent_id} restarted successfully");
            Ok(())
        }
        Err(e) => {
            error!("Failed to restart worker {agent_id}: {e}");
            lifecycle.set_state(agent_id, WorkerState::Crashed);
            Err(e)
        }
    }
}

/// Rotate log file if it exceeds size limit
///
/// # Arguments
///
/// * `log_path` - Path to the log file
///
/// # Errors
///
/// Returns error if rotation fails
pub fn rotate_log_if_needed(log_path: &PathBuf) -> Result<(), PluginError> {
    if !log_path.exists() {
        return Ok(());
    }

    match std::fs::metadata(log_path) {
        Ok(metadata) if metadata.len() > MAX_LOG_SIZE => {
            // Rotate by renaming to .log.old
            let old_path = log_path.with_extension("log.old");
            debug!(
                "Rotating log file {} ({} bytes) to {}",
                log_path.display(),
                metadata.len(),
                old_path.display()
            );

            if let Err(e) = std::fs::rename(log_path, &old_path) {
                warn!("Failed to rotate log file: {e}");
                return Err(PluginError::Runtime {
                    message: format!("Failed to rotate log file: {e}"),
                    source: Some(Box::new(e)),
                });
            }

            // Create new empty log file
            if let Err(e) = std::fs::File::create(log_path) {
                warn!("Failed to create new log file: {e}");
                return Err(PluginError::Runtime {
                    message: format!("Failed to create new log file: {e}"),
                    source: Some(Box::new(e)),
                });
            }

            info!("Log file rotated: {}", log_path.display());
        }
        Ok(_) => {
            // Size is OK, no rotation needed
        }
        Err(e) => {
            warn!("Failed to check log file size: {e}");
        }
    }

    Ok(())
}

/// Gracefully shutdown a worker with timeout
///
/// Sends exit command, waits for clean exit, falls back to kill-pane
///
/// # Arguments
///
/// * `agent_id` - Agent ID to shutdown
/// * `backend` - Worker backend
/// * `handle` - Worker handle
/// * `timeout_secs` - Timeout in seconds for graceful shutdown
///
/// # Errors
///
/// Returns error if shutdown fails
pub async fn graceful_shutdown(
    agent_id: &str,
    backend: &mut dyn WorkerAdapter,
    handle: &WorkerHandle,
    timeout_secs: u64,
) -> Result<(), PluginError> {
    debug!("Attempting graceful shutdown of worker {agent_id}");

    // Send exit command (backend-specific)
    // For Codex, this would be "exit" or Ctrl-D
    // For now, use backend's shutdown which does kill-pane
    // In the future, we could add a `send_exit_command` to WorkerAdapter trait

    // Wait for graceful exit with timeout
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    while start.elapsed() < timeout {
        // Check if worker is still alive
        if !check_worker_health(handle).await {
            debug!("Worker {agent_id} exited gracefully");
            return Ok(());
        }
        sleep(Duration::from_millis(500)).await;
    }

    // Timeout reached, force kill
    warn!("Worker {agent_id} did not exit gracefully, forcing kill");
    backend.shutdown(handle).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lifecycle_manager_new() {
        let manager = LifecycleManager::new();
        assert_eq!(manager.health_check_interval, 30);
        assert_eq!(manager.max_restart_attempts, 3);
        assert_eq!(manager.restart_backoff_secs, 5);
    }

    #[test]
    fn test_register_worker() {
        let mut manager = LifecycleManager::new();
        manager.register_worker("test-agent");

        assert_eq!(
            manager.get_state("test-agent"),
            Some(WorkerState::Running)
        );
    }

    #[test]
    fn test_set_state() {
        let mut manager = LifecycleManager::new();
        manager.register_worker("test-agent");

        manager.set_state("test-agent", WorkerState::Crashed);
        assert_eq!(manager.get_state("test-agent"), Some(WorkerState::Crashed));
    }

    #[test]
    fn test_restart_count() {
        let mut manager = LifecycleManager::new();
        manager.register_worker("test-agent");

        assert!(manager.can_restart("test-agent"));

        manager.increment_restart_count("test-agent");
        assert!(manager.can_restart("test-agent"));

        manager.increment_restart_count("test-agent");
        assert!(manager.can_restart("test-agent"));

        manager.increment_restart_count("test-agent");
        assert!(!manager.can_restart("test-agent")); // Max attempts reached
    }

    #[test]
    fn test_backoff_duration() {
        let mut manager = LifecycleManager::new();
        manager.register_worker("test-agent");

        // First attempt: 5s
        assert_eq!(manager.get_backoff_duration("test-agent").as_secs(), 5);

        // Second attempt: 10s
        manager.increment_restart_count("test-agent");
        assert_eq!(manager.get_backoff_duration("test-agent").as_secs(), 10);

        // Third attempt: 20s
        manager.increment_restart_count("test-agent");
        assert_eq!(manager.get_backoff_duration("test-agent").as_secs(), 20);
    }

    #[test]
    fn test_reset_restart_count() {
        let mut manager = LifecycleManager::new();
        manager.register_worker("test-agent");

        manager.increment_restart_count("test-agent");
        manager.increment_restart_count("test-agent");
        assert_eq!(manager.get_backoff_duration("test-agent").as_secs(), 20);

        manager.reset_restart_count("test-agent");
        assert_eq!(manager.get_backoff_duration("test-agent").as_secs(), 5);
    }

    #[test]
    fn test_unregister_worker() {
        let mut manager = LifecycleManager::new();
        manager.register_worker("test-agent");

        assert!(manager.get_state("test-agent").is_some());

        manager.unregister_worker("test-agent");
        assert!(manager.get_state("test-agent").is_none());
    }

    #[test]
    fn test_get_all_states() {
        let mut manager = LifecycleManager::new();
        manager.register_worker("agent1");
        manager.register_worker("agent2");
        manager.set_state("agent2", WorkerState::Crashed);

        let states = manager.get_all_states();
        assert_eq!(states.len(), 2);
        assert_eq!(states.get("agent1"), Some(&WorkerState::Running));
        assert_eq!(states.get("agent2"), Some(&WorkerState::Crashed));
    }

    #[test]
    fn test_worker_state_display() {
        assert_eq!(WorkerState::Running.to_string(), "running");
        assert_eq!(WorkerState::Crashed.to_string(), "crashed");
        assert_eq!(WorkerState::Restarting.to_string(), "restarting");
        assert_eq!(WorkerState::Idle.to_string(), "idle");
    }
}
