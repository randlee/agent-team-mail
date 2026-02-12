//! Main daemon event loop

use crate::daemon::{graceful_shutdown, spool_drain_loop, watch_inboxes};
use crate::plugin::{PluginContext, PluginRegistry};
use anyhow::{Context, Result};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

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

    // Start file system watcher
    let watcher_root = ctx.mail.teams_root().clone();
    let watcher_cancel = cancel.clone();
    let watcher_task = tokio::spawn(async move {
        if let Err(e) = watch_inboxes(watcher_root, watcher_cancel).await {
            error!("Inbox watcher failed: {}", e);
        }
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
