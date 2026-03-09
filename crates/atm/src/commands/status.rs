//! Status command implementation

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::daemon_client::{
    canonical_liveness_bool, query_list_agents, query_team_member_states,
};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::schema::{InboxMessage, TeamConfig};
use anyhow::Result;
use clap::Args;
use serde_json::json;
use std::collections::{BTreeSet, HashMap};
use std::fs;

use crate::util::member_labels::{GHOST_SUFFIX, UNREGISTERED_MARKER};
use crate::util::settings::get_home_dir;

/// Show combined team overview
#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Team name (optional, uses default team if not specified)
    #[arg(long)]
    team: Option<String>,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

struct StatusMemberRow {
    name: String,
    agent_type: String,
    liveness: Option<bool>,
    in_config: bool,
}

/// Execute the status command
pub fn execute(args: StatusArgs) -> Result<()> {
    // Prime daemon connectivity so daemon-backed liveness fields are available.
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

    let member_rows = build_status_member_rows(&team_config, &daemon_states);

    // Count unread messages for each member
    let inbox_counts = count_inbox_messages(&team_dir, &member_rows)?;

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
            "members": member_rows.iter().map(|m| {
                let unread = inbox_counts.get(&m.name).copied().unwrap_or(0);
                json!({
                    "name": m.name,
                    "type": m.agent_type,
                    "liveness": m.liveness,
                    "inConfig": m.in_config,
                    "ghost": !m.in_config,
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

        let member_count = member_rows.len();
        println!("Members ({member_count}):");
        for member in &member_rows {
            let active_str = match member.liveness {
                Some(true) => "Online ",
                Some(false) => "Offline",
                None => "Unknown",
            };
            let unread = inbox_counts.get(&member.name).copied().unwrap_or(0);
            let name = if member.in_config {
                member.name.clone()
            } else {
                format!("{}{}", member.name, GHOST_SUFFIX)
            };
            let agent_type = &member.agent_type;
            println!("  {name:<20} {agent_type:<20} {active_str:<6}    {unread} unread");
        }

        if pending_tasks > 0 || completed_tasks > 0 {
            println!();
            println!("Tasks: {pending_tasks} pending, {completed_tasks} completed");
        }
    }

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "status",
        team: Some(team_name.clone()),
        session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
        agent_id: Some(config.core.identity.clone()),
        agent_name: Some(config.core.identity.clone()),
        result: Some(if args.json { "ok_json" } else { "ok_human" }.to_string()),
        count: Some(member_rows.len() as u64),
        ..Default::default()
    });

    Ok(())
}

fn build_status_member_rows(
    team_config: &TeamConfig,
    daemon_states: &HashMap<String, agent_team_mail_core::daemon_client::CanonicalMemberState>,
) -> Vec<StatusMemberRow> {
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
                StatusMemberRow {
                    name,
                    agent_type: member.agent_type.clone(),
                    liveness: canonical_liveness_bool(daemon_states.get(member.name.as_str())),
                    in_config: true,
                }
            } else {
                StatusMemberRow {
                    name: name.clone(),
                    agent_type: UNREGISTERED_MARKER.to_string(),
                    liveness: canonical_liveness_bool(daemon_states.get(name.as_str())),
                    in_config: false,
                }
            }
        })
        .collect()
}

/// Count unread messages in inboxes
fn count_inbox_messages(
    team_dir: &std::path::Path,
    members: &[StatusMemberRow],
) -> Result<HashMap<String, usize>> {
    let mut counts = HashMap::new();
    let inboxes_dir = team_dir.join("inboxes");

    if !inboxes_dir.exists() {
        return Ok(counts);
    }

    for member in members {
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
    fn build_status_member_rows_includes_daemon_only_member() {
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
                last_alive_at: None,
                reason: "session active".to_string(),
                source: "session_registry".to_string(),
                in_config: false,
            },
        );

        let rows = build_status_member_rows(&cfg, &daemon_states);
        assert!(rows.iter().any(|r| r.name == "team-lead" && r.in_config));
        assert!(rows.iter().any(|r| r.name == "arch-ctm" && !r.in_config));
    }
}
