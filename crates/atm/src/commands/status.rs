//! Status command implementation

use anyhow::Result;
use agent_team_mail_core::config::{resolve_config, ConfigOverrides};
use agent_team_mail_core::schema::{InboxMessage, TeamConfig};
use clap::Args;
use serde_json::json;
use std::collections::HashMap;
use std::fs;

use crate::util::settings::get_home_dir;

/// Show combined team overview
#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Team name (optional, uses default team if not specified)
    team: Option<String>,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

/// Execute the status command
pub fn execute(args: StatusArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    // Resolve configuration to get default team
    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };
    let config = resolve_config(&overrides, &current_dir, &home_dir)?;
    let team_name = &config.core.default_team;

    // Load team config
    let team_dir = home_dir.join(".claude/teams").join(team_name);
    if !team_dir.exists() {
        anyhow::bail!("Team '{team_name}' not found (directory {team_dir:?} doesn't exist)");
    }

    let config_path = team_dir.join("config.json");
    if !config_path.exists() {
        anyhow::bail!("Team config not found at {config_path:?}");
    }

    let team_config: TeamConfig = serde_json::from_str(&fs::read_to_string(&config_path)?)?;

    // Count unread messages for each member
    let inbox_counts = count_inbox_messages(&team_dir, &team_config)?;

    // Count tasks if tasks directory exists
    let tasks_dir = home_dir.join(".claude/tasks").join(team_name);
    let (pending_tasks, completed_tasks) = if tasks_dir.exists() {
        count_tasks(&tasks_dir)?
    } else {
        (0, 0)
    };

    // Calculate age
    let age = format_age(team_config.created_at);

    // Output results
    if args.json {
        let output = json!({
            "team": team_name,
            "description": team_config.description,
            "createdAt": team_config.created_at,
            "members": team_config.members.iter().map(|m| {
                let unread = inbox_counts.get(&m.name).copied().unwrap_or(0);
                json!({
                    "name": m.name,
                    "type": m.agent_type,
                    "isActive": m.is_active.unwrap_or(false),
                    "unreadCount": unread,
                })
            }).collect::<Vec<_>>(),
            "inboxCounts": inbox_counts,
            "tasks": {
                "pending": pending_tasks,
                "completed": completed_tasks,
            }
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Team: {team_name}");
        if let Some(desc) = &team_config.description {
            println!("Description: {desc}");
        }
        println!("Created: {age}");
        println!();

        let member_count = team_config.members.len();
        println!("Members ({member_count}):");
        for member in &team_config.members {
            let active_str = if member.is_active.unwrap_or(false) { "Online " } else { "Offline" };
            let unread = inbox_counts.get(&member.name).copied().unwrap_or(0);
            let name = &member.name;
            let agent_type = &member.agent_type;
            println!("  {name:<20} {agent_type:<20} {active_str:<6}    {unread} unread");
        }

        if pending_tasks > 0 || completed_tasks > 0 {
            println!();
            println!("Tasks: {pending_tasks} pending, {completed_tasks} completed");
        }
    }

    Ok(())
}

/// Count unread messages in inboxes
fn count_inbox_messages(team_dir: &std::path::Path, team_config: &TeamConfig) -> Result<HashMap<String, usize>> {
    let mut counts = HashMap::new();
    let inboxes_dir = team_dir.join("inboxes");

    if !inboxes_dir.exists() {
        return Ok(counts);
    }

    for member in &team_config.members {
        let inbox_path = inboxes_dir.join(format!("{}.json", member.name));
        if inbox_path.exists() {
            match fs::read_to_string(&inbox_path) {
                Ok(content) => {
                    if let Ok(messages) = serde_json::from_str::<Vec<InboxMessage>>(&content) {
                        let unread_count = messages.iter().filter(|m| !m.read).count();
                        counts.insert(member.name.clone(), unread_count);
                    }
                }
                Err(_) => {
                    // Ignore read errors
                }
            }
        }
    }

    Ok(counts)
}

/// Count pending and completed tasks
fn count_tasks(tasks_dir: &std::path::Path) -> Result<(usize, usize)> {
    use agent_team_mail_core::{TaskItem, TaskStatus};

    let mut pending = 0;
    let mut completed = 0;

    if let Ok(entries) = fs::read_dir(tasks_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path.extension().and_then(|s| s.to_str()) == Some("json")
                && let Ok(content) = fs::read_to_string(&path)
                && let Ok(task) = serde_json::from_str::<TaskItem>(&content)
            {
                match task.status {
                    TaskStatus::Completed => completed += 1,
                    TaskStatus::Pending | TaskStatus::InProgress => pending += 1,
                    TaskStatus::Deleted => { /* don't count */ }
                }
            }
        }
    }

    Ok((pending, completed))
}

/// Format age as human-readable string
fn format_age(timestamp_ms: u64) -> String {
    use chrono::{DateTime, Utc};

    let created = DateTime::from_timestamp((timestamp_ms / 1000) as i64, 0);

    match created {
        Some(created_dt) => {
            let now = Utc::now();
            let duration = now.signed_duration_since(created_dt);

            let days = duration.num_days();
            if days > 0 {
                return if days == 1 {
                    "1 day ago".to_string()
                } else {
                    format!("{days} days ago")
                };
            }

            let hours = duration.num_hours();
            if hours > 0 {
                return if hours == 1 {
                    "1 hour ago".to_string()
                } else {
                    format!("{hours} hours ago")
                };
            }

            let minutes = duration.num_minutes();
            if minutes > 0 {
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
