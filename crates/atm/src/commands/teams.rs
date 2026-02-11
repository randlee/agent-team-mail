//! Teams command implementation

use anyhow::Result;
use atm_core::schema::TeamConfig;
use chrono::{DateTime, Utc};
use clap::Args;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

use crate::util::settings::get_home_dir;

/// List all teams on this machine
#[derive(Args, Debug)]
pub struct TeamsArgs {
    /// Output as JSON
    #[arg(long)]
    json: bool,
}

/// Team summary information
#[derive(Debug)]
struct TeamSummary {
    name: String,
    member_count: usize,
    created_at: u64,
}

/// Execute the teams command
pub fn execute(args: TeamsArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let teams_dir = home_dir.join(".claude/teams");

    // Check if teams directory exists
    if !teams_dir.exists() {
        if args.json {
            println!("{}", json!({"teams": []}));
        } else {
            let teams_path = teams_dir.display();
            println!("No teams found (directory {teams_path} doesn't exist)");
        }
        return Ok(());
    }

    // Scan for teams
    let mut teams = Vec::new();

    for entry in fs::read_dir(&teams_dir)? {
        let entry = entry?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let config_path = path.join("config.json");
        if !config_path.exists() {
            continue;
        }

        // Try to read team config
        match read_team_config(&config_path) {
            Ok(config) => {
                teams.push(TeamSummary {
                    name: config.name,
                    member_count: config.members.len(),
                    created_at: config.created_at,
                });
            }
            Err(e) => {
                let path_display = path.display();
                eprintln!("Warning: Failed to read config for {path_display}: {e}");
            }
        }
    }

    // Sort teams by name
    teams.sort_by(|a, b| a.name.cmp(&b.name));

    // Output results
    if args.json {
        let output = json!({
            "teams": teams.iter().map(|t| json!({
                "name": t.name,
                "memberCount": t.member_count,
                "createdAt": t.created_at,
            })).collect::<Vec<_>>()
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if teams.is_empty() {
        println!("No teams found");
    } else {
        println!("Teams:");
        for team in &teams {
            let age = format_age(team.created_at);
            let name = &team.name;
            let count = team.member_count;
            println!("  {name:20}  {count} members    Created {age}");
        }
    }

    Ok(())
}

/// Read team config from file
fn read_team_config(path: &PathBuf) -> Result<TeamConfig> {
    let content = fs::read_to_string(path)?;
    let config: TeamConfig = serde_json::from_str(&content)?;
    Ok(config)
}

/// Format age as human-readable string (e.g., "2 days ago")
fn format_age(timestamp_ms: u64) -> String {
    let created = DateTime::from_timestamp((timestamp_ms / 1000) as i64, 0);

    match created {
        Some(created_dt) => {
            let now = Utc::now();
            let duration = now.signed_duration_since(created_dt);

            let days = duration.num_days();
            let hours = duration.num_hours();
            let minutes = duration.num_minutes();

            if days > 0 {
                if days == 1 {
                    "1 day ago".to_string()
                } else {
                    format!("{days} days ago")
                }
            } else if hours > 0 {
                if hours == 1 {
                    "1 hour ago".to_string()
                } else {
                    format!("{hours} hours ago")
                }
            } else if minutes > 0 {
                if minutes == 1 {
                    "1 minute ago".to_string()
                } else {
                    format!("{minutes} minutes ago")
                }
            } else {
                "just now".to_string()
            }
        }
        None => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_age() {
        // Test with a timestamp from 1 day ago
        let now = Utc::now();
        let one_day_ago = now - chrono::Duration::days(1);
        let timestamp = (one_day_ago.timestamp() * 1000) as u64;

        let age = format_age(timestamp);
        assert!(age.contains("day"));
    }
}
