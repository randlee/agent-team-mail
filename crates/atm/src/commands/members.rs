//! Members command implementation

use anyhow::Result;
use agent_team_mail_core::config::{resolve_config, ConfigOverrides};
use agent_team_mail_core::schema::TeamConfig;
use clap::Args;
use serde_json::json;
use std::fs;

use crate::util::settings::get_home_dir;

/// List agents in a team
#[derive(Args, Debug)]
pub struct MembersArgs {
    /// Team name (optional, uses default team if not specified)
    #[arg(long)]
    team: Option<String>,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

/// Execute the members command
pub fn execute(args: MembersArgs) -> Result<()> {
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

    // Output results
    if args.json {
        let output = json!({
            "team": team_name,
            "members": team_config.members.iter().map(|m| json!({
                "name": m.name,
                "type": m.agent_type,
                "model": m.model,
                "isActive": m.is_active.unwrap_or(false),
            })).collect::<Vec<_>>()
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Team: {team_name}");
        println!();

        if team_config.members.is_empty() {
            println!("  No members");
        } else {
            println!("  {:<20} {:<20} {:<25} Status", "Name", "Type", "Model");
            println!("  {}", "─".repeat(72));

            for member in &team_config.members {
                let active = if member.is_active.unwrap_or(false) { "Online" } else { "Offline" };
                let name = &member.name;
                let agent_type = &member.agent_type;
                let model = &member.model;
                println!("  {name:<20} {agent_type:<20} {model:<25} {active}");
            }
        }
    }

    Ok(())
}
