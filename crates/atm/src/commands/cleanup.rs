//! Cleanup command implementation - apply retention policies to inboxes

use anyhow::{Context, Result};
use agent_team_mail_core::config::{resolve_config, ConfigOverrides};
use agent_team_mail_core::retention::apply_retention;
use agent_team_mail_core::schema::TeamConfig;
use clap::Args;
use std::path::Path;

use crate::util::settings::get_home_dir;

/// Apply retention policies to clean up old messages
#[derive(Args, Debug)]
pub struct CleanupArgs {
    /// Team name (uses default if not specified)
    #[arg(short, long)]
    team: Option<String>,

    /// Apply to all teams
    #[arg(long)]
    all_teams: bool,

    /// Show what would be cleaned without modifying
    #[arg(long)]
    dry_run: bool,
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

    let teams_dir = home_dir.join(".claude/teams");
    if !teams_dir.exists() {
        let display = teams_dir.display();
        anyhow::bail!("Teams directory not found at {display}");
    }

    // Check if retention policy is configured
    if config.retention.max_age.is_none() && config.retention.max_count.is_none() {
        println!("No retention policy configured. Set retention.max_age and/or retention.max_count in .atm.toml");
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

    let team_config: TeamConfig = serde_json::from_str(&std::fs::read_to_string(&team_config_path)?)
        .with_context(|| format!("Failed to parse team config for '{team_name}'"))?;

    println!("Team: {team_name}\n");
    println!("  {:<20} {:>8} {:>8} {:>10}", "Agent", "Kept", "Removed", "Archived");
    println!("  {}", "─".repeat(50));

    let mut total_kept = 0;
    let mut total_removed = 0;
    let mut total_archived = 0;

    // Apply retention to each agent's inbox (local files)
    for member in &team_config.members {
        let inbox_path = team_dir.join("inboxes").join(format!("{}.json", member.name));

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
    if inboxes_dir.exists() && let Ok(entries) = std::fs::read_dir(&inboxes_dir) {
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

            let result = apply_retention(
                &path,
                team_name,
                &display_name,
                retention_config,
                dry_run,
            )?;

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
