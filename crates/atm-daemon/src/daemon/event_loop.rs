//! Main daemon event loop

use crate::daemon::pid_backend_validation::{roster_process_id, validate_pid_backend};
use crate::daemon::status::{PluginStatus, PluginStatusKind, StatusWriter};
use crate::daemon::{
    InboxEvent, InboxEventKind, LogEventQueue, SharedDedupeStore, SharedPubSubStore,
    SharedSessionRegistry, SharedStateStore, SharedStreamEventSender, graceful_shutdown,
    spool_drain_loop, start_socket_server, watch_inboxes,
};
use crate::plugin::{Capability, PluginContext, PluginRegistry};
use crate::plugins::worker_adapter::AgentState;
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::io::{atomic::atomic_swap, lock::acquire_lock};
use agent_team_mail_core::schema::TeamConfig;
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Tracks how many consecutive reconcile cycles a dead session has been absent
/// from team config before stale-prune is allowed. This guards against
/// remove+re-add watcher races and prevents deleting legitimate members mid-update.
static ABSENT_REGISTRY_CYCLES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, u8>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Tracks consecutive reconcile cycles where a non-lead member is present in
/// team config with a dead daemon session. Terminal cleanup fires only after
/// two full cycles, giving re-added members a grace period to re-register.
static DEAD_MEMBER_CYCLES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, u8>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

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
/// * `dedup_store` - Durable dedupe store shared with the socket server for
///   restart-safe idempotency. Create with `crate::daemon::new_dedup_store()`.
/// * `stream_state_store` - Per-agent stream turn state store.
/// * `stream_event_sender` - Broadcast sender for push-based stream event fanout.
///   Create with `crate::daemon::new_stream_event_sender()`.
/// * `log_event_queue` - Bounded queue for `"log-event"` socket commands.
///   Create with `crate::daemon::new_log_event_queue()`.
#[expect(
    clippy::too_many_arguments,
    reason = "event loop wiring needs shared runtime handles and plugin coordination state"
)]
pub async fn run(
    registry: &mut PluginRegistry,
    ctx: &PluginContext,
    daemon_lock: agent_team_mail_core::io::lock::FileLock,
    cancel: CancellationToken,
    status_writer: Arc<StatusWriter>,
    state_store: SharedStateStore,
    pubsub_store: SharedPubSubStore,
    launch_tx: crate::daemon::LaunchSender,
    session_registry: SharedSessionRegistry,
    dedup_store: SharedDedupeStore,
    stream_state_store: crate::daemon::SharedStreamStateStore,
    stream_event_sender: SharedStreamEventSender,
    log_event_queue: LogEventQueue,
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
    let socket_home_dir = ctx
        .system
        .claude_root
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| {
            agent_team_mail_core::home::get_home_dir()
                .unwrap_or_else(|_| ctx.system.claude_root.clone())
        });
    let socket_cancel = cancel.clone();
    let _socket_server_handle = match start_socket_server(
        socket_home_dir,
        state_store.clone(),
        pubsub_store,
        launch_tx,
        session_registry.clone(),
        dedup_store,
        stream_state_store,
        stream_event_sender,
        log_event_queue,
        &daemon_lock,
        socket_cancel,
    )
    .await
    {
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
        if let Err(e) =
            watch_inboxes(watcher_root, event_tx, hostname_registry, watcher_cancel).await
        {
            error!("Inbox watcher failed: {}", e);
        }
    });

    // Start event dispatch loop for EventListener plugins
    let dispatch_plugins = plugins.clone();
    let dispatch_cancel = cancel.clone();
    let dispatch_reconcile_ctx = ctx.clone();
    let dispatch_reconcile_registry = session_registry.clone();
    let dispatch_reconcile_state_store = state_store.clone();
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

                    // Team config watcher event: reconcile immediately on config.json changes.
                    if event.agent == "__config__" {
                        let claude_root = dispatch_reconcile_ctx.system.claude_root.clone();
                        let session_registry = dispatch_reconcile_registry.clone();
                        let state_store = dispatch_reconcile_state_store.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            reconcile_team_member_activity_with_mode(
                                &claude_root,
                                &session_registry,
                                &state_store,
                                false,
                            )
                        })
                        .await;
                        match result {
                            Ok(Ok(())) => debug!("config.json reconcile pass completed"),
                            Ok(Err(e)) => warn!("config.json reconcile pass failed: {e}"),
                            Err(e) => warn!("config.json reconcile task panicked: {e}"),
                        }

                        continue;
                    }

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
        info!(
            "Starting retention task (interval: {}s)",
            ctx.config.retention.interval_secs
        );
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
        status_writer_loop(
            status_writer_clone,
            status_plugins,
            status_ctx,
            status_cancel,
        )
        .await;
    });

    let reconcile_cancel = cancel.clone();
    let reconcile_ctx = ctx.clone();
    let reconcile_registry = session_registry.clone();
    let reconcile_state_store = state_store.clone();

    // Run one startup reconcile immediately so roster/state is available
    // without waiting for the periodic interval tick.
    {
        let claude_root = ctx.system.claude_root.clone();
        let startup_registry = session_registry.clone();
        let startup_state_store = state_store.clone();
        let startup_result = tokio::task::spawn_blocking(move || {
            reconcile_team_member_activity(&claude_root, &startup_registry, &startup_state_store)
        })
        .await;
        match startup_result {
            Ok(Ok(())) => debug!("startup reconcile pass completed"),
            Ok(Err(e)) => warn!("startup reconcile pass failed: {e}"),
            Err(e) => warn!("startup reconcile task panicked: {e}"),
        }
    }

    let reconcile_task = tokio::spawn(async move {
        reconcile_loop(
            reconcile_ctx,
            reconcile_registry,
            reconcile_state_store,
            reconcile_cancel,
        )
        .await;
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
    if let Err(e) = tokio::time::timeout(Duration::from_secs(5), reconcile_task).await {
        error!("Reconcile task did not complete in time: {}", e);
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

async fn reconcile_loop(
    ctx: PluginContext,
    session_registry: SharedSessionRegistry,
    state_store: SharedStateStore,
    cancel: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = interval.tick() => {
                let claude_root = ctx.system.claude_root.clone();
                let session_registry = session_registry.clone();
                let state_store = state_store.clone();
                let result = tokio::task::spawn_blocking(move || {
                    reconcile_team_member_activity(&claude_root, &session_registry, &state_store)
                }).await;

                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => warn!("reconcile loop failed: {e}"),
                    Err(e) => warn!("reconcile loop task panicked: {e}"),
                }
            }
        }
    }
}

fn reconcile_team_member_activity(
    claude_root: &std::path::Path,
    session_registry: &SharedSessionRegistry,
    state_store: &SharedStateStore,
) -> Result<()> {
    reconcile_team_member_activity_with_mode(claude_root, session_registry, state_store, true)
}

fn reconcile_team_member_activity_with_mode(
    claude_root: &std::path::Path,
    session_registry: &SharedSessionRegistry,
    state_store: &SharedStateStore,
    advance_absent_prune_cycles: bool,
) -> Result<()> {
    let teams_root = claude_root.join("teams");
    if !teams_root.exists() {
        return Ok(());
    }

    let mut desired_agent_names = std::collections::HashSet::new();

    for entry in std::fs::read_dir(&teams_root)? {
        let Ok(entry) = entry else { continue };
        let team_dir = entry.path();
        if !team_dir.is_dir() {
            continue;
        }
        let config_path = team_dir.join("config.json");
        if !config_path.exists() {
            continue;
        }

        let content = match std::fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let mut config: TeamConfig = match serde_json::from_str(&content) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let team_name = config.name.clone();
        let mut changed = false;
        let mut terminal_non_lead_members: Vec<String> = Vec::new();
        for member in &mut config.members {
            desired_agent_names.insert(member.name.clone());
            let mut pid_backend_mismatch_detected = false;
            let mut record = {
                let reg = session_registry.lock().unwrap();
                reg.query_for_team(&team_name, &member.name).cloned()
            };

            {
                // Keep daemon state store seeded from config and lifecycle truth.
                let mut tracker = state_store.lock().unwrap();
                if tracker.get_state(&member.name).is_none() {
                    tracker.register_agent(&member.name);
                }
                match member.is_active {
                    Some(false) => tracker.set_state_with_context(
                        &member.name,
                        AgentState::Offline,
                        "config isActive=false",
                        "config_watcher",
                    ),
                    Some(true) => tracker.set_state_with_context(
                        &member.name,
                        AgentState::Active,
                        "config isActive=true",
                        "config_watcher",
                    ),
                    None => tracker.set_state_with_context(
                        &member.name,
                        AgentState::Unknown,
                        "config isActive missing",
                        "config_watcher",
                    ),
                }
            }

            // External/self-registration path: if config carries a session/PID hint
            // and daemon does not have that exact record yet, validate and upsert.
            let session_hint = member
                .session_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            let pid_hint = roster_process_id(member);
            let needs_registration = match (&record, session_hint.as_deref(), pid_hint) {
                (Some(rec), Some(sess), Some(pid)) => {
                    rec.session_id != sess || rec.process_id != pid
                }
                (None, Some(_), Some(_)) => true,
                _ => false,
            };

            if needs_registration
                && let (Some(sess), Some(pid)) = (session_hint.as_deref(), pid_hint)
            {
                let validation = validate_pid_backend(member, pid);
                if validation.is_alive_mismatch() {
                    let mismatch_message = format!(
                        "pid/backend mismatch at registration: agent='{}' backend='{}' expected='{}' actual='{}' pid={}",
                        member.name,
                        validation.backend,
                        validation.expected_display(),
                        validation.actual_display(),
                        validation.pid
                    );
                    warn!("{mismatch_message}");
                    emit_event_best_effort(EventFields {
                        level: "warn",
                        source: "atm-daemon",
                        action: "PID_PROCESS_MISMATCH",
                        team: Some(team_name.clone()),
                        agent_name: Some(member.name.clone()),
                        target: Some(format!("pid:{}", validation.pid)),
                        result: Some("registration".to_string()),
                        error: Some(mismatch_message.clone()),
                        ..Default::default()
                    });

                    state_store.lock().unwrap().set_state_with_context(
                        &member.name,
                        AgentState::Offline,
                        &mismatch_message,
                        "pid_backend_validation",
                    );
                    pid_backend_mismatch_detected = true;
                } else {
                    session_registry.lock().unwrap().upsert_for_team(
                        &team_name,
                        &member.name,
                        sess,
                        pid,
                    );
                    record = session_registry
                        .lock()
                        .unwrap()
                        .query_for_team(&team_name, &member.name)
                        .cloned();
                }
            }

            let alive = if let Some(ref record) = record {
                let is_alive = record.state
                    == crate::daemon::session_registry::SessionState::Active
                    && record.is_process_alive();

                if !is_alive
                    && record.state == crate::daemon::session_registry::SessionState::Active
                {
                    session_registry
                        .lock()
                        .unwrap()
                        .mark_dead_for_team(&team_name, &member.name);
                }
                is_alive
            } else {
                // No live session record means member is not active.
                false
            };
            let reconciled_alive = alive && !pid_backend_mismatch_detected;

            if member.is_active != Some(reconciled_alive) {
                let previous = member.is_active;
                member.is_active = Some(reconciled_alive);
                changed = true;
                emit_event_best_effort(EventFields {
                    level: "warn",
                    source: "atm-daemon",
                    action: "state_drift_detected",
                    team: Some(team_name.clone()),
                    agent_name: Some(member.name.clone()),
                    result: Some("reconciled".to_string()),
                    error: Some(format!(
                        "isActive drift for {}@{}: config={:?}, reconciled={}",
                        member.name, team_name, previous, reconciled_alive
                    )),
                    ..Default::default()
                });

                if pid_backend_mismatch_detected {
                    // Keep mismatch-offline state sticky for this pass.
                    // Re-registration must occur before Active is restored.
                } else if reconciled_alive {
                    member.last_active = Some(now_ms);
                    state_store.lock().unwrap().set_state_with_context(
                        &member.name,
                        AgentState::Active,
                        "session active + pid alive",
                        "session_reconcile",
                    );
                } else {
                    state_store.lock().unwrap().set_state_with_context(
                        &member.name,
                        AgentState::Offline,
                        "session missing/dead during reconcile",
                        "session_reconcile",
                    );
                }
            }

            // Terminal non-lead members must be fully removed (roster + mailbox)
            // once daemon confirms the session is dead. A grace-skip cycle is
            // inserted when a member just re-appeared after an absence (detected
            // via ABSENT_REGISTRY_CYCLES), preventing premature cleanup during
            // config-watcher races or remove+recreate flows.
            if member.name != "team-lead"
                && let Some(ref rec) = record
                && rec.state == crate::daemon::session_registry::SessionState::Dead
            {
                let key = format!("{team_name}:{}", member.name);
                // If the member was being tracked as absent (ABSENT_REGISTRY_CYCLES
                // has an entry), they just re-appeared in config. Reset dead-cycle
                // counter and skip this cycle entirely — terminal cleanup may only
                // fire after two consecutive cycles where the member is present in
                // config with a dead session starting from a clean counter.
                let was_absent = ABSENT_REGISTRY_CYCLES.lock().unwrap().contains_key(&key);
                if was_absent {
                    DEAD_MEMBER_CYCLES.lock().unwrap().remove(&key);
                } else {
                    let mut dead_cycles = DEAD_MEMBER_CYCLES.lock().unwrap();
                    let cycles = dead_cycles
                        .entry(key.clone())
                        .and_modify(|c| *c = c.saturating_add(1))
                        .or_insert(1);
                    if *cycles >= 2 {
                        terminal_non_lead_members.push(member.name.clone());
                        dead_cycles.remove(&key);
                    }
                }
            } else {
                // Member is alive, has no session, or is team-lead: reset counter.
                let key = format!("{team_name}:{}", member.name);
                DEAD_MEMBER_CYCLES.lock().unwrap().remove(&key);
            }
        }

        if !terminal_non_lead_members.is_empty() {
            for name in &terminal_non_lead_members {
                delete_member_inbox(&team_dir, name)?;
                session_registry
                    .lock()
                    .unwrap()
                    .remove_for_team(&team_name, name);
                state_store.lock().unwrap().unregister_agent(name);
                desired_agent_names.remove(name);
            }
            config
                .members
                .retain(|m| !terminal_non_lead_members.contains(&m.name));
            changed = true;
        }

        // Prune stale daemon session records for members no longer in this
        // team's config, but only after they remain absent for two full extra
        // reconcile cycles. This prevents deleting members that are mid-add
        // during config-watcher updates.
        {
            let live_member_names: std::collections::HashSet<String> =
                config.members.iter().map(|m| m.name.clone()).collect();
            let mut reg = session_registry.lock().unwrap();
            let tracked_for_team = reg.sessions_for_team(&team_name);
            let mut absent_cycles = ABSENT_REGISTRY_CYCLES.lock().unwrap();

            for tracked in tracked_for_team {
                let key = format!("{team_name}:{}", tracked.agent_name);
                if live_member_names.contains(&tracked.agent_name) {
                    absent_cycles.remove(&key);
                    continue;
                }

                // Only dead sessions are eligible for stale-prune. Active
                // sessions absent from config are left for later lifecycle
                // convergence to avoid racing legitimate add/update flows.
                if tracked.state != crate::daemon::session_registry::SessionState::Dead {
                    absent_cycles.remove(&key);
                    continue;
                }

                if !advance_absent_prune_cycles {
                    // Dispatch-triggered reconcile passes should not advance prune
                    // counters, but they must still record that this dead member
                    // was observed absent. That absence marker is used by the
                    // re-add guard in terminal cleanup to avoid deleting a member
                    // that was quickly removed and re-added.
                    absent_cycles.entry(key.clone()).or_insert(1);
                    continue;
                }

                let cycles = absent_cycles
                    .entry(key.clone())
                    .and_modify(|c| *c = c.saturating_add(1))
                    .or_insert(1);
                if *cycles >= 3 {
                    // Re-check the current on-disk team config before pruning.
                    // Config watcher updates can race with reconcile snapshots; if
                    // the member has been re-added, skip prune and reset cycle count.
                    let member_present =
                        is_member_present_in_config(&config_path, &tracked.agent_name)
                            .unwrap_or_else(|e| {
                                warn!(
                                    "stale-prune guard: failed to re-check config {}: {e}",
                                    config_path.display()
                                );
                                true
                            });
                    if member_present {
                        absent_cycles.remove(&key);
                        continue;
                    }
                    reg.remove_for_team(&team_name, &tracked.agent_name);
                    absent_cycles.remove(&key);
                }
            }
        }

        if changed {
            write_team_config_atomic(&config_path, &config)?;
        }
    }

    // Remove state entries for members no longer present in team configs.
    //
    // Use a fresh on-disk read here rather than the local per-pass snapshot.
    // Multiple reconcile passes can overlap (dispatch-triggered + periodic), so
    // using stale in-memory desired names can unregister members that were
    // re-added by a concurrent pass (TOCTOU race).
    {
        let fresh_desired_agent_names = match build_desired_agent_names(&teams_root) {
            Ok(names) => names,
            Err(e) => {
                warn!("cleanup desired-member refresh failed; falling back to local snapshot: {e}");
                desired_agent_names
            }
        };

        let mut tracker = state_store.lock().unwrap();
        let tracked: Vec<String> = tracker.all_states().into_keys().collect();
        for name in tracked {
            if !fresh_desired_agent_names.contains(&name) {
                tracker.unregister_agent(&name);
            }
        }
    }

    Ok(())
}

fn delete_member_inbox(team_dir: &std::path::Path, agent_name: &str) -> Result<()> {
    let inbox_path = team_dir.join("inboxes").join(format!("{agent_name}.json"));
    if !inbox_path.exists() {
        return Ok(());
    }
    std::fs::remove_file(&inbox_path)?;
    Ok(())
}

fn write_team_config_atomic(path: &std::path::Path, config: &TeamConfig) -> Result<()> {
    let lock_path = path.with_extension("lock");
    let _lock = acquire_lock(&lock_path, 5)
        .map_err(|e| anyhow::anyhow!("failed to acquire config lock: {e}"))?;

    let tmp = path.with_extension("tmp");
    let serialized = serde_json::to_string_pretty(config)?;
    std::fs::write(&tmp, serialized)?;
    atomic_swap(path, &tmp)?;
    Ok(())
}

fn build_desired_agent_names(
    teams_root: &std::path::Path,
) -> Result<std::collections::HashSet<String>> {
    let mut desired = std::collections::HashSet::new();
    for entry in std::fs::read_dir(teams_root)? {
        let Ok(entry) = entry else { continue };
        let team_dir = entry.path();
        if !team_dir.is_dir() {
            continue;
        }
        let config_path = team_dir.join("config.json");
        if !config_path.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&config_path)?;
        let config: TeamConfig = serde_json::from_str(&content)?;
        for member in config.members {
            desired.insert(member.name);
        }
    }
    Ok(desired)
}

fn is_member_present_in_config(config_path: &std::path::Path, member_name: &str) -> Result<bool> {
    let content = std::fs::read_to_string(config_path)?;
    let config: TeamConfig = serde_json::from_str(&content)?;
    Ok(config
        .members
        .iter()
        .any(|member| member.name == member_name))
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
    let inbox_msgs: Vec<agent_team_mail_core::schema::InboxMessage> =
        serde_json::from_str(&content)?;
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
fn extract_hostname_registry(
    config: &agent_team_mail_core::config::Config,
) -> Option<std::sync::Arc<agent_team_mail_core::config::HostnameRegistry>> {
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
        .plugin_config("gh_monitor")
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
    use agent_team_mail_core::retention::parse_duration;
    use agent_team_mail_core::retention::{apply_retention, clean_report_files};

    // Enumerate team directories
    let team_dirs = match std::fs::read_dir(teams_root) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::error!(
                "Failed to read teams directory {}: {}",
                teams_root.display(),
                e
            );
            emit_event_best_effort(EventFields {
                level: "error",
                source: "atm-daemon",
                action: "retention_dir_read_error",
                error: Some(format!("Failed to read teams directory: {e}")),
                ..Default::default()
            });
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
                tracing::warn!(
                    "Failed to read inboxes directory {}: {}",
                    inboxes_path.display(),
                    e
                );
                emit_event_best_effort(EventFields {
                    level: "error",
                    source: "atm-daemon",
                    action: "retention_inbox_read_error",
                    team: Some(team_name.clone()),
                    error: Some(format!("Failed to read inboxes directory: {e}")),
                    ..Default::default()
                });
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
            tracing::debug!(
                "Applying retention to {}/{}/{}",
                team_name,
                agent_name,
                file_name
            );
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
                            team_name,
                            agent_name,
                            file_name,
                            result.kept,
                            result.removed,
                            result.archived
                        );
                        emit_event_best_effort(EventFields {
                            level: "info",
                            source: "atm-daemon",
                            action: "retention_applied",
                            team: Some(team_name.clone()),
                            agent_id: Some(agent_name.clone()),
                            count: Some(result.removed as u64),
                            result: Some("ok".to_string()),
                            ..Default::default()
                        });
                    }
                }
                Err(e) => {
                    tracing::error!(
                        "Retention failed for {}/{}/{}: {}",
                        team_name,
                        agent_name,
                        file_name,
                        e
                    );
                    emit_event_best_effort(EventFields {
                        level: "error",
                        source: "atm-daemon",
                        action: "retention_error",
                        team: Some(team_name.clone()),
                        agent_id: Some(agent_name.clone()),
                        error: Some(format!("Retention failed: {e}")),
                        ..Default::default()
                    });
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
                        result.deleted_count,
                        result.skipped_count
                    );
                    emit_event_best_effort(EventFields {
                        level: "info",
                        source: "atm-daemon",
                        action: "report_cleanup",
                        count: Some(result.deleted_count as u64),
                        result: Some("ok".to_string()),
                        ..Default::default()
                    });
                }
            }
            Err(e) => {
                tracing::error!("Report cleanup failed: {}", e);
                emit_event_best_effort(EventFields {
                    level: "error",
                    source: "atm-daemon",
                    action: "report_cleanup_error",
                    error: Some(format!("Report cleanup failed: {e}")),
                    ..Default::default()
                });
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
    let plugin_statuses = build_plugin_statuses(&plugins, &ctx).await;
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

                let plugin_statuses = build_plugin_statuses(&plugins, &ctx).await;
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
    ctx: &PluginContext,
) -> Vec<PluginStatus> {
    let mut statuses = Vec::new();

    for (metadata, _plugin_arc) in plugins {
        let mut status_kind = PluginStatusKind::Running;
        let mut last_error = None;
        let mut last_updated = Some(format_timestamp(SystemTime::now()));
        let mut enabled = true;

        // GH monitor plugin lifecycle/availability projection:
        // daemon status surfaces should expose healthy|degraded|disabled_config_error
        // through existing PluginStatusKind fields.
        if matches!(metadata.name, "gh_monitor" | "ci_monitor")
            && let Some((kind, error, updated)) = gh_monitor_plugin_status_projection(ctx)
        {
            status_kind = kind;
            last_error = error;
            last_updated = updated.or_else(|| Some(format_timestamp(SystemTime::now())));
            enabled = !matches!(status_kind, PluginStatusKind::Disabled);
        }

        statuses.push(PluginStatus {
            name: metadata.name.to_string(),
            enabled,
            status: status_kind,
            last_error,
            last_updated,
        });
    }

    statuses
}

fn gh_monitor_plugin_status_projection(
    ctx: &PluginContext,
) -> Option<(PluginStatusKind, Option<String>, Option<String>)> {
    let home_dir = ctx.system.claude_root.parent()?.to_path_buf();
    let path = home_dir.join(".claude/daemon/gh-monitor-health.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&raw).ok()?;
    let records = value.get("records")?.as_array()?;
    let team = &ctx.config.core.default_team;
    let record = records
        .iter()
        .find(|r| r.get("team").and_then(|t| t.as_str()) == Some(team.as_str()))
        .or_else(|| records.first())?;

    let availability = record
        .get("availability_state")
        .and_then(|v| v.as_str())
        .unwrap_or("healthy");
    let message = record
        .get("message")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let updated = record
        .get("updated_at")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let kind = match availability {
        "healthy" => PluginStatusKind::Running,
        "degraded" => PluginStatusKind::Error,
        "disabled_config_error" => PluginStatusKind::Disabled,
        _ => PluginStatusKind::Running,
    };
    Some((kind, message, updated))
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

    let duration = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = duration.as_secs();
    let nanos = duration.subsec_nanos();

    let dt = DateTime::<Utc>::from_timestamp(secs as i64, nanos).unwrap_or_else(Utc::now);
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::{InboxCursor, read_new_inbox_messages, reconcile_team_member_activity};
    use crate::daemon::session_registry::new_session_registry;
    use crate::daemon::socket::new_state_store;
    use crate::plugins::worker_adapter::AgentState;
    use agent_team_mail_core::schema::InboxMessage;
    use serial_test::serial;
    use std::collections::HashMap;
    use std::fs as stdfs;
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

    fn write_team_config(home: &std::path::Path, team: &str, members: serde_json::Value) {
        let team_dir = home.join(".claude/teams").join(team);
        stdfs::create_dir_all(&team_dir).unwrap();
        let cfg = serde_json::json!({
            "name": team,
            "createdAt": 1739284800000u64,
            "leadAgentId": format!("team-lead@{team}"),
            "leadSessionId": "lead-session",
            "members": members,
        });
        stdfs::write(
            team_dir.join("config.json"),
            serde_json::to_string_pretty(&cfg).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn test_reconcile_seeds_state_store_from_config() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let cwd = home.display().to_string();
        write_team_config(
            home,
            "atm-dev",
            serde_json::json!([
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd.clone(),
                    "subscriptions": [],
                    "isActive": true
                },
                {
                    "agentId": "qa@atm-dev",
                    "name": "qa",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd.clone(),
                    "subscriptions": []
                }
            ]),
        );

        let sr = new_session_registry();
        let state_store = new_state_store();
        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();

        let tracker = state_store.lock().unwrap();
        assert_eq!(tracker.get_state("team-lead"), Some(AgentState::Offline));
        assert_eq!(tracker.get_state("qa"), Some(AgentState::Offline));
    }

    #[test]
    fn test_reconcile_removes_deleted_member_from_state_store() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let cwd = home.display().to_string();
        write_team_config(
            home,
            "atm-dev",
            serde_json::json!([
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd.clone(),
                    "subscriptions": [],
                    "isActive": true
                },
                {
                    "agentId": "worker@atm-dev",
                    "name": "worker",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd.clone(),
                    "subscriptions": [],
                    "isActive": true
                }
            ]),
        );
        let sr = new_session_registry();
        let state_store = new_state_store();
        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();
        assert!(state_store.lock().unwrap().get_state("worker").is_some());

        // Remove worker from config and reconcile again.
        write_team_config(
            home,
            "atm-dev",
            serde_json::json!([
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd.clone(),
                    "subscriptions": [],
                    "isActive": true
                }
            ]),
        );
        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();
        assert!(state_store.lock().unwrap().get_state("worker").is_none());
    }

    #[test]
    fn test_reconcile_marks_missing_session_member_inactive_after_restore() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let cwd = home.display().to_string();
        write_team_config(
            home,
            "atm-dev",
            serde_json::json!([
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd.clone(),
                    "subscriptions": [],
                    "isActive": true
                },
                {
                    "agentId": "arch-gtm@atm-dev",
                    "name": "arch-gtm",
                    "agentType": "gemini",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd.clone(),
                    "subscriptions": [],
                    "isActive": true
                }
            ]),
        );

        let sr = new_session_registry();
        let state_store = new_state_store();
        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();

        let cfg: agent_team_mail_core::schema::TeamConfig = serde_json::from_str(
            &stdfs::read_to_string(home.join(".claude/teams/atm-dev/config.json")).unwrap(),
        )
        .unwrap();
        let restored = cfg
            .members
            .iter()
            .find(|m| m.name == "arch-gtm")
            .expect("member present");
        assert_eq!(restored.is_active, Some(false));
    }

    #[test]
    fn test_reconcile_keeps_pid_backend_mismatch_offline_within_same_pass() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let cwd = home.display().to_string();
        let live_pid = std::process::id();
        write_team_config(
            home,
            "atm-dev",
            serde_json::json!([
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd.clone(),
                    "subscriptions": [],
                    "isActive": true
                },
                {
                    "agentId": "arch-ctm@atm-dev",
                    "name": "arch-ctm",
                    "agentType": "codex",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd.clone(),
                    "subscriptions": [],
                    "isActive": true,
                    "sessionId": "hint-session",
                    "processId": live_pid,
                    "externalBackendType": "codex"
                }
            ]),
        );

        let sr = new_session_registry();
        {
            let mut reg = sr.lock().unwrap();
            reg.upsert_for_team("atm-dev", "arch-ctm", "stale-session", live_pid);
        }
        let state_store = new_state_store();
        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();

        let tracker = state_store.lock().unwrap();
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Offline));
        let transition = tracker
            .transition_meta("arch-ctm")
            .expect("transition metadata should be present");
        assert_eq!(transition.source, "pid_backend_validation");

        let cfg: agent_team_mail_core::schema::TeamConfig = serde_json::from_str(
            &stdfs::read_to_string(home.join(".claude/teams/atm-dev/config.json")).unwrap(),
        )
        .unwrap();
        let member = cfg
            .members
            .iter()
            .find(|m| m.name == "arch-ctm")
            .expect("member present");
        assert_eq!(member.is_active, Some(false));
    }

    #[test]
    #[serial]
    fn test_reconcile_does_not_auto_promote_dead_session_with_live_pid() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let cwd = home.display().to_string();
        let live_pid = std::process::id();

        write_team_config(
            home,
            "atm-dev",
            serde_json::json!([
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd.clone(),
                    "subscriptions": [],
                    "isActive": true
                },
                {
                    "agentId": "arch-ctm@atm-dev",
                    "name": "arch-ctm",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd,
                    "subscriptions": [],
                    "isActive": true
                }
            ]),
        );

        let sr = new_session_registry();
        {
            let mut reg = sr.lock().unwrap();
            reg.upsert_for_team("atm-dev", "arch-ctm", "dead-live-pid", live_pid);
            reg.mark_dead_for_team("atm-dev", "arch-ctm");
        }

        let state_store = new_state_store();
        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();

        let tracker = state_store.lock().unwrap();
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Offline));
        drop(tracker);

        let cfg: agent_team_mail_core::schema::TeamConfig = serde_json::from_str(
            &stdfs::read_to_string(home.join(".claude/teams/atm-dev/config.json")).unwrap(),
        )
        .unwrap();
        let member = cfg
            .members
            .iter()
            .find(|m| m.name == "arch-ctm")
            .expect("member present");
        assert_eq!(member.is_active, Some(false));

        let reg = sr.lock().unwrap();
        let record = reg.query_for_team("atm-dev", "arch-ctm").unwrap();
        assert_eq!(
            record.state,
            crate::daemon::session_registry::SessionState::Dead
        );
    }

    fn assert_terminal_non_lead_cleanup(
        home: &std::path::Path,
        team_name: &str,
        inbox_dir: &std::path::Path,
    ) {
        let cfg: agent_team_mail_core::schema::TeamConfig = serde_json::from_str(
            &stdfs::read_to_string(home.join(format!(".claude/teams/{team_name}/config.json")))
                .unwrap(),
        )
        .unwrap();
        assert!(
            cfg.members.iter().all(|m| m.name != "arch-ctm"),
            "terminal non-lead should be removed from roster"
        );
        assert!(
            !inbox_dir.join("arch-ctm.json").exists(),
            "terminal non-lead inbox should be removed"
        );
    }

    fn unique_test_team_name() -> String {
        let now_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("atm-dev-{now_nanos}")
    }

    fn setup_dead_terminal_non_lead() -> (
        TempDir,
        std::path::PathBuf,
        String,
        super::SharedSessionRegistry,
        super::SharedStateStore,
    ) {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let cwd = home.display().to_string();
        let team_name = unique_test_team_name();
        let inbox_dir = home.join(format!(".claude/teams/{team_name}/inboxes"));
        stdfs::create_dir_all(&inbox_dir).unwrap();
        stdfs::write(inbox_dir.join("arch-ctm.json"), "[]").unwrap();
        write_team_config(
            home,
            &team_name,
            serde_json::json!([
                {
                    "agentId": format!("team-lead@{}", team_name),
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd.clone(),
                    "subscriptions": [],
                    "isActive": true
                },
                {
                    "agentId": format!("arch-ctm@{}", team_name),
                    "name": "arch-ctm",
                    "agentType": "codex",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd.clone(),
                    "subscriptions": [],
                    "isActive": true
                }
            ]),
        );

        let sr = new_session_registry();
        let state_store = new_state_store();
        (tmp, inbox_dir, team_name, sr, state_store)
    }

    #[test]
    #[serial]
    fn test_session_end_converges_to_remove_dead_member_from_roster_and_mailbox() {
        super::ABSENT_REGISTRY_CYCLES.lock().unwrap().clear();
        super::DEAD_MEMBER_CYCLES.lock().unwrap().clear();
        let (tmp, inbox_dir, team_name, sr, state_store) = setup_dead_terminal_non_lead();
        let home = tmp.path();
        {
            let mut reg = sr.lock().unwrap();
            reg.upsert_for_team(&team_name, "arch-ctm", "sess-session-end", i32::MAX as u32);
            // Simulate hook_watcher SessionEnd processing.
            reg.mark_dead_for_team(&team_name, "arch-ctm");
        }
        // Two cycles required: first cycle increments dead counter; second fires terminal cleanup.
        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();
        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();
        assert_terminal_non_lead_cleanup(home, &team_name, &inbox_dir);
        assert!(
            sr.lock()
                .unwrap()
                .query_for_team(&team_name, "arch-ctm")
                .is_none(),
            "terminal non-lead session record should be removed"
        );
    }

    #[test]
    #[serial]
    fn test_sigterm_escalation_converges_to_remove_dead_member_from_roster_and_mailbox() {
        super::ABSENT_REGISTRY_CYCLES.lock().unwrap().clear();
        super::DEAD_MEMBER_CYCLES.lock().unwrap().clear();
        let (tmp, inbox_dir, team_name, sr, state_store) = setup_dead_terminal_non_lead();
        let home = tmp.path();
        {
            let mut reg = sr.lock().unwrap();
            reg.upsert_for_team(&team_name, "arch-ctm", "sess-sigterm", i32::MAX as u32);
            // Simulate daemon --kill escalation where SIGTERM eventually marks the session dead.
            reg.mark_dead_for_team(&team_name, "arch-ctm");
        }
        // Two cycles required: first cycle increments dead counter; second fires terminal cleanup.
        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();
        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();
        assert_terminal_non_lead_cleanup(home, &team_name, &inbox_dir);
        assert!(
            sr.lock()
                .unwrap()
                .query_for_team(&team_name, "arch-ctm")
                .is_none(),
            "terminal non-lead session record should be removed"
        );
    }

    #[test]
    #[serial]
    fn test_kill_timeout_fallback_converges_to_remove_dead_member_from_roster_and_mailbox() {
        super::ABSENT_REGISTRY_CYCLES.lock().unwrap().clear();
        super::DEAD_MEMBER_CYCLES.lock().unwrap().clear();
        let (tmp, inbox_dir, team_name, sr, state_store) = setup_dead_terminal_non_lead();
        let home = tmp.path();
        {
            let mut reg = sr.lock().unwrap();
            reg.upsert_for_team(&team_name, "arch-ctm", "sess-kill-timeout", i32::MAX as u32);
            // Simulate daemon --kill exhausting graceful waits and forcing termination.
            reg.mark_dead_for_team(&team_name, "arch-ctm");
        }
        // Two cycles required: first cycle increments dead counter; second fires terminal cleanup.
        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();
        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();
        assert_terminal_non_lead_cleanup(home, &team_name, &inbox_dir);
        assert!(
            sr.lock()
                .unwrap()
                .query_for_team(&team_name, "arch-ctm")
                .is_none(),
            "terminal non-lead session record should be removed"
        );
    }

    #[test]
    #[serial]
    fn test_reconcile_prunes_stale_absent_dead_members_only_after_two_full_extra_cycles() {
        super::ABSENT_REGISTRY_CYCLES.lock().unwrap().clear();
        super::DEAD_MEMBER_CYCLES.lock().unwrap().clear();

        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let cwd = home.display().to_string();
        stdfs::create_dir_all(home.join(".claude/teams/atm-dev/inboxes")).unwrap();
        write_team_config(
            home,
            "atm-dev",
            serde_json::json!([
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd,
                    "subscriptions": [],
                    "isActive": true
                }
            ]),
        );

        let sr = new_session_registry();
        let state_store = new_state_store();
        {
            let mut reg = sr.lock().unwrap();
            reg.upsert_for_team("atm-dev", "arch-ctm", "stale-sess", i32::MAX as u32);
            reg.mark_dead_for_team("atm-dev", "arch-ctm");
        }

        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();
        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "arch-ctm")
                .is_some(),
            "first absent cycle should not prune dead member yet"
        );

        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();
        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "arch-ctm")
                .is_some(),
            "second absent cycle should still not prune dead member"
        );

        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();
        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "arch-ctm")
                .is_none(),
            "third absent cycle should prune stale dead member"
        );
    }

    #[test]
    #[serial]
    fn test_reconcile_does_not_prune_absent_active_sessions() {
        super::ABSENT_REGISTRY_CYCLES.lock().unwrap().clear();
        super::DEAD_MEMBER_CYCLES.lock().unwrap().clear();

        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let cwd = home.display().to_string();
        stdfs::create_dir_all(home.join(".claude/teams/atm-dev/inboxes")).unwrap();
        write_team_config(
            home,
            "atm-dev",
            serde_json::json!([
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd,
                    "subscriptions": [],
                    "isActive": true
                }
            ]),
        );

        let sr = new_session_registry();
        let state_store = new_state_store();
        {
            let mut reg = sr.lock().unwrap();
            reg.upsert_for_team("atm-dev", "arch-ctm", "active-sess", std::process::id());
        }

        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();
        reconcile_team_member_activity(&home.join(".claude"), &sr, &state_store).unwrap();

        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "arch-ctm")
                .is_some(),
            "active absent sessions must never be stale-pruned"
        );
    }

    #[test]
    #[serial]
    fn test_reconcile_dispatch_mode_remove_then_readd_preserves_dead_session_record() {
        super::ABSENT_REGISTRY_CYCLES.lock().unwrap().clear();
        super::DEAD_MEMBER_CYCLES.lock().unwrap().clear();

        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let cwd = home.display().to_string();
        stdfs::create_dir_all(home.join(".claude/teams/atm-dev/inboxes")).unwrap();
        write_team_config(
            home,
            "atm-dev",
            serde_json::json!([
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd,
                    "subscriptions": [],
                    "isActive": true
                }
            ]),
        );

        let sr = new_session_registry();
        let state_store = new_state_store();
        {
            let mut reg = sr.lock().unwrap();
            reg.upsert_for_team("atm-dev", "arch-ctm", "dead-sess", i32::MAX as u32);
            reg.mark_dead_for_team("atm-dev", "arch-ctm");
        }

        // Step 1: member present in config -> reconcile seeds state store and
        // must clear any prior absence tracking.
        write_team_config(
            home,
            "atm-dev",
            serde_json::json!([
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": home.display().to_string(),
                    "subscriptions": [],
                    "isActive": true
                },
                {
                    "agentId": "arch-ctm@atm-dev",
                    "name": "arch-ctm",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": home.display().to_string(),
                    "subscriptions": [],
                    "isActive": true
                }
            ]),
        );
        super::reconcile_team_member_activity_with_mode(
            &home.join(".claude"),
            &sr,
            &state_store,
            true,
        )
        .unwrap();
        assert!(
            state_store.lock().unwrap().get_state("arch-ctm").is_some(),
            "member present in config should be seeded in state store"
        );
        assert!(
            !super::ABSENT_REGISTRY_CYCLES
                .lock()
                .unwrap()
                .contains_key("atm-dev:arch-ctm"),
            "presence should clear stale absent-cycle markers"
        );

        // Step 2: member removed from config -> cycle should advance absent
        // tracking but not prune dead record yet.
        write_team_config(
            home,
            "atm-dev",
            serde_json::json!([
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": home.display().to_string(),
                    "subscriptions": [],
                    "isActive": true
                }
            ]),
        );
        super::reconcile_team_member_activity_with_mode(
            &home.join(".claude"),
            &sr,
            &state_store,
            true,
        )
        .unwrap();
        assert_eq!(
            super::ABSENT_REGISTRY_CYCLES
                .lock()
                .unwrap()
                .get("atm-dev:arch-ctm")
                .copied(),
            Some(1),
            "first absent cycle should be recorded when member is removed"
        );
        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "arch-ctm")
                .is_some(),
            "dead session record must not be pruned on first absence cycle"
        );

        // Step 3: member re-added -> reconcile must keep dead session record and
        // clear absent-cycle marker.
        write_team_config(
            home,
            "atm-dev",
            serde_json::json!([
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": home.display().to_string(),
                    "subscriptions": [],
                    "isActive": true
                },
                {
                    "agentId": "arch-ctm@atm-dev",
                    "name": "arch-ctm",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": home.display().to_string(),
                    "subscriptions": [],
                    "isActive": true
                }
            ]),
        );

        super::reconcile_team_member_activity_with_mode(
            &home.join(".claude"),
            &sr,
            &state_store,
            true,
        )
        .unwrap();

        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "arch-ctm")
                .is_some(),
            "dead session record must not be stale-pruned after member is re-added"
        );
        assert!(
            state_store.lock().unwrap().get_state("arch-ctm").is_some(),
            "re-added member must remain registered in state store"
        );
        assert!(
            !super::ABSENT_REGISTRY_CYCLES
                .lock()
                .unwrap()
                .contains_key("atm-dev:arch-ctm"),
            "re-added member should clear absent-cycle marker"
        );
    }

    #[test]
    #[serial]
    fn test_reconcile_config_dispatch_mode_does_not_advance_absent_prune_cycles() {
        super::ABSENT_REGISTRY_CYCLES.lock().unwrap().clear();
        super::DEAD_MEMBER_CYCLES.lock().unwrap().clear();

        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let cwd = home.display().to_string();
        stdfs::create_dir_all(home.join(".claude/teams/atm-dev/inboxes")).unwrap();
        write_team_config(
            home,
            "atm-dev",
            serde_json::json!([
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "unknown",
                    "joinedAt": 1,
                    "cwd": cwd,
                    "subscriptions": [],
                    "isActive": true
                }
            ]),
        );

        let sr = new_session_registry();
        let state_store = new_state_store();
        {
            let mut reg = sr.lock().unwrap();
            reg.upsert_for_team("atm-dev", "arch-ctm", "dead-sess", i32::MAX as u32);
            reg.mark_dead_for_team("atm-dev", "arch-ctm");
        }

        for _ in 0..5 {
            super::reconcile_team_member_activity_with_mode(
                &home.join(".claude"),
                &sr,
                &state_store,
                false,
            )
            .unwrap();
        }

        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "arch-ctm")
                .is_some(),
            "dispatch-mode reconcile must not advance stale-prune absent cycles"
        );
    }
}
