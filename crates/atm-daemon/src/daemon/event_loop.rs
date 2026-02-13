//! Main daemon event loop

use crate::daemon::{graceful_shutdown, spool_drain_loop, watch_inboxes, InboxEvent, InboxEventKind};
use crate::plugin::{Capability, PluginContext, PluginRegistry};
use anyhow::{Context, Result};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

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

                    let inbox_msg = match read_latest_inbox_message(&event.path).await {
                        Ok(Some(msg)) => msg,
                        Ok(None) => continue,
                        Err(e) => {
                            // File might be transiently locked, deleted, or malformed
                            debug!(
                                "Failed to read latest inbox message at {}: {}",
                                event.path.display(),
                                e
                            );
                            continue;
                        }
                    };

                    // Dispatch to all plugins with EventListener capability
                    for (metadata, plugin_arc) in &dispatch_plugins {
                        if metadata.capabilities.contains(&Capability::EventListener) {
                            // Try to acquire lock without blocking
                            if let Ok(mut plugin) = plugin_arc.try_lock() {
                                debug!("Dispatching to plugin: {}", metadata.name);
                                if let Err(e) = plugin.handle_message(&inbox_msg).await {
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

async fn read_latest_inbox_message(
    path: &std::path::Path,
) -> Result<Option<atm_core::schema::InboxMessage>> {
    let content = tokio::fs::read_to_string(path).await?;
    let inbox_msgs: Vec<atm_core::schema::InboxMessage> = serde_json::from_str(&content)?;
    Ok(inbox_msgs.last().cloned())
}

#[cfg(test)]
mod tests {
    use super::read_latest_inbox_message;
    use atm_core::schema::InboxMessage;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_read_latest_inbox_message_returns_last() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("inbox.json");

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

        let content = serde_json::to_string_pretty(&vec![msg1.clone(), msg2.clone()]).unwrap();
        tokio::fs::write(&path, content).await.unwrap();

        let latest = read_latest_inbox_message(&path).await.unwrap();
        assert_eq!(latest.unwrap().message_id, msg2.message_id);
    }

    #[tokio::test]
    async fn test_read_latest_inbox_message_empty_returns_none() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("inbox.json");
        tokio::fs::write(&path, "[]").await.unwrap();

        let latest = read_latest_inbox_message(&path).await.unwrap();
        assert!(latest.is_none());
    }
}
