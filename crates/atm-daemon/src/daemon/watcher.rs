//! File system watcher for inbox directories

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Watch team inbox directories for changes.
///
/// This sets up a file system watcher on the teams root directory and logs
/// all file system events. In the future, this can dispatch events to plugins
/// that have registered for file change notifications.
///
/// # Arguments
///
/// * `teams_root` - Root directory containing team inboxes (e.g., ~/.claude/teams)
/// * `cancel` - Cancellation token to stop watching
pub async fn watch_inboxes(teams_root: PathBuf, cancel: CancellationToken) -> Result<()> {
    info!("Starting inbox watcher for: {}", teams_root.display());

    // Create a channel to receive file system events
    let (tx, rx) = channel();

    // Create the watcher
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        match res {
            Ok(event) => {
                if let Err(e) = tx.send(event) {
                    error!("Failed to send file system event: {}", e);
                }
            }
            Err(e) => {
                error!("File system watcher error: {}", e);
            }
        }
    })
    .context("Failed to create file system watcher")?;

    // Start watching the teams root directory recursively
    watcher
        .watch(&teams_root, RecursiveMode::Recursive)
        .context("Failed to watch teams directory")?;

    info!("Watching {} for changes", teams_root.display());

    // Event processing loop
    // Spawn a blocking task to handle the synchronous mpsc receiver
    let cancel_clone = cancel.clone();
    tokio::task::spawn_blocking(move || {
        loop {
            if cancel_clone.is_cancelled() {
                info!("Inbox watcher cancelled");
                break;
            }

            // Use try_recv to avoid blocking indefinitely
            match rx.try_recv() {
                Ok(event) => {
                    debug!("File system event: {:?}", event);
                    // TODO: Dispatch to plugins based on event type and capabilities
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // No events, sleep briefly to avoid busy loop
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    warn!("Watcher channel disconnected");
                    break;
                }
            }
        }
    })
    .await
    .context("Watcher task panicked")?;

    Ok(())
}
