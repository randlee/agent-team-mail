//! Inbox command implementation - show inbox summaries

use anyhow::Result;
use atm_core::config::{resolve_config, ConfigOverrides};
use atm_core::schema::{InboxMessage, TeamConfig};
use chrono::DateTime;
use clap::Args;
use std::path::Path;

use crate::util::settings::get_home_dir;

/// Show inbox summary for team members
#[derive(Args, Debug)]
pub struct InboxArgs {
    /// Override default team
    #[arg(long)]
    team: Option<String>,

    /// Show summary across all teams
    #[arg(long)]
    all_teams: bool,
}

/// Execute the inbox command
pub fn execute(args: InboxArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };

    let config = resolve_config(&overrides, &current_dir, &home_dir)?;

    let teams_dir = home_dir.join(".claude/teams");
    if !teams_dir.exists() {
        anyhow::bail!("Teams directory not found at {teams_dir:?}");
    }

    if args.all_teams {
        // Show summary for all teams
        let entries = std::fs::read_dir(&teams_dir)?;
        let mut team_names: Vec<String> = Vec::new();

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name() {
                    if let Some(name_str) = name.to_str() {
                        team_names.push(name_str.to_string());
                    }
                }
            }
        }

        team_names.sort();

        for team_name in team_names {
            show_team_summary(&home_dir, &team_name)?;
            println!();
        }
    } else {
        // Show summary for single team
        let team_name = &config.core.default_team;
        show_team_summary(&home_dir, team_name)?;
    }

    Ok(())
}

/// Show inbox summary for a single team
fn show_team_summary(home_dir: &Path, team_name: &str) -> Result<()> {
    let team_dir = home_dir.join(".claude/teams").join(team_name);

    if !team_dir.exists() {
        println!("Team: {team_name} (not found)");
        return Ok(());
    }

    // Load team config
    let team_config_path = team_dir.join("config.json");
    if !team_config_path.exists() {
        println!("Team: {team_name} (config not found)");
        return Ok(());
    }

    let team_config: TeamConfig = serde_json::from_str(&std::fs::read_to_string(&team_config_path)?)?;

    println!("Team: {team_name}\n");
    println!("  {:<20} {:>8} {:>8} {:>12}", "Agent", "Unread", "Total", "Latest");
    println!("  {}", "─".repeat(52));

    // Collect agent summaries
    let mut summaries = Vec::new();
    for member in &team_config.members {
        let inbox_path = team_dir.join("inboxes").join(format!("{}.json", member.name));

        let (unread, total, latest) = if inbox_path.exists() {
            let content = std::fs::read_to_string(&inbox_path)?;
            let messages: Vec<InboxMessage> = serde_json::from_str(&content)?;

            let unread_count = messages.iter().filter(|m| !m.read).count();
            let total_count = messages.len();
            let latest_time = messages
                .last()
                .map(|m| format_relative_time(&m.timestamp))
                .unwrap_or_else(|| "-".to_string());

            (unread_count, total_count, latest_time)
        } else {
            (0, 0, "-".to_string())
        };

        summaries.push((member.name.clone(), unread, total, latest));
    }

    // Display summaries
    for (agent_name, unread, total, latest) in summaries {
        println!("  {agent_name:<20} {unread:>8} {total:>8} {latest:>12}");
    }

    Ok(())
}

/// Format timestamp as relative time (e.g., "2m ago", "1h ago")
fn format_relative_time(timestamp_str: &str) -> String {
    let timestamp = DateTime::parse_from_rfc3339(timestamp_str).ok();
    if let Some(ts) = timestamp {
        let now = chrono::Utc::now();
        let duration = now.signed_duration_since(ts.with_timezone(&chrono::Utc));

        if duration.num_seconds() < 0 {
            "future".to_string()
        } else if duration.num_seconds() < 60 {
            format!("{}s ago", duration.num_seconds())
        } else if duration.num_minutes() < 60 {
            format!("{}m ago", duration.num_minutes())
        } else if duration.num_hours() < 24 {
            format!("{}h ago", duration.num_hours())
        } else {
            format!("{}d ago", duration.num_days())
        }
    } else {
        "unknown".to_string()
    }
}
