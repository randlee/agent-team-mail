//! Teams command implementation

use anyhow::Result;
use agent_team_mail_core::io::atomic::atomic_swap;
use agent_team_mail_core::io::lock::acquire_lock;
use agent_team_mail_core::schema::TeamConfig;
use chrono::{DateTime, Utc};
use clap::Args;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::io::Write;

use crate::util::settings::get_home_dir;

/// List all teams on this machine
#[derive(Args, Debug)]
pub struct TeamsArgs {
    /// Subcommand (e.g., add-member)
    #[command(subcommand)]
    command: Option<TeamsCommand>,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

#[derive(clap::Subcommand, Debug)]
pub enum TeamsCommand {
    /// Add a member to a team without launching an agent
    AddMember(AddMemberArgs),
}

/// Add a member to a team (no agent spawn)
#[derive(Args, Debug)]
pub struct AddMemberArgs {
    /// Team name
    team: String,

    /// Agent name (unique within team)
    agent: String,

    /// Agent type (e.g., "codex", "human", "plugin:ci_monitor")
    #[arg(long, default_value = "codex")]
    agent_type: String,

    /// Model identifier (optional, defaults to "unknown")
    #[arg(long, default_value = "unknown")]
    model: String,

    /// Working directory (defaults to current directory)
    #[arg(long)]
    cwd: Option<PathBuf>,

    /// Mark agent as inactive
    #[arg(long)]
    inactive: bool,

    /// tmux pane ID for message injection (e.g. "%235")
    #[arg(long)]
    pane_id: Option<String>,
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
    if let Some(command) = args.command {
        return match command {
            TeamsCommand::AddMember(add_args) => add_member(add_args),
        };
    }

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

fn add_member(args: AddMemberArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let team_dir = home_dir.join(".claude/teams").join(&args.team);
    if !team_dir.exists() {
        anyhow::bail!(
            "Team '{}' not found (directory {} doesn't exist)",
            args.team,
            team_dir.display()
        );
    }

    let config_path = team_dir.join("config.json");
    if !config_path.exists() {
        anyhow::bail!("Team config not found at {}", config_path.display());
    }

    let lock_path = config_path.with_extension("lock");
    let _lock = acquire_lock(&lock_path, 5)
        .map_err(|e| anyhow::anyhow!("Failed to acquire lock for team config: {e}"))?;

    let mut team_config: TeamConfig = serde_json::from_str(&fs::read_to_string(&config_path)?)?;

    let agent_id = format!("{}@{}", args.agent, args.team);
    let existing_idx = team_config
        .members
        .iter()
        .position(|m| m.agent_id == agent_id || m.name == args.agent);
    if let Some(idx) = existing_idx {
        // If --pane-id provided, update it on the existing member
        if let Some(ref pane_id) = args.pane_id {
            team_config.members[idx].tmux_pane_id = Some(pane_id.clone());
            let serialized = serde_json::to_string_pretty(&team_config)?;
            let tmp_path = config_path.with_extension("tmp");
            let mut file = std::fs::File::create(&tmp_path)?;
            file.write_all(serialized.as_bytes())?;
            file.sync_all()?;
            drop(file);
            atomic_swap(&config_path, &tmp_path)?;
            println!("Updated tmuxPaneId for '{}' in team '{}' (paneId='{}')", args.agent, args.team, pane_id);
        } else {
            println!("Member '{}' already exists in team '{}'", args.agent, args.team);
        }
        return Ok(());
    }

    let cwd = match args.cwd {
        Some(path) => path,
        None => std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf()),
    };

    let now_ms = chrono::Utc::now().timestamp_millis() as u64;

    let agent_type = args.agent_type.clone();
    let member = agent_team_mail_core::schema::AgentMember {
        agent_id,
        name: args.agent.clone(),
        agent_type,
        model: args.model,
        prompt: None,
        color: None,
        plan_mode_required: None,
        joined_at: now_ms,
        tmux_pane_id: args.pane_id,
        cwd: cwd.to_string_lossy().to_string(),
        subscriptions: Vec::new(),
        backend_type: None,
        is_active: Some(!args.inactive),
        last_active: if !args.inactive { Some(now_ms) } else { None },
        unknown_fields: std::collections::HashMap::new(),
    };

    team_config.members.push(member);
    let serialized = serde_json::to_string_pretty(&team_config)?;
    let tmp_path = config_path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp_path)?;
    file.write_all(serialized.as_bytes())?;
    file.sync_all()?;
    drop(file);

    atomic_swap(&config_path, &tmp_path)?;

    println!(
        "Added member '{}' to team '{}' (agentType='{}')",
        args.agent, args.team, args.agent_type
    );

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
