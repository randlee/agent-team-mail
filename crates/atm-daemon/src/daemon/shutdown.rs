//! Graceful shutdown coordination for plugins

use crate::plugin::{PluginMetadata, SharedPlugin};
use anyhow::Result;
use std::time::Duration;
use tokio::time::timeout;
use tracing::{error, info, warn};

/// Perform graceful shutdown of all plugins.
///
/// Calls shutdown() on each plugin with a timeout. If any plugin exceeds the
/// timeout, it's logged as a warning and shutdown continues for remaining plugins.
///
/// # Arguments
///
/// * `plugins` - Vec of (metadata, plugin) pairs to shut down
/// * `shutdown_timeout` - Maximum time to wait for each plugin to shut down
pub async fn graceful_shutdown(
    plugins: Vec<(PluginMetadata, SharedPlugin)>,
    shutdown_timeout: Duration,
) -> Result<()> {
    info!(
        "Beginning graceful shutdown of {} plugin(s) (timeout: {:?})",
        plugins.len(),
        shutdown_timeout
    );

    let mut success_count = 0;
    let mut timeout_count = 0;
    let mut error_count = 0;

    for (metadata, plugin_arc) in plugins {
        let plugin_name = metadata.name.to_string();
        info!("Shutting down plugin: {}", plugin_name);

        let mut plugin = plugin_arc.lock().await;

        match timeout(shutdown_timeout, plugin.shutdown()).await {
            Ok(Ok(())) => {
                info!("Plugin {} shut down cleanly", plugin_name);
                success_count += 1;
            }
            Ok(Err(e)) => {
                error!("Plugin {} shutdown failed: {}", plugin_name, e);
                error_count += 1;
            }
            Err(_) => {
                warn!(
                    "Plugin {} shutdown timed out after {:?}",
                    plugin_name, shutdown_timeout
                );
                timeout_count += 1;
            }
        }
    }

    info!(
        "Graceful shutdown complete: {} success, {} timeout, {} error",
        success_count, timeout_count, error_count
    );

    if error_count > 0 {
        anyhow::bail!("{error_count} plugin(s) failed to shut down cleanly");
    }

    // Attempt final spool drain before exiting
    // TODO: Get inbox_base from context or config
    // For now, skip this - it requires access to the teams root path

    Ok(())
}
