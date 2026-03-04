//! Members command implementation

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::daemon_client::{
    canonical_liveness_bool, query_list_agents, query_team_member_states,
};
use agent_team_mail_core::schema::TeamConfig;
use anyhow::Result;
use clap::Args;
use serde_json::json;
use std::collections::{BTreeSet, HashMap};
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

struct MemberRow {
    name: String,
    agent_type: String,
    model: String,
    liveness: Option<bool>,
    in_config: bool,
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
    let daemon_states: HashMap<_, _> = query_team_member_states(team_name)
        .ok()
        .flatten()
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.agent.clone(), s))
        .collect();

    let member_rows = build_member_rows(&team_config, &daemon_states);

    // Output results
    if args.json {
        let output = json!({
            "team": team_name,
            "members": member_rows.iter().map(|m| json!({
                "name": m.name,
                "type": m.agent_type,
                "model": m.model,
                "liveness": m.liveness,
                "inConfig": m.in_config,
                "ghost": !m.in_config,
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

            for member in &member_rows {
                let active = match member.liveness {
                    Some(true) => "Online",
                    Some(false) => "Offline",
                    None => "Unknown",
                };
                let name = if member.in_config {
                    member.name.clone()
                } else {
                    format!("{} [ghost]", member.name)
                };
                let agent_type = &member.agent_type;
                let model = &member.model;
                println!("  {name:<20} {agent_type:<20} {model:<25} {active}");
            }
        }
    }

    Ok(())
}

fn build_member_rows(
    team_config: &TeamConfig,
    daemon_states: &HashMap<String, agent_team_mail_core::daemon_client::CanonicalMemberState>,
) -> Vec<MemberRow> {
    let mut by_name: HashMap<&str, &agent_team_mail_core::schema::AgentMember> = HashMap::new();
    for member in &team_config.members {
        by_name.insert(member.name.as_str(), member);
    }

    let mut names = BTreeSet::new();
    for member in &team_config.members {
        names.insert(member.name.clone());
    }
    for state in daemon_states.values() {
        names.insert(state.agent.clone());
    }

    names
        .into_iter()
        .map(|name| {
            if let Some(member) = by_name.get(name.as_str()) {
                MemberRow {
                    name,
                    agent_type: member.agent_type.clone(),
                    model: member.model.clone(),
                    liveness: canonical_liveness_bool(daemon_states.get(member.name.as_str())),
                    in_config: true,
                }
            } else {
                MemberRow {
                    name: name.clone(),
                    agent_type: "[unregistered]".to_string(),
                    model: "[unregistered]".to_string(),
                    liveness: canonical_liveness_bool(daemon_states.get(name.as_str())),
                    in_config: false,
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_core::schema::{AgentMember, TeamConfig};

    fn member(name: &str) -> AgentMember {
        AgentMember {
            agent_id: format!("{name}@atm-dev"),
            name: name.to_string(),
            agent_type: "general-purpose".to_string(),
            model: "unknown".to_string(),
            prompt: None,
            color: None,
            plan_mode_required: None,
            joined_at: 0,
            tmux_pane_id: None,
            cwd: ".".to_string(),
            subscriptions: Vec::new(),
            backend_type: None,
            is_active: None,
            last_active: None,
            session_id: None,
            external_backend_type: None,
            external_model: None,
            unknown_fields: HashMap::new(),
        }
    }

    #[test]
    fn build_member_rows_includes_daemon_only_member() {
        let cfg = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "sess".to_string(),
            members: vec![member("team-lead")],
            unknown_fields: HashMap::new(),
        };
        let mut daemon_states = HashMap::new();
        daemon_states.insert(
            "arch-ctm".to_string(),
            agent_team_mail_core::daemon_client::CanonicalMemberState {
                agent: "arch-ctm".to_string(),
                state: "active".to_string(),
                activity: "busy".to_string(),
                session_id: Some("sess-1".to_string()),
                process_id: Some(1234),
                reason: "session active".to_string(),
                source: "session_registry".to_string(),
                in_config: false,
            },
        );

        let rows = build_member_rows(&cfg, &daemon_states);
        assert!(rows.iter().any(|r| r.name == "team-lead" && r.in_config));
        assert!(rows.iter().any(|r| r.name == "arch-ctm" && !r.in_config));
    }
}
