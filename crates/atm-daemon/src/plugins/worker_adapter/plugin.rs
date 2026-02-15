//! Worker Adapter plugin implementation

use super::activity::ActivityTracker;
use super::capture::LogTailer;
use super::codex_tmux::CodexTmuxBackend;
use super::config::WorkersConfig;
use super::lifecycle::{self, LifecycleManager, WorkerState};
use super::router::{ConcurrencyPolicy, MessageRouter};
use super::trait_def::{WorkerAdapter, WorkerHandle};
use crate::plugin::{Capability, Plugin, PluginContext, PluginError, PluginMetadata};
use atm_core::io::inbox::inbox_append;
use atm_core::schema::InboxMessage;
use chrono::Utc;
use std::collections::HashMap;
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};
use uuid::Uuid;

/// Worker Adapter plugin — manages async agent teammates in tmux panes
pub struct WorkerAdapterPlugin {
    /// Plugin configuration from [workers]
    config: WorkersConfig,
    /// The worker backend (Codex TMUX, SSH, Docker, etc.)
    backend: Option<Box<dyn WorkerAdapter>>,
    /// Active worker handles
    workers: HashMap<String, WorkerHandle>,
    /// Message router with concurrency control
    router: MessageRouter,
    /// Activity tracker for agent heartbeats
    activity_tracker: ActivityTracker,
    /// Log tailer for response capture
    log_tailer: LogTailer,
    /// Lifecycle manager for worker health and restart
    lifecycle: LifecycleManager,
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
            router: MessageRouter::new(),
            activity_tracker: ActivityTracker::default(),
            log_tailer: LogTailer::new(),
            lifecycle: LifecycleManager::new(),
            ctx: None,
        }
    }

    /// Get worker status for all configured agents
    #[allow(dead_code)]
    pub fn get_worker_states(&self) -> HashMap<String, WorkerState> {
        self.lifecycle.get_all_states()
    }

    /// Format a message using the agent's prompt template
    ///
    /// # Arguments
    ///
    /// * `message` - Inbox message to format
    /// * `config_key` - Config key for the agent
    #[allow(dead_code)]
    fn format_message(&self, message: &InboxMessage, config_key: &str) -> String {
        let template = self
            .config
            .agents
            .get(config_key)
            .map(|cfg| cfg.prompt_template.as_str())
            .unwrap_or("{message}");

        template.replace("{message}", &message.text)
    }

    /// Process a message for a worker agent
    ///
    /// Routes message through concurrency control, formats it, sends to worker,
    /// captures response, and writes response back to sender inbox.
    ///
    /// # Arguments
    ///
    /// * `config_key` - Config key for the agent
    /// * `message` - Inbox message to process
    #[allow(dead_code)]
    async fn process_message(
        &mut self,
        config_key: &str,
        message: InboxMessage,
    ) -> Result<(), PluginError> {
        // Check if agent is configured and enabled
        let agent_config = self.config.agents.get(config_key).ok_or_else(|| {
            PluginError::Runtime {
                message: format!("Agent config not found for {config_key}"),
                source: None,
            }
        })?;

        if !agent_config.enabled {
            debug!("Agent {config_key} is not enabled for worker adapter");
            return Ok(());
        }

        // Clone member_name to avoid borrow issues
        let member_name = agent_config.member_name.clone();

        // Route through concurrency control (using member_name as runtime identity)
        let routable = self.router.route_message(&member_name, message.clone())?;
        if routable.is_none() {
            // Message was queued or rejected
            return Ok(());
        }

        let message = routable.unwrap();

        // Ensure worker is spawned (keyed by member_name)
        if !self.workers.contains_key(&member_name) {
            debug!("Spawning worker for agent {config_key} (member: {member_name})");
            self.spawn_worker(config_key).await?;
        }

        let worker_handle = self
            .workers
            .get(&member_name)
            .ok_or_else(|| PluginError::Runtime {
                message: format!("Worker handle not found for member {member_name}"),
                source: None,
            })?
            .clone();

        // Format message with template (using config_key)
        let formatted_prompt = self.format_message(&message, config_key);

        // Send message to worker
        let backend = self.backend.as_mut().ok_or_else(|| PluginError::Runtime {
            message: "Worker backend not initialized".to_string(),
            source: None,
        })?;

        backend
            .send_message(&worker_handle, &formatted_prompt)
            .await?;

        debug!("Sent message to worker {member_name}");

        // Capture response from log file
        let captured = match self
            .log_tailer
            .capture_response(&worker_handle.log_file_path, &formatted_prompt)
        {
            Ok(captured) => captured,
            Err(e) => {
                error!("Failed to capture response from {member_name}: {e}");
                self.router.agent_finished(&member_name);
                return Err(e);
            }
        };

        debug!("Captured response from {member_name}: {} bytes", captured.response_text.len());

        // Build response message (use member_name as sender)
        let response = InboxMessage {
            from: member_name.clone(),
            text: captured.response_text,
            timestamp: Utc::now().to_rfc3339(),
            read: false,
            summary: Some(format!("Response from {member_name}")),
            message_id: Some(Uuid::new_v4().to_string()),
            unknown_fields: if let Some(request_id) = message.unknown_fields.get("requestId") {
                // Correlate with Request-ID if present
                let mut fields = HashMap::new();
                fields.insert("requestId".to_string(), request_id.clone());
                fields
            } else {
                HashMap::new()
            },
        };

        // Write response to sender's inbox
        let sender_name = &message.from;

        let ctx = self.ctx.as_ref().ok_or_else(|| PluginError::Runtime {
            message: "Plugin context not initialized".to_string(),
            source: None,
        })?;

        let team_name = &ctx.system.default_team;
        let home_dir = &ctx.system.claude_root;
        let sender_inbox_path = home_dir
            .join("teams")
            .join(team_name)
            .join("inboxes")
            .join(format!("{sender_name}.json"));

        if let Err(e) = inbox_append(&sender_inbox_path, &response, team_name, sender_name) {
            error!("Failed to write response to {sender_name} inbox: {e}");
        } else {
            debug!("Wrote response to {sender_name} inbox");
        }

        // Mark agent as finished processing (using member_name)
        self.router.agent_finished(&member_name);

        // Check for queued messages and process next one
        if let Some(next_message) = self.router.agent_finished(&member_name) {
            debug!("Processing next queued message for {member_name}");
            Box::pin(self.process_message(config_key, next_message)).await?;
        }

        Ok(())
    }

    /// Spawn a worker for the given agent
    ///
    /// # Arguments
    ///
    /// * `config_key` - Config key for the agent
    #[allow(dead_code)]
    async fn spawn_worker(&mut self, config_key: &str) -> Result<(), PluginError> {
        let backend = self.backend.as_mut().ok_or_else(|| PluginError::Runtime {
            message: "Worker backend not initialized".to_string(),
            source: None,
        })?;

        let agent_config = self.config.agents.get(config_key).ok_or_else(|| {
            PluginError::Runtime {
                message: format!("Agent config not found for {config_key}"),
                source: None,
            }
        })?;

        let member_name = &agent_config.member_name;
        let command = self.config.resolve_command(config_key);

        let handle = backend.spawn(member_name, command).await?;
        self.lifecycle.register_worker(member_name);
        self.workers.insert(member_name.to_string(), handle);
        debug!("Spawned worker for agent {config_key} (member: {member_name})");

        Ok(())
    }

    /// Perform health check on all workers
    async fn health_check_all_workers(&mut self) -> Result<(), PluginError> {
        let member_names: Vec<String> = self.workers.keys().cloned().collect();

        for member_name in member_names {
            // Skip if health check not needed yet
            if !self.lifecycle.needs_health_check(&member_name) {
                continue;
            }

            if let Some(handle) = self.workers.get(&member_name) {
                let is_healthy = lifecycle::check_worker_health(handle).await;
                self.lifecycle.update_health_check(&member_name);

                if !is_healthy {
                    error!("Worker {member_name} health check failed, initiating restart");
                    self.lifecycle.set_state(&member_name, WorkerState::Crashed);

                    // Attempt restart
                    if let Some(backend) = self.backend.as_mut()
                        && let Err(e) = lifecycle::restart_worker(
                            &member_name,
                            backend.as_mut(),
                            &self.config,
                            &mut self.lifecycle,
                            &mut self.workers,
                        )
                        .await
                    {
                        error!("Failed to restart worker {member_name}: {e}");
                    }
                }
            }
        }

        Ok(())
    }

    /// Rotate log files for all workers if needed
    fn rotate_logs_if_needed(&self) {
        for handle in self.workers.values() {
            if let Err(e) = lifecycle::rotate_log_if_needed(&handle.log_file_path) {
                error!("Failed to rotate log for {}: {e}", handle.agent_id);
            }
        }
    }

    /// Check for inactive agents and mark them as offline
    async fn check_inactivity(&self) -> Result<(), PluginError> {
        let ctx = self.ctx.as_ref().ok_or_else(|| PluginError::Runtime {
            message: "Plugin context not initialized".to_string(),
            source: None,
        })?;

        let team_name = &ctx.system.default_team;
        let home_dir = &ctx.system.claude_root;
        let team_config_path = home_dir
            .join("teams")
            .join(team_name)
            .join("config.json");

        if team_config_path.exists() {
            self.activity_tracker
                .check_inactivity(&team_config_path)?;
        }

        Ok(())
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
            version: "0.2.0",
            description: "Manages async agent teammates in isolated tmux panes with message routing",
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

        // Initialize activity tracker with configured timeout
        self.activity_tracker = ActivityTracker::new(self.config.inactivity_timeout_ms);

        // Initialize lifecycle manager with config
        self.lifecycle = LifecycleManager::from_config(&self.config);

        // Configure router policies for each agent (using member_name as runtime identity)
        for (config_key, agent_config) in &self.config.agents {
            let policy = match agent_config.concurrency_policy.as_str() {
                "reject" => ConcurrencyPolicy::Reject,
                "concurrent" => ConcurrencyPolicy::Concurrent,
                _ => ConcurrencyPolicy::Queue, // default
            };
            let member_name = &agent_config.member_name;
            self.router.set_policy(member_name.clone(), policy);
            debug!("Set concurrency policy for {config_key} (member: {member_name}): {policy:?}");
        }

        // Store context for runtime use
        self.ctx = Some(ctx.clone());

        debug!("Worker Adapter plugin initialized with {} backend", self.config.backend);
        debug!("Configured {} agents", self.config.agents.len());

        // Auto-start configured workers
        if let Some(backend) = self.backend.as_mut() {
            info!("Auto-starting configured workers on daemon init");
            lifecycle::auto_start_workers(
                backend.as_mut(),
                &self.config,
                &mut self.lifecycle,
                &mut self.workers,
            )
            .await?;
        }

        Ok(())
    }

    async fn run(&mut self, cancel: CancellationToken) -> Result<(), PluginError> {
        // If disabled or no backend, just wait for cancellation
        if !self.config.enabled || self.backend.is_none() {
            cancel.cancelled().await;
            return Ok(());
        }

        debug!("Worker Adapter plugin running with lifecycle management enabled");

        // Set up periodic inactivity check (every 30 seconds)
        let mut inactivity_timer = interval(Duration::from_secs(30));

        // Set up periodic health check (configurable, default 30 seconds)
        let health_check_interval = Duration::from_secs(self.config.health_check_interval_secs);
        let mut health_check_timer = interval(health_check_interval);

        // Set up periodic log rotation check (every 5 minutes)
        let mut log_rotation_timer = interval(Duration::from_secs(300));

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("Worker Adapter plugin shutting down");
                    break;
                }
                _ = inactivity_timer.tick() => {
                    if let Err(e) = self.check_inactivity().await {
                        error!("Failed to check agent inactivity: {e}");
                    }
                }
                _ = health_check_timer.tick() => {
                    if let Err(e) = self.health_check_all_workers().await {
                        error!("Failed to perform health checks: {e}");
                    }
                }
                _ = log_rotation_timer.tick() => {
                    self.rotate_logs_if_needed();
                }
            }
        }

        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), PluginError> {
        // Shut down all active workers gracefully
        if let Some(backend) = &mut self.backend {
            info!("Shutting down {} workers", self.workers.len());

            for (member_name, handle) in self.workers.drain() {
                debug!("Shutting down worker for member {}", member_name);

                // Use graceful shutdown with timeout
                let timeout_secs = self.config.shutdown_timeout_secs;
                if let Err(e) =
                    lifecycle::graceful_shutdown(&member_name, backend.as_mut(), &handle, timeout_secs)
                        .await
                {
                    error!("Failed to shut down worker for {member_name}: {e}");
                }

                // Unregister from lifecycle manager
                self.lifecycle.unregister_worker(&member_name);
            }

            info!("All workers shut down");
        }

        Ok(())
    }

    async fn handle_message(&mut self, msg: &InboxMessage) -> Result<(), PluginError> {
        // Sprint 7.2: Implement message routing
        // This is called when a new inbox message is detected by the daemon

        // For now, we need to determine the target agent from context
        // The daemon should provide this information, but we'll use a placeholder
        // In a real implementation, the daemon would pass the target agent name

        // TODO: Get target agent from daemon context
        // For now, skip message handling (will be implemented when daemon provides routing info)

        debug!("Received message from {} (routing not yet implemented)", msg.from);
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
        assert_eq!(metadata.version, "0.2.0");
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

    #[test]
    fn test_format_message_default_template() {
        let plugin = WorkerAdapterPlugin::new();
        let msg = InboxMessage {
            from: "sender".to_string(),
            text: "Hello, agent!".to_string(),
            timestamp: "2026-02-14T00:00:00Z".to_string(),
            read: false,
            summary: None,
            message_id: None,
            unknown_fields: HashMap::new(),
        };

        let formatted = plugin.format_message(&msg, "test-agent");
        assert_eq!(formatted, "Hello, agent!");
    }
}
