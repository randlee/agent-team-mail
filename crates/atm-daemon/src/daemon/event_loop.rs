//! Main daemon event loop

use crate::daemon::{graceful_shutdown, spool_drain_loop, start_socket_server, watch_inboxes, InboxEvent, InboxEventKind, SharedPubSubStore, SharedSessionRegistry, SharedStateStore};
use crate::daemon::status::{PluginStatus, PluginStatusKind, StatusWriter};
use crate::plugin::{Capability, PluginContext, PluginRegistry};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Run the main daemon event loop.
///
/// This function:
/// 1. Initializes all plugins via the registry
/// 2. Spawns each plugin's run() method in its own task
/// 3. Starts the Unix socket server backed by `state_store` and `pubsub_store`
/// 4. Starts the spool drain background task
/// 5. Starts the file system watcher
/// 6. Writes daemon status periodically
/// 7. Waits for cancellation signal
/// 8. Performs graceful shutdown of all plugins
///
/// # Arguments
///
/// * `registry` - Mutable plugin registry (plugins will be taken out for task spawning)
/// * `ctx` - Shared plugin context
/// * `cancel` - Cancellation token for shutdown coordination
/// * `status_writer` - Status file writer for daemon state tracking
/// * `state_store` - Shared agent state store for the socket server.
///   When the worker adapter plugin is enabled the caller should pass the
///   same `Arc` that was given to `WorkerAdapterPlugin::with_state_store`
///   so that the socket server sees live agent state.  When the worker
///   adapter is absent, pass a fresh `new_state_store()`; the socket
///   server will still accept connections but return `AGENT_NOT_FOUND`
///   for all agent-state queries.
/// * `pubsub_store` - Shared pub/sub registry for subscribe/unsubscribe requests.
///   Pass the same `Arc` from `WorkerAdapterPlugin::pubsub_store()` so that
///   CLI subscriptions are routed to the same registry that delivers notifications.
///   Pass `new_pubsub_store()` when the worker adapter is absent.
/// * `launch_tx` - Shared sender for the agent launch channel.
///   When the worker adapter plugin is enabled the caller should populate
///   the inner `Option` with the sender half of the channel and pass the
///   receiver to `WorkerAdapterPlugin::set_launch_receiver`.  Pass
///   `new_launch_sender()` (with empty inner) when the plugin is absent.
/// * `session_registry` - Shared session registry for `session-query` socket
///   commands. Pass `new_session_registry()` from `crate::daemon`.
#[expect(
    clippy::too_many_arguments,
    reason = "event loop wiring needs shared runtime handles and plugin coordination state"
)]
pub async fn run(
    registry: &mut PluginRegistry,
    ctx: &PluginContext,
    cancel: CancellationToken,
    status_writer: Arc<StatusWriter>,
    state_store: SharedStateStore,
    pubsub_store: SharedPubSubStore,
    launch_tx: crate::daemon::LaunchSender,
    session_registry: SharedSessionRegistry,
) -> Result<()> {
    info!("Initializing daemon event loop");

    // Initialize all plugins
    info!("Initializing {} plugin(s)", registry.len());
    registry
        .init_all(ctx)
        .await
        .context("Failed to initialize plugins")?;

    // Take plugins out of the registry for task spawning
    let plugins = registry.take_plugins();
    info!("Starting {} plugin task(s)", plugins.len());

    // Spawn a task for each plugin's run() method
    let mut plugin_tasks: Vec<JoinHandle<()>> = Vec::new();

    for (metadata, plugin_arc) in plugins.clone() {
        let plugin_name = metadata.name.to_string();
        let cancel_clone = cancel.clone();

        let task = tokio::spawn(async move {
            info!("Plugin {} run() starting", plugin_name);
            let mut plugin = plugin_arc.lock().await;

            match plugin.run(cancel_clone).await {
                Ok(()) => {
                    info!("Plugin {} run() completed", plugin_name);
                }
                Err(e) => {
                    error!("Plugin {} run() failed: {}", plugin_name, e);
                }
            }
        });

        plugin_tasks.push(task);
    }

    // Start the Unix socket server (CLI↔daemon IPC).
    //
    // The socket path is ${ATM_HOME}/.claude/daemon/atm-daemon.sock.
    // ctx.system.claude_root is ${ATM_HOME}/.claude, so the home_dir is its
    // parent. We fall back to get_home_dir() if the parent cannot be determined
    // (e.g., claude_root is the filesystem root, which should never happen in
    // practice).
    //
    // `state_store` is the same Arc that the WorkerAdapterPlugin was given at
    // construction time, so the socket server reads live agent state. When the
    // worker adapter is not enabled the caller passes a fresh empty store; the
    // socket server still accepts connections but returns AGENT_NOT_FOUND.
    let socket_home_dir = ctx.system.claude_root
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| {
            agent_team_mail_core::home::get_home_dir().unwrap_or_else(|_| ctx.system.claude_root.clone())
        });
    let socket_cancel = cancel.clone();
    let _socket_server_handle = match start_socket_server(socket_home_dir, state_store, pubsub_store, launch_tx, session_registry, socket_cancel).await {
        Ok(handle) => {
            if handle.is_some() {
                info!("Unix socket server started successfully");
            }
            handle
        }
        Err(e) => {
            warn!("Failed to start Unix socket server (daemon will continue without it): {e}");
            None
        }
    };

    // Start spool drain loop
    let teams_root = ctx.mail.teams_root().clone();
    let spool_cancel = cancel.clone();
    let spool_task = tokio::spawn(async move {
        if let Err(e) = spool_drain_loop(
            teams_root,
            Duration::from_secs(10), // Drain every 10 seconds
            spool_cancel,
        )
        .await
        {
            error!("Spool drain loop failed: {}", e);
        }
    });

    // Create event channel for watcher → dispatch communication
    let (event_tx, mut event_rx) = mpsc::channel::<InboxEvent>(100);

    // Extract hostname registry from bridge config (if available)
    let hostname_registry = extract_hostname_registry(&ctx.config);

    // Start file system watcher
    let watcher_root = ctx.mail.teams_root().clone();
    let watcher_cancel = cancel.clone();
    let watcher_task = tokio::spawn(async move {
        if let Err(e) = watch_inboxes(watcher_root, event_tx, hostname_registry, watcher_cancel).await {
            error!("Inbox watcher failed: {}", e);
        }
    });

    // Start event dispatch loop for EventListener plugins
    let dispatch_plugins = plugins.clone();
    let dispatch_cancel = cancel.clone();
    let dispatch_task = tokio::spawn(async move {
        info!("Starting event dispatch loop");
        let mut cursors: std::collections::HashMap<std::path::PathBuf, InboxCursor> =
            std::collections::HashMap::new();
        let mut read_error_count: u64 = 0;
        loop {
            tokio::select! {
                _ = dispatch_cancel.cancelled() => {
                    info!("Event dispatch cancelled");
                    break;
                }
                Some(event) = event_rx.recv() => {
                    let dispatch_span = tracing::info_span!(
                        "daemon_dispatch",
                        team = %event.team,
                        agent = %event.agent,
                        path = %event.path.display()
                    );
                    let _span_guard = dispatch_span.enter();
                    debug!("Dispatching event: team={}, agent={}, kind={:?}",
                           event.team, event.agent, event.kind);

                    // Only dispatch MessageReceived events
                    if event.kind != InboxEventKind::MessageReceived {
                        continue;
                    }

                    let cursor = cursors.entry(event.path.clone()).or_default();
                    let inbox_msgs = match read_new_inbox_messages(&event.path, cursor).await {
                        Ok(msgs) => msgs,
                        Err(e) => {
                            read_error_count += 1;
                            // File might be transiently locked, deleted, or malformed
                            warn!(
                                "Failed to read inbox at {}: {} (errors={})",
                                event.path.display(),
                                e,
                                read_error_count
                            );
                            continue;
                        }
                    };

                    if inbox_msgs.is_empty() {
                        continue;
                    }

                    for mut inbox_msg in inbox_msgs {
                        emit_event_best_effort(EventFields {
                            level: "info",
                            source: "atm-daemon",
                            action: "dispatch_message",
                            team: Some(event.team.clone()),
                            session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
                            agent_id: Some(event.agent.clone()),
                            agent_name: Some(event.agent.clone()),
                            message_id: inbox_msg.message_id.clone(),
                            result: Some("received".to_string()),
                            ..Default::default()
                        });

                        // Attach routing metadata for plugins
                        inbox_msg
                            .unknown_fields
                            .insert("recipient".to_string(), Value::String(event.agent.clone()));
                        inbox_msg
                            .unknown_fields
                            .insert("team".to_string(), Value::String(event.team.clone()));
                        inbox_msg.unknown_fields.insert(
                            "path".to_string(),
                            Value::String(event.path.display().to_string()),
                        );
                        if let Some(origin) = &event.origin {
                            inbox_msg.unknown_fields.insert(
                                "origin".to_string(),
                                Value::String(origin.clone()),
                            );
                        }

                        // Dispatch to all plugins with EventListener capability
                        for (metadata, plugin_arc) in &dispatch_plugins {
                            if metadata.capabilities.contains(&Capability::EventListener) {
                                // Await lock to avoid dropping events under load
                                let mut plugin = plugin_arc.lock().await;
                                debug!("Dispatching to plugin: {}", metadata.name);
                                if let Err(e) = plugin.handle_message(&inbox_msg).await {
                                    emit_event_best_effort(EventFields {
                                        level: "error",
                                        source: "atm-daemon",
                                        action: "dispatch_plugin_error",
                                        team: Some(event.team.clone()),
                                        session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
                                        agent_id: Some(event.agent.clone()),
                                        agent_name: Some(event.agent.clone()),
                                        message_id: inbox_msg.message_id.clone(),
                                        target: Some(metadata.name.to_string()),
                                        result: Some("error".to_string()),
                                        error: Some(e.to_string()),
                                        ..Default::default()
                                    });
                                    error!("Plugin {} handle_message error: {}", metadata.name, e);
                                }
                            }
                        }
                    }
                }
            }
        }
        info!("Event dispatch loop stopped");
    });

    // Start retention task if enabled
    let retention_task = if ctx.config.retention.enabled {
        info!("Starting retention task (interval: {}s)", ctx.config.retention.interval_secs);
        let retention_cancel = cancel.clone();
        let retention_ctx = ctx.clone();
        Some(tokio::spawn(async move {
            retention_loop(retention_ctx, retention_cancel).await;
        }))
    } else {
        info!("Retention task disabled in config");
        None
    };

    // Start status writer task
    let status_cancel = cancel.clone();
    let status_writer_clone = status_writer.clone();
    let status_plugins = plugins.clone();
    let status_ctx = ctx.clone();
    let status_task = tokio::spawn(async move {
        status_writer_loop(status_writer_clone, status_plugins, status_ctx, status_cancel).await;
    });

    info!("Daemon event loop running. Waiting for cancellation signal...");

    // Wait for cancellation
    cancel.cancelled().await;
    info!("Cancellation signal received. Beginning shutdown...");

    // Wait for background tasks to complete (they should respect cancellation)
    if let Err(e) = tokio::time::timeout(Duration::from_secs(5), spool_task).await {
        error!("Spool task did not complete in time: {}", e);
    }

    if let Err(e) = tokio::time::timeout(Duration::from_secs(5), watcher_task).await {
        error!("Watcher task did not complete in time: {}", e);
    }

    if let Err(e) = tokio::time::timeout(Duration::from_secs(5), dispatch_task).await {
        error!("Dispatch task did not complete in time: {}", e);
    }

    if let Some(task) = retention_task {
        if let Err(e) = tokio::time::timeout(Duration::from_secs(5), task).await {
            error!("Retention task did not complete in time: {}", e);
        }
    }

    if let Err(e) = tokio::time::timeout(Duration::from_secs(5), status_task).await {
        error!("Status writer task did not complete in time: {}", e);
    }

    // Graceful shutdown of all plugins
    graceful_shutdown(plugins, Duration::from_secs(5))
        .await
        .context("Plugin shutdown encountered errors")?;

    // Wait for plugin tasks to complete
    for task in plugin_tasks {
        if let Err(e) = task.await {
            error!("Plugin task panicked: {}", e);
        }
    }

    info!("Daemon event loop shutdown complete");
    Ok(())
}

#[derive(Default, Debug, Clone)]
struct InboxCursor {
    last_message_id: Option<String>,
    last_index: usize,
}

async fn read_new_inbox_messages(
    path: &std::path::Path,
    cursor: &mut InboxCursor,
) -> Result<Vec<agent_team_mail_core::schema::InboxMessage>> {
    let content = tokio::fs::read_to_string(path).await?;
    let inbox_msgs: Vec<agent_team_mail_core::schema::InboxMessage> = serde_json::from_str(&content)?;
    if inbox_msgs.is_empty() {
        cursor.last_message_id = None;
        cursor.last_index = 0;
        return Ok(Vec::new());
    }

    let mut start_idx = 0;
    if let Some(last_id) = cursor.last_message_id.as_ref() {
        if let Some(pos) = inbox_msgs
            .iter()
            .position(|msg| msg.message_id.as_ref() == Some(last_id))
        {
            start_idx = pos + 1;
        } else {
            start_idx = 0;
        }
    } else if cursor.last_index <= inbox_msgs.len() {
        start_idx = cursor.last_index;
    }

    if start_idx > inbox_msgs.len() {
        start_idx = 0;
    }

    let new_msgs = inbox_msgs[start_idx..].to_vec();
    cursor.last_index = inbox_msgs.len();
    cursor.last_message_id = inbox_msgs.last().and_then(|m| m.message_id.clone());
    Ok(new_msgs)
}

/// Extract hostname registry from bridge plugin config
///
/// Returns None if bridge plugin is not configured or not enabled.
fn extract_hostname_registry(config: &agent_team_mail_core::config::Config) -> Option<std::sync::Arc<agent_team_mail_core::config::HostnameRegistry>> {
    use agent_team_mail_core::config::BridgeConfig;

    // Check if bridge plugin config exists
    let bridge_table = config.plugins.get("bridge")?;

    // Parse bridge config
    let bridge_config: BridgeConfig = match bridge_table.clone().try_into() {
        Ok(cfg) => cfg,
        Err(e) => {
            warn!("Failed to parse bridge config: {}", e);
            return None;
        }
    };

    // Check if bridge is enabled
    if !bridge_config.enabled {
        return None;
    }

    // Build hostname registry from remotes
    let mut registry = agent_team_mail_core::config::HostnameRegistry::new();
    for remote in bridge_config.remotes {
        if let Err(e) = registry.register(remote) {
            warn!("Failed to register remote in hostname registry: {}", e);
        }
    }

    Some(std::sync::Arc::new(registry))
}

/// Periodic retention task
///
/// Runs retention on all team inbox files at configured intervals.
/// Also cleans up old CI report files if CI monitor plugin is configured.
async fn retention_loop(ctx: PluginContext, cancel: CancellationToken) {
    // Extract retention config
    let config = &ctx.config.retention;
    let interval_secs = config.interval_secs;

    // Set up defaults for daemon mode
    let max_age = config.max_age.clone().or_else(|| Some("30d".to_string()));
    let max_count = config.max_count.or(Some(1000));

    let retention_policy = agent_team_mail_core::config::RetentionConfig {
        max_age,
        max_count,
        strategy: config.strategy,
        archive_dir: config.archive_dir.clone(),
        enabled: config.enabled,
        interval_secs: config.interval_secs,
    };

    let teams_root = ctx.mail.teams_root().clone();

    // Extract report_dir from CI monitor plugin config if present
    let report_dir: Option<PathBuf> = ctx
        .config
        .plugin_config("ci_monitor")
        .and_then(|table| table.get("report_dir"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from);

    info!("Retention loop started (interval: {}s)", interval_secs);

    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("Retention loop cancelled");
                break;
            }
            _ = interval.tick() => {
                debug!("Running periodic retention");

                // Run retention work in spawn_blocking to avoid blocking the tokio runtime
                let teams_root_clone = teams_root.clone();
                let retention_policy_clone = retention_policy.clone();
                let report_dir_clone = report_dir.clone();

                let result = tokio::task::spawn_blocking(move || {
                    retention_work(&teams_root_clone, &retention_policy_clone, report_dir_clone.as_ref())
                }).await;

                if let Err(e) = result {
                    error!("Retention task panicked: {}", e);
                }
            }
        }
    }

    info!("Retention loop stopped");
}

/// Perform retention work synchronously (called from spawn_blocking)
fn retention_work(
    teams_root: &PathBuf,
    retention_policy: &agent_team_mail_core::config::RetentionConfig,
    report_dir: Option<&PathBuf>,
) {
    use agent_team_mail_core::retention::{apply_retention, clean_report_files};
    use agent_team_mail_core::retention::parse_duration;

    // Enumerate team directories
    let team_dirs = match std::fs::read_dir(teams_root) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::error!("Failed to read teams directory {}: {}", teams_root.display(), e);
            return;
        }
    };

    for team_entry in team_dirs {
        let team_entry = match team_entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Failed to read team directory entry: {}", e);
                continue;
            }
        };

        let team_path = team_entry.path();
        if !team_path.is_dir() {
            continue;
        }

        let team_name = match team_path.file_name() {
            Some(name) => name.to_string_lossy().to_string(),
            None => continue,
        };

        // Enumerate agent inbox files in the inboxes/ subdirectory
        let inboxes_path = team_path.join("inboxes");
        if !inboxes_path.is_dir() {
            // No inboxes subdirectory, skip this team
            continue;
        }

        let agents = match std::fs::read_dir(&inboxes_path) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("Failed to read inboxes directory {}: {}", inboxes_path.display(), e);
                continue;
            }
        };

        for agent_entry in agents {
            let agent_entry = match agent_entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("Failed to read agent entry: {}", e);
                    continue;
                }
            };

            let agent_path = agent_entry.path();

            // Look for inbox files: agent.json or agent.hostname.json
            if !agent_path.is_file() {
                continue;
            }

            let file_name = match agent_path.file_name() {
                Some(name) => name.to_string_lossy().to_string(),
                None => continue,
            };

            // Parse agent name from filename
            // Format: <agent>.json or <agent>.<hostname>.json
            let agent_name = if file_name.ends_with(".json") {
                let base = &file_name[..file_name.len() - 5]; // Remove .json

                // Check if this is a bridge inbox (has hostname suffix)
                // Bridge inboxes have format: agent.hostname.json
                // Local inboxes have format: agent.json
                // Agent names may contain dots, so we can't just split on dots.
                //
                // Heuristic: If the base contains a dot, assume the last component
                // is the hostname (bridge inbox). Otherwise, use the whole base.
                if base.contains('.') {
                    // Has a dot — might be agent.hostname or just agent.with.dots
                    // We'll use the entire base as the agent name for retention purposes.
                    // Retention doesn't need to distinguish local vs bridge inboxes;
                    // it just needs a stable identifier for the archive directory.
                    base.to_string()
                } else {
                    // No dot — simple agent name
                    base.to_string()
                }
            } else {
                continue;
            };

            // Apply retention to this inbox
            tracing::debug!("Applying retention to {}/{}/{}", team_name, agent_name, file_name);
            match apply_retention(
                &agent_path,
                &team_name,
                &agent_name,
                retention_policy,
                false, // Not a dry run
            ) {
                Ok(result) => {
                    if result.removed > 0 {
                        tracing::info!(
                            "Retention: {}/{}/{}: kept={}, removed={}, archived={}",
                            team_name, agent_name, file_name,
                            result.kept, result.removed, result.archived
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(
                        "Retention failed for {}/{}/{}: {}",
                        team_name, agent_name, file_name, e
                    );
                }
            }
        }
    }

    // Clean up old report files if configured
    if let Some(dir) = report_dir {
        tracing::debug!("Cleaning old report files from {}", dir.display());

        // Use max_age for report files (default 30 days)
        let max_age_str = retention_policy.max_age.as_deref().unwrap_or("30d");
        let max_age_duration = match parse_duration(max_age_str) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("Invalid max_age duration '{}': {}", max_age_str, e);
                return;
            }
        };

        match clean_report_files(dir, &max_age_duration) {
            Ok(result) => {
                if result.deleted_count > 0 {
                    tracing::info!(
                        "Report cleanup: deleted={}, skipped={}",
                        result.deleted_count, result.skipped_count
                    );
                }
            }
            Err(e) => {
                tracing::error!("Report cleanup failed: {}", e);
            }
        }
    }
}

/// Periodic status writer task
///
/// Writes daemon status to status.json at regular intervals (every 30 seconds).
/// Status includes plugin states, active teams, PID, and uptime.
async fn status_writer_loop(
    status_writer: Arc<StatusWriter>,
    plugins: Vec<(crate::plugin::PluginMetadata, crate::plugin::SharedPlugin)>,
    ctx: PluginContext,
    cancel: CancellationToken,
) {
    info!("Status writer loop started (interval: 30s)");

    // Write initial status at startup
    let plugin_statuses = build_plugin_statuses(&plugins).await;
    let teams = get_active_teams(&ctx).await;
    if let Err(e) = status_writer.write_status(plugin_statuses.clone(), teams.clone()) {
        error!("Failed to write initial daemon status: {}", e);
    }

    let mut interval = tokio::time::interval(Duration::from_secs(30));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("Status writer loop cancelled");
                break;
            }
            _ = interval.tick() => {
                debug!("Writing daemon status");

                let plugin_statuses = build_plugin_statuses(&plugins).await;
                let teams = get_active_teams(&ctx).await;

                if let Err(e) = status_writer.write_status(plugin_statuses, teams) {
                    error!("Failed to write daemon status: {}", e);
                }
            }
        }
    }

    info!("Status writer loop stopped");
}

/// Build plugin status list from running plugins
async fn build_plugin_statuses(
    plugins: &[(crate::plugin::PluginMetadata, crate::plugin::SharedPlugin)],
) -> Vec<PluginStatus> {
    let mut statuses = Vec::new();

    for (metadata, _plugin_arc) in plugins {
        // For now, all plugins are running (we don't track per-plugin errors yet)
        // Future enhancement: track plugin-level errors via shared state
        statuses.push(PluginStatus {
            name: metadata.name.to_string(),
            enabled: true,
            status: PluginStatusKind::Running,
            last_error: None,
            last_updated: Some(format_timestamp(SystemTime::now())),
        });
    }

    statuses
}

/// Get list of active teams from the teams directory (async wrapper)
async fn get_active_teams(ctx: &PluginContext) -> Vec<String> {
    let teams_root = ctx.mail.teams_root().clone();

    // Use spawn_blocking to avoid blocking the async runtime
    tokio::task::spawn_blocking(move || {
        let mut teams = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&teams_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name() {
                        teams.push(name.to_string_lossy().to_string());
                    }
                }
            }
        }

        teams.sort();
        teams
    })
    .await
    .unwrap_or_default()
}

/// Format timestamp as ISO 8601 string
fn format_timestamp(time: SystemTime) -> String {
    use chrono::{DateTime, Utc};

    let duration = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = duration.as_secs();
    let nanos = duration.subsec_nanos();

    let dt = DateTime::<Utc>::from_timestamp(secs as i64, nanos)
        .unwrap_or_else(Utc::now);
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::{read_new_inbox_messages, InboxCursor};
    use agent_team_mail_core::schema::InboxMessage;
    use std::collections::HashMap;
    use tempfile::TempDir;
    use tokio::fs;

    async fn write_inbox(path: &std::path::Path, msgs: &[InboxMessage]) {
        let content = serde_json::to_string_pretty(msgs).unwrap();
        fs::write(path, content).await.unwrap();
    }

    #[tokio::test]
    async fn test_read_new_inbox_messages_returns_all_new() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("inbox.json");
        let mut cursor = InboxCursor::default();

        let msg1 = InboxMessage {
            from: "a".to_string(),
            text: "first".to_string(),
            timestamp: "2026-02-11T10:00:00Z".to_string(),
            read: false,
            summary: None,
            message_id: Some("msg-1".to_string()),
            unknown_fields: HashMap::new(),
        };

        let msg2 = InboxMessage {
            from: "b".to_string(),
            text: "second".to_string(),
            timestamp: "2026-02-11T10:05:00Z".to_string(),
            read: false,
            summary: None,
            message_id: Some("msg-2".to_string()),
            unknown_fields: HashMap::new(),
        };

        let msgs = vec![msg1.clone(), msg2.clone()];
        write_inbox(&path, &msgs).await;

        let new_msgs = read_new_inbox_messages(&path, &mut cursor).await.unwrap();
        assert_eq!(new_msgs.len(), 2);
        assert_eq!(new_msgs[0].text, "first");
        assert_eq!(new_msgs[1].text, "second");

        let msg3 = InboxMessage {
            from: "c".to_string(),
            text: "third".to_string(),
            timestamp: "2026-02-11T10:10:00Z".to_string(),
            read: false,
            summary: None,
            message_id: Some("msg-3".to_string()),
            unknown_fields: HashMap::new(),
        };

        let msgs = vec![msg1, msg2, msg3.clone()];
        write_inbox(&path, &msgs).await;
        let new_msgs = read_new_inbox_messages(&path, &mut cursor).await.unwrap();
        assert_eq!(new_msgs.len(), 1);
        assert_eq!(new_msgs[0].text, "third");
    }

    #[tokio::test]
    async fn test_read_new_inbox_messages_empty_returns_none() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("inbox.json");
        let mut cursor = InboxCursor::default();

        write_inbox(&path, &[]).await;
        let new_msgs = read_new_inbox_messages(&path, &mut cursor).await.unwrap();
        assert!(new_msgs.is_empty());
    }

    #[tokio::test]
    async fn test_read_new_inbox_messages_resets_on_truncate() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("inbox.json");
        let mut cursor = InboxCursor::default();

        let msg1 = InboxMessage {
            from: "a".to_string(),
            text: "first".to_string(),
            timestamp: "2026-02-11T10:00:00Z".to_string(),
            read: false,
            summary: None,
            message_id: Some("msg-1".to_string()),
            unknown_fields: HashMap::new(),
        };

        let msg2 = InboxMessage {
            from: "b".to_string(),
            text: "second".to_string(),
            timestamp: "2026-02-11T10:05:00Z".to_string(),
            read: false,
            summary: None,
            message_id: Some("msg-2".to_string()),
            unknown_fields: HashMap::new(),
        };

        write_inbox(&path, &[msg1.clone(), msg2]).await;
        let _ = read_new_inbox_messages(&path, &mut cursor).await.unwrap();

        write_inbox(&path, std::slice::from_ref(&msg1)).await;
        let new_msgs = read_new_inbox_messages(&path, &mut cursor).await.unwrap();
        assert_eq!(new_msgs.len(), 1);
        assert_eq!(new_msgs[0].text, "first");
    }
}
