//! Worker Adapter plugin implementation

use super::activity::ActivityTracker;
use super::agent_state::{AgentState, AgentStateTracker};
use super::capture::LogTailer;
use super::codex_tmux::CodexTmuxBackend;
use super::config::WorkersConfig;
use super::hook_watcher::HookWatcher;
use super::lifecycle::{self, LifecycleManager, WorkerState};
use super::nudge::NudgeEngine;
use super::pubsub::PubSub;
use super::router::{ConcurrencyPolicy, MessageRouter};
use super::trait_def::{WorkerAdapter, WorkerHandle};
use crate::plugin::{Capability, Plugin, PluginContext, PluginError, PluginMetadata};
use agent_team_mail_core::io::inbox::inbox_append;
use agent_team_mail_core::schema::InboxMessage;
use chrono::Utc;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Interval for PID-based process health polling (5 seconds per acceptance criteria).
const PID_POLL_INTERVAL_SECS: u64 = 5;

/// Interval for PubSub GC (60 seconds).
const PUBSUB_GC_INTERVAL_SECS: u64 = 60;

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
    /// Turn-level agent state tracker (Launching/Busy/Idle/Killed)
    agent_state: Arc<Mutex<AgentStateTracker>>,
    /// Ephemeral pub/sub registry for agent state change notifications
    pubsub: Arc<Mutex<PubSub>>,
    /// Nudge engine — auto-nudges idle agents with unread messages
    nudge_engine: NudgeEngine,
    /// Snapshot of last-notified agent states for change detection
    last_notified_states: HashMap<String, AgentState>,
    /// Cached context for runtime use
    ctx: Option<PluginContext>,
}

impl WorkerAdapterPlugin {
    /// Create a new Worker Adapter plugin instance with a fresh state store.
    pub fn new() -> Self {
        Self::with_state_store(Arc::new(Mutex::new(AgentStateTracker::new())))
    }

    /// Create a new Worker Adapter plugin instance that shares the given state
    /// store.
    ///
    /// Use this when the socket server or another component needs to read
    /// live agent state from the same tracker that the plugin populates:
    ///
    /// ```rust,ignore
    /// let store = new_state_store();
    /// let plugin = WorkerAdapterPlugin::with_state_store(Arc::clone(&store));
    /// // pass `store` to the socket server
    /// ```
    pub fn with_state_store(
        state_store: std::sync::Arc<std::sync::Mutex<AgentStateTracker>>,
    ) -> Self {
        let config = WorkersConfig::default();
        let nudge_engine = NudgeEngine::new(config.nudge.clone());
        Self {
            config,
            backend: None,
            workers: HashMap::new(),
            router: MessageRouter::new(),
            activity_tracker: ActivityTracker::default(),
            log_tailer: LogTailer::new(),
            lifecycle: LifecycleManager::new(),
            agent_state: state_store,
            pubsub: Arc::new(Mutex::new(PubSub::new())),
            nudge_engine,
            last_notified_states: HashMap::new(),
            ctx: None,
        }
    }

    /// Return a clone of the shared agent state store.
    ///
    /// The returned `Arc` points to the same tracker that the `HookWatcher`
    /// populates, so the socket server can read live state without a fresh
    /// empty store.
    pub fn state_store(
        &self,
    ) -> std::sync::Arc<std::sync::Mutex<AgentStateTracker>> {
        Arc::clone(&self.agent_state)
    }

    /// Return a clone of the shared pub/sub registry.
    ///
    /// Pass this `Arc` to the socket server so that `subscribe` and
    /// `unsubscribe` requests from the CLI are stored in the same registry
    /// that the plugin uses for notification delivery.
    pub fn pubsub_store(&self) -> Arc<Mutex<PubSub>> {
        Arc::clone(&self.pubsub)
    }

    /// Get worker status for all configured agents
    #[allow(dead_code)]
    pub fn get_worker_states(&self) -> HashMap<String, WorkerState> {
        self.lifecycle.get_all_states()
    }

    /// Get turn-level agent states
    #[allow(dead_code)]
    pub fn get_agent_states(&self) -> HashMap<String, AgentState> {
        self.agent_state.lock().unwrap().all_states()
    }

    /// Build the path to the hook events file.
    ///
    /// Path: `${ATM_HOME}/.claude/daemon/hooks/events.jsonl`
    fn hook_events_path(ctx: &PluginContext) -> PathBuf {
        ctx.system
            .claude_root
            .join("daemon")
            .join("hooks")
            .join("events.jsonl")
    }

    fn resolve_team_name<'a>(
        &'a self,
        ctx: &'a PluginContext,
        msg: Option<&'a InboxMessage>,
    ) -> &'a str {
        if let Some(msg) = msg
            && let Some(team) = msg.unknown_fields.get("team").and_then(|v| v.as_str())
        {
            return team;
        }

        if self.config.team_name.is_empty() {
            &ctx.system.default_team
        } else {
            &self.config.team_name
        }
    }

    fn team_config_path(&self, ctx: &PluginContext, team_name: &str) -> std::path::PathBuf {
        ctx.system
            .claude_root
            .join("teams")
            .join(team_name)
            .join("config.json")
    }

    fn record_activity(&self, ctx: &PluginContext, team_name: &str, member_name: &str) {
        let team_config_path = self.team_config_path(ctx, team_name);
        if team_config_path.exists()
            && let Err(e) = self
                .activity_tracker
                .record_activity(&team_config_path, member_name)
        {
            warn!("Failed to record activity for {member_name}: {e}");
        }
    }

    fn notify_routing_issue(
        &self,
        ctx: &PluginContext,
        team_name: &str,
        sender_name: &str,
        details: &str,
    ) {
        let warning_text = format!(
            "Warning: worker_adapter could not route your message. {details}\n\nAction: Please specify a valid recipient (agent member_name)."
        );

        let warn_msg = InboxMessage {
            from: "worker-adapter".to_string(),
            text: warning_text,
            timestamp: Utc::now().to_rfc3339(),
            read: false,
            summary: Some("Worker adapter routing warning".to_string()),
            message_id: Some(Uuid::new_v4().to_string()),
            unknown_fields: HashMap::new(),
        };

        let team_root = ctx.system.claude_root.join("teams").join(team_name);
        let sender_inbox = team_root.join("inboxes").join(format!("{sender_name}.json"));
        if let Err(e) = inbox_append(&sender_inbox, &warn_msg, team_name, sender_name) {
            error!("Failed to warn sender {sender_name}: {e}");
        }

        if sender_name != "team-lead" {
            let lead_inbox = team_root.join("inboxes").join("team-lead.json");
            if let Err(e) = inbox_append(&lead_inbox, &warn_msg, team_name, "team-lead") {
                error!("Failed to warn team-lead: {e}");
            }
        }
    }

    /// Build the inbox path for an agent member by name.
    ///
    /// Path: `{claude_root}/teams/{team_name}/inboxes/{member_name}.json`
    fn agent_inbox_path(&self, ctx: &PluginContext, team_name: &str, member_name: &str) -> PathBuf {
        ctx.system
            .claude_root
            .join("teams")
            .join(team_name)
            .join("inboxes")
            .join(format!("{member_name}.json"))
    }

    /// Trigger a nudge for `member_name` if it is currently `Idle` and has
    /// unread messages. Called after a `Busy → Idle` state transition.
    ///
    /// This is a best-effort operation; errors are logged but not propagated.
    async fn trigger_nudge_if_idle(&mut self, member_name: &str) {
        // Only nudge if config is enabled
        if !self.config.nudge.enabled {
            return;
        }

        // Verify the agent is actually Idle right now
        let current_state = {
            self.agent_state
                .lock()
                .unwrap()
                .get_state(member_name)
        };

        let Some(AgentState::Idle) = current_state else {
            return;
        };

        // Resolve pane ID from the worker handle
        let pane_id = match self.workers.get(member_name) {
            Some(h) => h.backend_id.clone(),
            None => {
                debug!("Nudge: no worker handle for {member_name}, skipping");
                return;
            }
        };

        let ctx = match &self.ctx {
            Some(c) => c.clone(),
            None => return,
        };

        let team_name = if self.config.team_name.is_empty() {
            ctx.system.default_team.clone()
        } else {
            self.config.team_name.clone()
        };

        let inbox_path = self.agent_inbox_path(&ctx, &team_name, member_name);

        if let Err(e) = self
            .nudge_engine
            .on_idle_transition(member_name, &pane_id, &inbox_path)
            .await
        {
            warn!("Nudge engine error for {member_name}: {e}");
        }
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

        // Mark agent as Busy now that we've sent a message
        {
            let mut state = self.agent_state.lock().unwrap();
            state.set_state(&member_name, AgentState::Busy);
        }

        // Record activity after successful message send
        let ctx = self.ctx.as_ref().ok_or_else(|| PluginError::Runtime {
            message: "Plugin context not initialized".to_string(),
            source: None,
        })?;
        let team_name = self.resolve_team_name(ctx, Some(&message));
        self.record_activity(ctx, team_name, &member_name);

        // Capture response from log file (uses blocking sleep, so wrap in spawn_blocking)
        let log_path = worker_handle.log_file_path.clone();
        let prompt_for_capture = formatted_prompt.clone();
        let log_tailer = self.log_tailer.clone();

        let captured = match tokio::task::spawn_blocking(move || {
            log_tailer.capture_response(&log_path, &prompt_for_capture)
        })
        .await
        {
            Ok(Ok(captured)) => captured,
            Ok(Err(e)) => {
                error!("Failed to capture response from {member_name}: {e}");
                self.router.agent_finished(&member_name);
                return Err(e);
            }
            Err(e) => {
                error!("Task join error while capturing response from {member_name}: {e}");
                self.router.agent_finished(&member_name);
                return Err(PluginError::Runtime {
                    message: format!("Task join error: {e}"),
                    source: Some(Box::new(e)),
                });
            }
        };

        debug!("Captured response from {member_name}: {} bytes", captured.response_text.len());

        // Record activity after successful response capture
        self.record_activity(ctx, team_name, &member_name);

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

        // Use workers.team_name or message team (if present)
        let team_name = self.resolve_team_name(ctx, Some(&message));
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

        // Mark agent as finished and check for queued messages
        if let Some(next_message) = self.router.agent_finished(&member_name) {
            debug!("Processing next queued message for {member_name}");
            Box::pin(self.process_message(config_key, next_message)).await?;
        }

        // Trigger nudge scan after this agent finishes (it may be Idle now
        // from a hook event that arrived while we were capturing the response).
        self.trigger_nudge_if_idle(&member_name).await;

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
        // Register agent in turn-level state tracker and store pane info
        {
            let mut state = self.agent_state.lock().unwrap();
            state.register_agent(member_name);
            state.set_pane_info(member_name, &handle.backend_id, &handle.log_file_path);
        }
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

    /// Poll PIDs of all registered workers and mark killed agents.
    ///
    /// Runs every `PID_POLL_INTERVAL_SECS` seconds. On Unix, uses
    /// `lifecycle::poll_worker_pid`. On other platforms is a no-op.
    fn poll_pids_for_killed_agents(&self) {
        let handles: Vec<(String, WorkerHandle)> = self
            .workers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        for (member_name, handle) in handles {
            if !lifecycle::poll_worker_pid(&handle) {
                // PID is gone — transition to Killed if not already
                let mut state = self.agent_state.lock().unwrap();
                let current = state.get_state(&member_name);
                if !matches!(current, Some(AgentState::Killed) | None) {
                    warn!("Worker {member_name} PID gone — marking as Killed");
                    state.set_state(&member_name, AgentState::Killed);
                }
            }
        }
    }

    /// Rotate log files for all workers if needed
    fn rotate_logs_if_needed(&self) {
        for handle in self.workers.values() {
            if let Err(e) = lifecycle::rotate_log_if_needed(&handle.log_file_path) {
                error!("Failed to rotate log for {}: {e}", handle.agent_id);
            }
        }
    }

    /// Scan all registered workers and nudge those that are `Idle` with unread mail.
    ///
    /// Called periodically from the `run()` loop to catch hook-driven `Busy → Idle`
    /// transitions that happen while the plugin is idle (no `process_message` in flight).
    async fn scan_and_nudge_idle_agents(&mut self) {
        if !self.config.nudge.enabled {
            return;
        }
        let member_names: Vec<String> = self.workers.keys().cloned().collect();
        for member_name in member_names {
            self.trigger_nudge_if_idle(&member_name).await;
        }
    }

    /// Deliver pub/sub notifications for `agent` transitioning to `new_state`.
    ///
    /// Looks up all non-expired subscribers interested in the event, then writes
    /// an [`InboxMessage`] to each subscriber's inbox using the standard
    /// [`inbox_append`] atomic writer.
    ///
    /// This is best-effort: individual delivery failures are logged as warnings
    /// but do not stop delivery to other subscribers.
    fn deliver_pubsub_notifications(&self, agent: &str, new_state: &str) {
        let subscribers = {
            self.pubsub
                .lock()
                .unwrap()
                .matching_subscribers(agent, new_state)
        };

        if subscribers.is_empty() {
            return;
        }

        let ctx = match &self.ctx {
            Some(c) => c,
            None => return,
        };

        let team_name = if self.config.team_name.is_empty() {
            ctx.system.default_team.as_str()
        } else {
            self.config.team_name.as_str()
        };

        for subscriber in &subscribers {
            let notification_text = format!("[AGENT STATE] {} is now {}", agent, new_state);
            let msg = InboxMessage {
                from: "daemon".to_string(),
                text: notification_text,
                timestamp: Utc::now().to_rfc3339(),
                read: false,
                summary: Some(format!("Agent {} → {}", agent, new_state)),
                message_id: Some(Uuid::new_v4().to_string()),
                unknown_fields: HashMap::new(),
            };
            let inbox_path = self.agent_inbox_path(ctx, team_name, subscriber);
            if let Err(e) = inbox_append(&inbox_path, &msg, team_name, subscriber) {
                warn!(
                    "Failed to deliver pubsub notification to {subscriber}: {e}"
                );
            } else {
                debug!(
                    "Delivered pubsub notification to {subscriber}: {agent} → {new_state}"
                );
            }
        }
    }

    /// Scan current agent states against the last-notified snapshot and deliver
    /// notifications for any changes.
    ///
    /// Called periodically from the `run()` loop (every 5 s, sharing the nudge
    /// scan timer) to catch state transitions driven by [`HookWatcher`] or
    /// PID polling that happen asynchronously without going through
    /// `process_message`.
    fn scan_and_deliver_pubsub_notifications(&mut self) {
        let current_states = self.agent_state.lock().unwrap().all_states();
        for (agent, state) in &current_states {
            let changed = self.last_notified_states.get(agent) != Some(state);
            if changed {
                self.deliver_pubsub_notifications(agent, &state.to_string());
                self.last_notified_states.insert(agent.clone(), *state);
            }
        }
    }

    /// Check for inactive agents and mark them as offline
    async fn check_inactivity(&self) -> Result<(), PluginError> {
        let ctx = self.ctx.as_ref().ok_or_else(|| PluginError::Runtime {
            message: "Plugin context not initialized".to_string(),
            source: None,
        })?;

        // Use workers.team_name for team lookups, falling back to default_team
        let team_name = self.resolve_team_name(ctx, None);
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

        // Reinitialize nudge engine with the parsed config
        self.nudge_engine = NudgeEngine::new(self.config.nudge.clone());

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

            // Register all auto-started workers in the turn-level state tracker
            // and store pane info so socket queries can locate their log files.
            for (member_name, handle) in &self.workers {
                let mut state = self.agent_state.lock().unwrap();
                state.register_agent(member_name);
                state.set_pane_info(member_name, &handle.backend_id, &handle.log_file_path);
            }
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

        // Start hook event watcher as a background task
        if let Some(ctx) = &self.ctx {
            let events_path = Self::hook_events_path(ctx);
            // Ensure parent directory exists
            if let Some(parent) = events_path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    warn!("Could not create hook events directory {}: {e}", parent.display());
                }
            }
            let watcher = HookWatcher::new(events_path, Arc::clone(&self.agent_state));
            let watcher_cancel = cancel.clone();
            tokio::spawn(async move {
                watcher.run(watcher_cancel).await;
            });
            debug!("Hook event watcher started");
        }

        // Set up periodic inactivity check (every 30 seconds)
        let mut inactivity_timer = interval(Duration::from_secs(30));

        // Set up periodic health check (configurable, default 30 seconds)
        let health_check_interval = Duration::from_secs(self.config.health_check_interval_secs);
        let mut health_check_timer = interval(health_check_interval);

        // Set up periodic PID poll (every 5 seconds — detects killed agents)
        let mut pid_poll_timer = interval(Duration::from_secs(PID_POLL_INTERVAL_SECS));

        // Set up periodic log rotation check (every 5 minutes)
        let mut log_rotation_timer = interval(Duration::from_secs(300));

        // Set up periodic nudge scan (every 5 seconds).
        // This catches Busy → Idle transitions driven by hook events, which
        // arrive asynchronously via HookWatcher and are not otherwise signalled
        // to the plugin main loop.
        let mut nudge_scan_timer = interval(Duration::from_secs(5));

        // Set up periodic pub/sub GC (every 60 seconds) to evict expired
        // subscriptions and keep memory usage bounded.
        let mut pubsub_gc_timer = interval(Duration::from_secs(PUBSUB_GC_INTERVAL_SECS));

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
                _ = pid_poll_timer.tick() => {
                    self.poll_pids_for_killed_agents();
                }
                _ = log_rotation_timer.tick() => {
                    self.rotate_logs_if_needed();
                }
                _ = nudge_scan_timer.tick() => {
                    self.scan_and_nudge_idle_agents().await;
                    self.scan_and_deliver_pubsub_notifications();
                }
                _ = pubsub_gc_timer.tick() => {
                    let removed = self.pubsub.lock().unwrap().gc();
                    if removed > 0 {
                        debug!("PubSub GC: removed {removed} expired subscription(s)");
                    }
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

                // Unregister from lifecycle manager and state tracker
                self.lifecycle.unregister_worker(&member_name);
                self.agent_state.lock().unwrap().unregister_agent(&member_name);
            }

            info!("All workers shut down");
        }

        Ok(())
    }

    async fn handle_message(&mut self, msg: &InboxMessage) -> Result<(), PluginError> {
        if !self.config.enabled {
            return Ok(());
        }

        let ctx = self.ctx.as_ref().ok_or_else(|| PluginError::Runtime {
            message: "Plugin context not initialized".to_string(),
            source: None,
        })?;

        let team_name = self.resolve_team_name(ctx, Some(msg));

        let recipient = msg
            .unknown_fields
            .get("recipient")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let recipient = match recipient {
            Some(recipient) => recipient,
            None => {
                self.notify_routing_issue(
                    ctx,
                    team_name,
                    &msg.from,
                    "Recipient not specified in message metadata.",
                );
                return Ok(());
            }
        };

        // Determine target agent from unknown_fields["recipient"]
        let target_config_key = self
            .config
            .agents
            .iter()
            .find(|(_, cfg)| cfg.enabled && cfg.member_name == recipient)
            .map(|(key, _)| key.clone());

        let Some(config_key) = target_config_key else {
            self.notify_routing_issue(
                ctx,
                team_name,
                &msg.from,
                &format!("Recipient '{recipient}' not found or not enabled."),
            );
            return Ok(());
        };

        debug!(
            "Routing message from {} to agent {} (config key: {config_key})",
            msg.from,
            self.config.agents[&config_key].member_name
        );

        self.process_message(&config_key, msg.clone()).await
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

    #[test]
    fn test_agent_state_initially_empty() {
        let plugin = WorkerAdapterPlugin::new();
        let states = plugin.get_agent_states();
        assert!(states.is_empty());
    }
}
