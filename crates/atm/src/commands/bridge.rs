//! Bridge command implementation

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::util::settings::get_home_dir;

/// Bridge metrics (subset needed for CLI display)
#[derive(Debug, Serialize, Deserialize, Default)]
struct BridgeMetrics {
    total_syncs: u64,
    total_pushed: u64,
    total_pulled: u64,
    total_errors: u64,
    last_sync_time: Option<u64>,
    #[serde(default)]
    remote_failures: HashMap<String, u64>,
    #[serde(default)]
    disabled_remotes: Vec<String>,
}

impl BridgeMetrics {
    fn get_remote_failures(&self, remote: &str) -> u64 {
        self.remote_failures.get(remote).copied().unwrap_or(0)
    }
}

/// Bridge plugin commands
#[derive(Args, Debug)]
pub struct BridgeArgs {
    #[command(subcommand)]
    command: BridgeCommand,
}

#[derive(Subcommand, Debug)]
enum BridgeCommand {
    /// Show bridge status and metrics
    Status(BridgeStatusArgs),

    /// Trigger an immediate sync cycle
    Sync(BridgeSyncArgs),
}

#[derive(Args, Debug)]
struct BridgeStatusArgs {
    /// Team name (optional, uses default team if not specified)
    team: Option<String>,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct BridgeSyncArgs {
    /// Team name (optional, uses default team if not specified)
    team: Option<String>,
}

/// Execute the bridge command
pub fn execute(args: BridgeArgs) -> Result<()> {
    match args.command {
        BridgeCommand::Status(status_args) => execute_status(status_args),
        BridgeCommand::Sync(sync_args) => execute_sync(sync_args),
    }
}

fn execute_status(args: BridgeStatusArgs) -> Result<()> {
    use atm_core::config::{resolve_config, ConfigOverrides};

    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    // Resolve configuration to get default team
    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };
    let config = resolve_config(&overrides, &current_dir, &home_dir)?;
    let team_name = &config.core.default_team;

    // Load bridge metrics
    let team_dir = home_dir.join(".claude/teams").join(team_name);
    let metrics_path = team_dir.join(".bridge-metrics.json");

    let metrics = if metrics_path.exists() {
        let content = std::fs::read_to_string(&metrics_path)
            .context("Failed to read bridge metrics")?;
        serde_json::from_str(&content).context("Failed to parse bridge metrics")?
    } else {
        // No metrics file yet - bridge not initialized or never synced
        BridgeMetrics::default()
    };

    // Load bridge config from main config
    let bridge_config = config.plugin_config("bridge");

    if args.json {
        let output = serde_json::json!({
            "team": team_name,
            "enabled": bridge_config.is_some(),
            "metrics": metrics,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Bridge Status for team: {team_name}");
        println!();

        if bridge_config.is_none() {
            println!("Bridge plugin: Not configured");
            return Ok(());
        }

        println!("Bridge plugin: Configured");
        println!();

        println!("Metrics:");
        println!("  Total sync cycles: {}", metrics.total_syncs);
        println!("  Total messages pushed: {}", metrics.total_pushed);
        println!("  Total messages pulled: {}", metrics.total_pulled);
        println!("  Total errors: {}", metrics.total_errors);

        if let Some(last_sync) = metrics.last_sync_time {
            let elapsed = format_elapsed_ms(last_sync);
            println!("  Last sync: {elapsed}");
        } else {
            println!("  Last sync: Never");
        }

        if !metrics.disabled_remotes.is_empty() {
            println!();
            println!("Disabled remotes (circuit breaker):");
            for remote in &metrics.disabled_remotes {
                let failures = metrics.get_remote_failures(remote);
                println!("  {remote} ({failures} consecutive failures)");
            }
        }
    }

    Ok(())
}

fn execute_sync(args: BridgeSyncArgs) -> Result<()> {
    use atm_core::config::{resolve_config, ConfigOverrides};

    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    // Resolve configuration to get default team
    let overrides = ConfigOverrides {
        team: args.team,
        ..Default::default()
    };
    let config = resolve_config(&overrides, &current_dir, &home_dir)?;
    let team_name = &config.core.default_team;

    // Check if bridge is configured
    if config.plugin_config("bridge").is_none() {
        anyhow::bail!("Bridge plugin not configured for team '{team_name}'");
    }

    println!("Triggering sync cycle for team: {team_name}");
    println!();

    // Trigger sync by touching a sentinel file that the daemon watches
    let team_dir = home_dir.join(".claude/teams").join(team_name);
    let sync_trigger_path = team_dir.join(".bridge-sync-trigger");

    std::fs::create_dir_all(&team_dir)?;
    std::fs::write(&sync_trigger_path, format!("{}", current_time_ms()))?;

    println!("Sync trigger sent. Check daemon logs for sync results.");
    println!();
    println!("Run `atm bridge status` to view metrics after sync completes.");

    Ok(())
}

/// Format elapsed time since timestamp (milliseconds)
fn format_elapsed_ms(timestamp_ms: u64) -> String {
    let now_ms = current_time_ms();
    if timestamp_ms > now_ms {
        return "in the future".to_string();
    }

    let elapsed_ms = now_ms - timestamp_ms;
    let elapsed_secs = elapsed_ms / 1000;

    if elapsed_secs < 60 {
        format!("{elapsed_secs} seconds ago")
    } else if elapsed_secs < 3600 {
        let minutes = elapsed_secs / 60;
        format!("{minutes} minute{} ago", if minutes == 1 { "" } else { "s" })
    } else if elapsed_secs < 86400 {
        let hours = elapsed_secs / 3600;
        format!("{hours} hour{} ago", if hours == 1 { "" } else { "s" })
    } else {
        let days = elapsed_secs / 86400;
        format!("{days} day{} ago", if days == 1 { "" } else { "s" })
    }
}

/// Get current time in milliseconds since epoch
fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
