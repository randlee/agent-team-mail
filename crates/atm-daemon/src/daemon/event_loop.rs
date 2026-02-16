//! Main daemon event loop

use crate::daemon::{graceful_shutdown, spool_drain_loop, watch_inboxes, InboxEvent, InboxEventKind};
use crate::plugin::{Capability, PluginContext, PluginRegistry};
use anyhow::{Context, Result};
use serde_json::Value;
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
