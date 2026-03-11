//! Daemon management commands

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::io::inbox::inbox_append;
use agent_team_mail_core::schema::InboxMessage;
use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Args, Subcommand};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};
use uuid::Uuid;

use crate::commands::logging_health::LoggingHealthSnapshot;
use crate::util::settings::get_home_dir;
use agent_team_mail_core::daemon_client::daemon_status_path_for;

/// Daemon management commands
#[derive(Args, Debug)]
pub struct DaemonArgs {
    /// Kill the named agent process via daemon session tracking
    #[arg(long, value_name = "AGENT")]
    kill: Option<String>,

    /// Team override for --kill
    #[arg(long, requires = "kill")]
    team: Option<String>,

    /// Wait timeout in seconds for graceful shutdown before forced termination
    #[arg(long, default_value_t = 10, requires = "kill")]
    timeout: u64,

    #[command(subcommand)]
    command: Option<DaemonCommands>,
}

#[derive(Subcommand, Debug)]
enum DaemonCommands {
    /// Show daemon status
    Status(StatusArgs),
    /// Stop the running daemon gracefully
    Stop(StopArgs),
    /// Restart the daemon (stop then autostart)
    Restart(RestartArgs),
}

/// Stop the running daemon
#[derive(Args, Debug)]
pub struct StopArgs {
    /// Wait timeout in seconds for graceful shutdown (default 10)
    #[arg(long, default_value_t = 10)]
    timeout: u64,
}

/// Restart the daemon (stop then autostart)
#[derive(Args, Debug)]
pub struct RestartArgs {
    /// Wait timeout in seconds for graceful shutdown before restart (default 10)
    #[arg(long, default_value_t = 10)]
    timeout: u64,
}

/// Show daemon status
#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Output as JSON
    #[arg(long)]
    json: bool,
}

/// Execute daemon command
pub fn execute(args: DaemonArgs) -> Result<()> {
    if let Some(agent) = args.kill.as_deref() {
        return execute_kill(agent, args.team.as_deref(), args.timeout.max(1));
    }

    match args
        .command
        .unwrap_or(DaemonCommands::Status(StatusArgs { json: false }))
    {
        DaemonCommands::Status(status_args) => execute_status(status_args),
        DaemonCommands::Stop(stop_args) => execute_stop(stop_args.timeout.max(1)),
        DaemonCommands::Restart(restart_args) => execute_restart(restart_args.timeout.max(1)),
    }
}

fn execute_kill(agent: &str, team_override: Option<&str>, timeout_secs: u64) -> Result<()> {
    if !agent_team_mail_core::daemon_client::daemon_is_running() {
        ensure_daemon_running().context("failed to auto-start daemon for --kill")?;
    }
    if !agent_team_mail_core::daemon_client::daemon_is_running() {
        anyhow::bail!("daemon is not running");
    }

    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;
    let config = resolve_config(
        &ConfigOverrides {
            team: team_override.map(ToString::to_string),
            ..Default::default()
        },
        &current_dir,
        &home_dir,
    )?;
    let team_name = team_override.unwrap_or(&config.core.default_team);

    let Some(info) = agent_team_mail_core::daemon_client::query_session_for_team(team_name, agent)?
    else {
        anyhow::bail!("no daemon session record found for {agent}@{team_name}");
    };

    if !info.alive {
        crate::commands::teams::cleanup_single_agent(
            team_name.to_string(),
            agent.to_string(),
            true,
        )?;
        println!("Agent {agent}@{team_name} already not alive; teardown cleanup completed");
        return Ok(());
    }

    let pid = validated_signal_pid(info.process_id).ok_or_else(|| {
        anyhow::anyhow!(
            "refusing to signal invalid pid {} for {}@{}",
            info.process_id,
            agent,
            team_name
        )
    })?;

    send_shutdown_request(&home_dir, team_name, agent)?;
    if wait_for_session_dead(team_name, agent, timeout_secs) {
        crate::commands::teams::cleanup_single_agent(
            team_name.to_string(),
            agent.to_string(),
            true,
        )?;
        println!("Graceful shutdown + teardown cleanup complete for {agent}@{team_name}");
        return Ok(());
    }

    #[cfg(unix)]
    {
        // SAFETY: SIGINT requests graceful interrupt of the target process.
        let _ = unsafe { libc::kill(pid, libc::SIGINT) };
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("forced termination not supported on this platform");
    }

    if wait_for_session_dead(team_name, agent, 10) {
        crate::commands::teams::cleanup_single_agent(
            team_name.to_string(),
            agent.to_string(),
            true,
        )?;
        println!("SIGINT termination + teardown cleanup complete for {agent}@{team_name}");
        Ok(())
    } else {
        #[cfg(unix)]
        {
            // SAFETY: SIGTERM requests cooperative shutdown after SIGINT timeout.
            let _ = unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        if wait_for_session_dead(team_name, agent, 10) {
            crate::commands::teams::cleanup_single_agent(
                team_name.to_string(),
                agent.to_string(),
                true,
            )?;
            println!("SIGTERM termination + teardown cleanup complete for {agent}@{team_name}");
            Ok(())
        } else {
            #[cfg(unix)]
            {
                // SAFETY: SIGKILL force-terminates process that ignored prior shutdown attempts.
                let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            if wait_for_session_dead(team_name, agent, 3) {
                crate::commands::teams::cleanup_single_agent(
                    team_name.to_string(),
                    agent.to_string(),
                    true,
                )?;
                println!("SIGKILL termination + teardown cleanup complete for {agent}@{team_name}");
                Ok(())
            } else {
                anyhow::bail!("failed to terminate {agent}@{team_name} within timeout")
            }
        }
    }
}

fn validated_signal_pid(pid: u32) -> Option<i32> {
    if pid > 1 && pid <= i32::MAX as u32 {
        Some(pid as i32)
    } else {
        None
    }
}

fn ensure_daemon_running() -> Result<()> {
    // Trigger daemon autostart through the daemon query path used by published
    // atm-core APIs, then verify liveness.
    let _ = agent_team_mail_core::daemon_client::query_list_agents();
    if agent_team_mail_core::daemon_client::daemon_is_running() {
        Ok(())
    } else {
        anyhow::bail!("daemon is not running")
    }
}

fn send_shutdown_request(
    home_dir: &std::path::Path,
    team_name: &str,
    agent_name: &str,
) -> Result<()> {
    let payload = serde_json::json!({
        "type": "shutdown_request",
        "requestId": Uuid::new_v4().to_string(),
        "from": "atm",
        "reason": "daemon --kill",
        "timestamp": Utc::now().to_rfc3339(),
    });
    let msg = InboxMessage {
        from: "atm".to_string(),
        text: payload.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        read: false,
        summary: Some("shutdown_request".to_string()),
        message_id: Some(Uuid::new_v4().to_string()),
        unknown_fields: HashMap::new(),
    };
    let inbox_path = home_dir
        .join(".claude/teams")
        .join(team_name)
        .join("inboxes")
        .join(format!("{agent_name}.json"));
    inbox_append(&inbox_path, &msg, team_name, "atm")?;
    Ok(())
}

/// Stop the running daemon by sending SIGTERM to the PID recorded in the PID file.
///
/// On non-Unix platforms this is a no-op (returns `Ok(())`).
fn execute_stop(timeout_secs: u64) -> Result<()> {
    #[cfg(unix)]
    {
        let pid_path = agent_team_mail_core::daemon_client::daemon_pid_path()
            .context("failed to resolve daemon PID file path")?;

        if !pid_path.exists() {
            println!(
                "Daemon is not running (no PID file at {}).",
                pid_path.display()
            );
            return Ok(());
        }

        let content = std::fs::read_to_string(&pid_path)
            .with_context(|| format!("failed to read daemon PID file {}", pid_path.display()))?;
        let pid: i32 = content.trim().parse().with_context(|| {
            format!(
                "daemon PID file {} contains non-integer content",
                pid_path.display()
            )
        })?;

        if pid <= 1 {
            anyhow::bail!(
                "daemon PID file contains invalid PID {}; refusing to send signal",
                pid
            );
        }

        // SAFETY: SIGTERM requests cooperative shutdown of the daemon process.
        let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                println!("Daemon process {pid} no longer exists; cleaning up PID file.");
                let _ = std::fs::remove_file(&pid_path);
                return Ok(());
            }
            return Err(anyhow::anyhow!(
                "failed to send SIGTERM to daemon process {pid}: {err}"
            ));
        }

        println!("Sent SIGTERM to daemon (PID {pid}); waiting up to {timeout_secs}s for exit...");

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        while Instant::now() < deadline {
            // SAFETY: kill(pid, 0) checks if the process is alive without sending a signal.
            let alive = unsafe { libc::kill(pid, 0) == 0 };
            if !alive {
                println!("Daemon (PID {pid}) has stopped.");
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        println!("Daemon (PID {pid}) did not stop within {timeout_secs}s after SIGTERM.");
        println!("You may force-kill it with: kill -9 {pid}");
        std::process::exit(1);
    }

    #[cfg(not(unix))]
    {
        eprintln!("atm daemon stop is not supported on this platform.");
        std::process::exit(1);
    }
}

/// Restart the daemon: stop the running instance then trigger autostart.
fn execute_restart(timeout_secs: u64) -> Result<()> {
    execute_stop(timeout_secs)?;
    // Brief pause to let socket and PID files be cleaned up before autostart.
    std::thread::sleep(Duration::from_millis(500));
    agent_team_mail_core::daemon_client::ensure_daemon_running()
        .context("failed to autostart daemon after stop")?;
    if agent_team_mail_core::daemon_client::daemon_is_running() {
        println!("Daemon restarted successfully.");
    } else {
        anyhow::bail!("daemon did not come back online after restart");
    }
    Ok(())
}

fn wait_for_session_dead(team_name: &str, agent_name: &str, timeout_secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        match agent_team_mail_core::daemon_client::query_session_for_team(team_name, agent_name) {
            Ok(Some(info)) if !info.alive => return true,
            Ok(None) => return true,
            Ok(Some(_)) => {}
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Execute daemon status command
fn execute_status(args: StatusArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let status_path = daemon_status_path_for(&home_dir);

    // Check if status file exists
    if !status_path.exists() {
        if args.json {
            println!("{{\"error\": \"No daemon status found. Is the daemon running?\"}}");
        } else {
            eprintln!("No daemon status found. Is the daemon running?");
            eprintln!("Status file not found: {}", status_path.display());
        }
        std::process::exit(1);
    }

    // Read and parse status file
    let content =
        std::fs::read_to_string(&status_path).context("Failed to read daemon status file")?;

    let status: DaemonStatus =
        serde_json::from_str(&content).context("Failed to parse daemon status file")?;

    // Check if status is stale (timestamp older than 2x poll interval = 60 seconds)
    let stale_threshold_secs = 60;
    let is_stale = is_status_stale(&status.timestamp, stale_threshold_secs);

    if args.json {
        // Output as JSON with stale flag
        let mut output = serde_json::to_value(&status)?;
        if let Some(obj) = output.as_object_mut() {
            obj.insert("stale".to_string(), serde_json::Value::Bool(is_stale));
        }
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        // Human-readable output
        println!("Daemon Status");
        println!("=============");
        println!("PID:         {}", status.pid);
        println!("Version:     {}", status.version);
        println!("Uptime:      {}", format_duration(status.uptime_secs));
        println!("Last update: {}", status.timestamp);

        if is_stale {
            println!();
            println!(
                "WARNING: Daemon status is stale (last update > {}s ago)",
                stale_threshold_secs
            );
            println!("         The daemon may not be running.");
        }

        if !status.teams.is_empty() {
            println!();
            println!("Teams ({}):", status.teams.len());
            for team in &status.teams {
                println!("  - {}", team);
            }
        }

        if !status.plugins.is_empty() {
            println!();
            println!("Plugins ({}):", status.plugins.len());
            for plugin in &status.plugins {
                let status_str = match plugin.status {
                    PluginStatusKind::Running => "running",
                    PluginStatusKind::Error => "error",
                    PluginStatusKind::Disabled => "disabled",
                    PluginStatusKind::DisabledInitError => "disabled_init_error",
                };

                let enabled_str = if plugin.enabled {
                    "enabled"
                } else {
                    "disabled"
                };

                print!("  {} - {} ({})", plugin.name, status_str, enabled_str);

                if let Some(ref error) = plugin.last_error {
                    print!(" - Error: {}", error);
                }

                if let Some(ref last_updated) = plugin.last_updated {
                    print!(" - Last updated: {}", last_updated);
                }

                println!();
            }
        }

        println!();
        println!("Logging:");
        println!("  state:           {}", status.logging.state);
        println!("  dropped_counter: {}", status.logging.dropped_counter);
        println!("  spool_path:      {}", status.logging.spool_path);
        println!(
            "  canonical_log_path: {}",
            status.logging.canonical_log_path
        );
        println!("  spool_count:     {}", status.logging.spool_count);
        if let Some(oldest_spool_age) = status.logging.oldest_spool_age {
            println!("  oldest_spool_age: {oldest_spool_age}s");
        }
        if let Some(last_error) = &status.logging.last_error {
            println!("  last_error:      {last_error}");
        }
    }

    // Exit with error code if stale
    if is_stale {
        std::process::exit(1);
    }

    Ok(())
}

/// Check if status timestamp is stale
fn is_status_stale(timestamp: &str, threshold_secs: u64) -> bool {
    use chrono::DateTime;
    use std::time::UNIX_EPOCH;

    let parsed = match DateTime::parse_from_rfc3339(timestamp) {
        Ok(dt) => dt,
        Err(_) => return true, // If we can't parse, assume stale
    };

    let status_time = UNIX_EPOCH + Duration::from_secs(parsed.timestamp() as u64);
    let now = SystemTime::now();

    match now.duration_since(status_time) {
        Ok(elapsed) => elapsed.as_secs() > threshold_secs,
        Err(_) => true, // Clock skew or future timestamp
    }
}

/// Format duration as human-readable string
fn format_duration(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if days > 0 {
        format!("{}d {}h {}m {}s", days, hours, minutes, seconds)
    } else if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

// Re-export types from daemon crate for status file parsing
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DaemonStatus {
    timestamp: String,
    pid: u32,
    version: String,
    uptime_secs: u64,
    plugins: Vec<PluginStatus>,
    teams: Vec<String>,
    #[serde(default)]
    logging: LoggingHealthSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PluginStatus {
    name: String,
    enabled: bool,
    status: PluginStatusKind,
    last_error: Option<String>,
    last_updated: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PluginStatusKind {
    Running,
    Error,
    Disabled,
    #[serde(rename = "disabled_init_error")]
    DisabledInitError,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(30), "30s");
        assert_eq!(format_duration(90), "1m 30s");
        assert_eq!(format_duration(3661), "1h 1m 1s");
        assert_eq!(format_duration(86400), "1d 0h 0m 0s");
        assert_eq!(format_duration(90061), "1d 1h 1m 1s");
    }

    #[test]
    fn test_is_status_stale_fresh() {
        use chrono::Utc;

        let now = Utc::now();
        let timestamp = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        assert!(!is_status_stale(&timestamp, 60));
    }

    #[test]
    fn test_is_status_stale_old() {
        use chrono::{Duration as ChronoDuration, Utc};

        let old = Utc::now() - ChronoDuration::seconds(120);
        let timestamp = old.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        assert!(is_status_stale(&timestamp, 60));
    }

    #[test]
    fn test_is_status_stale_invalid() {
        assert!(is_status_stale("not-a-timestamp", 60));
    }
}
