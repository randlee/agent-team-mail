//! Periodic spool drain task for guaranteed message delivery

use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

/// Run a periodic spool drain loop until cancelled.
///
/// Calls atm_core::io::spool_drain() on the given inbox base directory
/// at regular intervals. This ensures that any spooled messages (from lock
/// contention) are eventually delivered.
///
/// # Arguments
///
/// * `inbox_base` - Base directory for team inboxes (usually ~/.claude/teams)
/// * `interval_duration` - How often to run the drain (e.g., Duration::from_secs(10))
/// * `cancel` - Cancellation token to stop the loop
pub async fn spool_drain_loop(
    inbox_base: PathBuf,
    interval_duration: Duration,
    cancel: CancellationToken,
) -> Result<()> {
    info!("Starting spool drain loop (interval: {:?})", interval_duration);
    let mut ticker = interval(interval_duration);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                debug!("Running spool drain");
                match atm_core::io::spool_drain(&inbox_base) {
                    Ok(status) => {
                        if status.delivered > 0 || status.failed > 0 {
                            info!(
                                "Spool drain complete: delivered={}, pending={}, failed={}",
                                status.delivered, status.pending, status.failed
                            );
                        } else {
                            debug!(
                                "Spool drain complete: delivered={}, pending={}, failed={}",
                                status.delivered, status.pending, status.failed
                            );
                        }
                    }
                    Err(e) => {
                        error!("Spool drain failed: {}", e);
                    }
                }
            }
            _ = cancel.cancelled() => {
                info!("Spool drain loop cancelled");
                break;
            }
        }
    }

    Ok(())
}
