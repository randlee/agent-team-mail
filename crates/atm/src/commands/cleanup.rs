//! Cleanup command implementation - apply retention policies to inboxes

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::io::inbox::inbox_append;
use agent_team_mail_core::retention::apply_retention;
use agent_team_mail_core::schema::{InboxMessage, TeamConfig};
use anyhow::{Context, Result};
use chrono::Utc;
use clap::Args;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::commands::teams;
use crate::util::settings::get_home_dir;

/// Apply retention policies to clean up old messages
#[derive(Args, Debug)]
pub struct CleanupArgs {
    /// Team name (uses default if not specified)
    #[arg(short, long)]
    team: Option<String>,

    /// Remove stale state for a specific agent (compatibility alias for teams cleanup)
    #[arg(long)]
    agent: Option<String>,

    /// Apply to all teams
    #[arg(long)]
    all_teams: bool,

    /// Show what would be cleaned without modifying
    #[arg(long)]
    dry_run: bool,

    /// Force cleanup when daemon liveness checks are unavailable (agent mode only)
    #[arg(long)]
    force: bool,

    /// Wait timeout in seconds for graceful shutdown (agent mode only)
    #[arg(long, default_value_t = 10)]
    timeout: u64,
}

/// Execute the cleanup command
pub fn execute(args: CleanupArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };

    let config = resolve_config(&overrides, &current_dir, &home_dir)?;

    // Agent cleanup compatibility mode:
    // `atm cleanup --agent <name> [--team <team>] [--force]`
    if let Some(agent) = &args.agent {
        if args.all_teams {
            anyhow::bail!("--agent cannot be combined with --all-teams");
        }
        if args.dry_run {
            anyhow::bail!("--dry-run is not supported with --agent");
        }
        let team_name = args
            .team
            .clone()
            .unwrap_or_else(|| config.core.default_team.clone());
        return execute_agent_cleanup(
            &home_dir,
            &team_name,
            agent,
            args.force,
            args.timeout.max(1),
        );
    }

    let teams_dir = home_dir.join(".claude/teams");
    if !teams_dir.exists() {
        let display = teams_dir.display();
        anyhow::bail!("Teams directory not found at {display}");
    }

    // Check if retention policy is configured
    if config.retention.max_age.is_none() && config.retention.max_count.is_none() {
        println!(
            "No retention policy configured. Set retention.max_age and/or retention.max_count in .atm.toml"
        );
        return Ok(());
    }

    if args.dry_run {
        println!("DRY RUN - no files will be modified\n");
    }

    if args.all_teams {
        // Apply to all teams
        let entries = std::fs::read_dir(&teams_dir)?;
        let mut team_names: Vec<String> = Vec::new();

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir()
                && let Some(name_str) = path.file_name().and_then(|n| n.to_str())
            {
                team_names.push(name_str.to_string());
            }
        }

        team_names.sort();

        for team_name in team_names {
            cleanup_team(&home_dir, &team_name, &config.retention, args.dry_run)?;
        }
    } else {
        // Apply to single team
        let team_name = &config.core.default_team;
        cleanup_team(&home_dir, team_name, &config.retention, args.dry_run)?;
    }

    Ok(())
}

fn execute_agent_cleanup(
    home_dir: &Path,
    team_name: &str,
    agent_name: &str,
    force: bool,
    timeout_secs: u64,
) -> Result<()> {
    if agent_name == "team-lead" {
        anyhow::bail!("team-lead is protected and cannot be removed by cleanup");
    }

    if agent_team_mail_core::daemon_client::daemon_is_running() {
        if let Ok(Some(info)) = agent_team_mail_core::daemon_client::query_session_for_team(
            team_name, agent_name,
        ) && info.alive
        {
            send_shutdown_request(home_dir, team_name, agent_name)?;

            if !wait_for_session_dead(team_name, agent_name, timeout_secs) {
                #[cfg(unix)]
                {
                    let _ = unsafe { libc::kill(info.process_id as libc::pid_t, libc::SIGTERM) };
                }
                #[cfg(not(unix))]
                {
                    eprintln!(
                        "Warning: forced process termination is not supported on this platform"
                    );
                }
            }
        }
    } else if !force {
        anyhow::bail!(
            "daemon is not running; cannot safely confirm liveness for '{}'. Start daemon or re-run with --force",
            agent_name
        );
    }

    teams::cleanup_single_agent(team_name.to_string(), agent_name.to_string(), force)
}

fn send_shutdown_request(home_dir: &Path, team_name: &str, agent_name: &str) -> Result<()> {
    let request_id = Uuid::new_v4().to_string();
    let shutdown_payload = serde_json::json!({
        "type": "shutdown_request",
        "requestId": request_id,
        "from": "atm",
        "reason": "cleanup --agent",
        "timestamp": Utc::now().to_rfc3339(),
    });

    let msg = InboxMessage {
        from: "atm".to_string(),
        text: shutdown_payload.to_string(),
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

/// Clean up a single team's inboxes
fn cleanup_team(
    home_dir: &Path,
    team_name: &str,
    retention_config: &agent_team_mail_core::config::RetentionConfig,
    dry_run: bool,
) -> Result<()> {
    let team_dir = home_dir.join(".claude/teams").join(team_name);

    if !team_dir.exists() {
        println!("Team '{team_name}' not found, skipping");
        return Ok(());
    }

    // Load team config to get member list
    let team_config_path = team_dir.join("config.json");
    if !team_config_path.exists() {
        println!("Team '{team_name}' has no config.json, skipping");
        return Ok(());
    }

    let team_config: TeamConfig =
        serde_json::from_str(&std::fs::read_to_string(&team_config_path)?)
            .with_context(|| format!("Failed to parse team config for '{team_name}'"))?;

    println!("Team: {team_name}\n");
    println!(
        "  {:<20} {:>8} {:>8} {:>10}",
        "Agent", "Kept", "Removed", "Archived"
    );
    println!("  {}", "─".repeat(50));

    let mut total_kept = 0;
    let mut total_removed = 0;
    let mut total_archived = 0;

    // Apply retention to each agent's inbox (local files)
    for member in &team_config.members {
        let inbox_path = team_dir
            .join("inboxes")
            .join(format!("{}.json", member.name));

        if !inbox_path.exists() {
            // Skip agents with no inbox file
            continue;
        }

        let result = apply_retention(
            &inbox_path,
            team_name,
            &member.name,
            retention_config,
            dry_run,
        )?;

        // Only show agents where something happened
        if result.removed > 0 || result.kept > 0 {
            println!(
                "  {:<20} {:>8} {:>8} {:>10}",
                member.name, result.kept, result.removed, result.archived
            );

            total_kept += result.kept;
            total_removed += result.removed;
            total_archived += result.archived;
        }
    }

    // Also clean up per-origin inbox files (format: agent.hostname.json)
    let inboxes_dir = team_dir.join("inboxes");
    if inboxes_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&inboxes_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let Some(filename) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };

            // Per-origin files have format: agent.hostname.json
            // Skip if it's a local file (no dots in stem)
            if !filename.ends_with(".json") {
                continue;
            }

            let stem = filename.strip_suffix(".json").unwrap();
            if !stem.contains('.') {
                // This is a local file, already processed above
                continue;
            }

            // Extract agent name and hostname
            // Format: agent-name.hostname.json
            let parts: Vec<_> = stem.rsplitn(2, '.').collect();
            if parts.len() != 2 {
                continue;
            }

            let hostname = parts[0];
            let agent_name = parts[1];
            let display_name = format!("{agent_name}@{hostname}");

            let result =
                apply_retention(&path, team_name, &display_name, retention_config, dry_run)?;

            if result.removed > 0 || result.kept > 0 {
                println!(
                    "  {:<20} {:>8} {:>8} {:>10}",
                    display_name, result.kept, result.removed, result.archived
                );

                total_kept += result.kept;
                total_removed += result.removed;
                total_archived += result.archived;
            }
        }
    }

    if total_kept == 0 && total_removed == 0 {
        println!("  (no messages in any inbox)");
    } else {
        println!("  {}", "─".repeat(50));
        println!(
            "  {:<20} {:>8} {:>8} {:>10}",
            "TOTAL", total_kept, total_removed, total_archived
        );
    }

    println!();

    Ok(())
}
