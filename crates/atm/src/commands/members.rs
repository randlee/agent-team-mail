//! Members command implementation

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::daemon_client::query_list_agents;
use agent_team_mail_core::schema::TeamConfig;
use anyhow::Result;
use clap::Args;
use serde_json::json;
use std::collections::HashMap;
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
    // Prime daemon connectivity so daemon-backed liveness can be queried.
    let _ = query_list_agents();

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
    let daemon_liveness = load_daemon_liveness(team_name, &team_config);

    // Output results
    if args.json {
        let output = json!({
            "team": team_name,
            "members": team_config.members.iter().map(|m| json!({
                "name": m.name,
                "type": m.agent_type,
                "model": m.model,
                "liveness": resolve_member_liveness(m, &daemon_liveness),
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
                let active = match resolve_member_liveness(member, &daemon_liveness) {
                    Some(true) => "Online",
                    Some(false) => "Offline",
                    None => "Unknown",
                };
                let name = &member.name;
                let agent_type = &member.agent_type;
                let model = &member.model;
                println!("  {name:<20} {agent_type:<20} {model:<25} {active}");
            }
        }
    }

    Ok(())
}

fn load_daemon_liveness(team_name: &str, team_config: &TeamConfig) -> HashMap<String, bool> {
    let mut liveness = HashMap::new();
    for member in &team_config.members {
        if let Ok(Some(info)) =
            agent_team_mail_core::daemon_client::query_session_for_team(team_name, &member.name)
        {
            liveness.insert(member.name.clone(), info.alive);
        }
    }
    liveness
}

fn resolve_member_liveness(
    member: &agent_team_mail_core::schema::AgentMember,
    daemon_liveness: &HashMap<String, bool>,
) -> Option<bool> {
    daemon_liveness.get(&member.name).copied()
}
