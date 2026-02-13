//! Main daemon event loop

use crate::daemon::{graceful_shutdown, spool_drain_loop, watch_inboxes, InboxEvent, InboxEventKind};
use crate::plugin::{Capability, PluginContext, PluginRegistry};
use anyhow::{Context, Result};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Run the main daemon event loop.
///
/// This function:
/// 1. Initializes all plugins via the registry
/// 2. Spawns each plugin's run() method in its own task
/// 3. Starts the spool drain background task
/// 4. Starts the file system watcher
/// 5. Waits for cancellation signal
/// 6. Performs graceful shutdown of all plugins
///
/// # Arguments
///
/// * `registry` - Mutable plugin registry (plugins will be taken out for task spawning)
/// * `ctx` - Shared plugin context
/// * `cancel` - Cancellation token for shutdown coordination
pub async fn run(
    registry: &mut PluginRegistry,
    ctx: &PluginContext,
    cancel: CancellationToken,
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

    // Start file system watcher
    let watcher_root = ctx.mail.teams_root().clone();
    let watcher_cancel = cancel.clone();
    let watcher_task = tokio::spawn(async move {
        if let Err(e) = watch_inboxes(watcher_root, event_tx, watcher_cancel).await {
            error!("Inbox watcher failed: {}", e);
        }
    });

    // Start event dispatch loop for EventListener plugins
    let dispatch_plugins = plugins.clone();
    let dispatch_cancel = cancel.clone();
    let dispatch_task = tokio::spawn(async move {
        info!("Starting event dispatch loop");
        loop {
            tokio::select! {
                _ = dispatch_cancel.cancelled() => {
                    info!("Event dispatch cancelled");
                    break;
                }
                Some(event) = event_rx.recv() => {
                    debug!("Dispatching event: team={}, agent={}, kind={:?}",
                           event.team, event.agent, event.kind);

                    // Only dispatch MessageReceived events
                    if event.kind != InboxEventKind::MessageReceived {
                        continue;
                    }

                    // Read inbox messages (array) and dispatch the newest message
                    let inbox_msgs: Vec<atm_core::schema::InboxMessage> =
                        match tokio::fs::read_to_string(&event.path).await {
                            Ok(content) => match serde_json::from_str(&content) {
                                Ok(msgs) => msgs,
                                Err(e) => {
                                    warn!(
                                        "Failed to parse inbox at {}: {}",
                                        event.path.display(),
                                        e
                                    );
                                    continue;
                                }
                            },
                            Err(e) => {
                                // File might be transiently locked or deleted
                                debug!(
                                    "Failed to read inbox file at {}: {}",
                                    event.path.display(),
                                    e
                                );
                                continue;
                            }
                        };

                    let inbox_msg = match inbox_msgs.last() {
                        Some(msg) => msg,
                        None => continue,
                    };

                    // Dispatch to all plugins with EventListener capability
                    for (metadata, plugin_arc) in &dispatch_plugins {
                        if metadata.capabilities.contains(&Capability::EventListener) {
                            // Try to acquire lock without blocking
                            if let Ok(mut plugin) = plugin_arc.try_lock() {
                                debug!("Dispatching to plugin: {}", metadata.name);
                                if let Err(e) = plugin.handle_message(inbox_msg).await {
                                    error!("Plugin {} handle_message error: {}", metadata.name, e);
                                }
                            } else {
                                debug!("Plugin {} is busy, skipping event dispatch", metadata.name);
                            }
                        }
                    }
                }
            }
        }
        info!("Event dispatch loop stopped");
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
