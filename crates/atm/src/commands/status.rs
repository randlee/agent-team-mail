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

use crate::commands::logging_health::{
    build_logging_health_contract, build_otel_health_contract, logging_remediation,
    read_daemon_logging_health, read_daemon_otel_health,
};
use crate::util::member_labels::{GHOST_SUFFIX, UNREGISTERED_MARKER};
use crate::util::settings::{get_home_dir, teams_root_dir_for};

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

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
struct InboxCounts {
    unread: usize,
    pending: usize,
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
    let team_dir = teams_root_dir_for(&home_dir).join(team_name);
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
    let logging = read_daemon_logging_health(&home_dir);
    let logging_health = build_logging_health_contract(&logging, &home_dir);
    let otel = read_daemon_otel_health(&home_dir);
    let otel_health = build_otel_health_contract(&otel);

    // Count inbox message states for each member
    let inbox_counts = count_inbox_messages(&team_dir, &member_rows)?;

    // Count tasks if tasks directory exists
    let tasks_dir = crate::util::settings::claude_root_dir_for(&home_dir)
        .join("tasks")
        .join(team_name);
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
                let counts = inbox_counts.get(&m.name).copied().unwrap_or_default();
                json!({
                    "name": m.name,
                    "type": m.agent_type,
                    "liveness": m.liveness,
                    "inConfig": m.in_config,
                    "ghost": !m.in_config,
                    "unreadCount": counts.unread,
                    "pendingCount": counts.pending,
                })
            }).collect::<Vec<_>>(),
            "inboxCounts": inbox_counts,
            "tasks": {
                "pending": pending_tasks,
                "completed": completed_tasks,
            },
            "logging_health": serde_json::to_value(&logging_health)
                .expect("logging_health should serialize"),
            "otel_health": serde_json::to_value(&otel_health)
                .expect("otel_health should serialize"),
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
            let counts = inbox_counts.get(&member.name).copied().unwrap_or_default();
            let name = if member.in_config {
                member.name.clone()
            } else {
                format!("{}{}", member.name, GHOST_SUFFIX)
            };
            let agent_type = &member.agent_type;
            println!(
                "  {name:<20} {agent_type:<20} {active_str:<6}    {} pending",
                counts.pending
            );
        }

        if pending_tasks > 0 || completed_tasks > 0 {
            println!();
            println!("Tasks: {pending_tasks} pending, {completed_tasks} completed");
        }

        println!();
        println!("Logging:");
        println!("  state:           {}", logging.state);
        println!("  dropped_counter: {}", logging.dropped_counter);
        println!("  spool_path:      {}", logging.spool_path);
        println!("  canonical_log_path: {}", logging.canonical_log_path);
        println!("  spool_count:     {}", logging.spool_count);
        if let Some(oldest_spool_age) = logging.oldest_spool_age {
            println!("  oldest_spool_age: {oldest_spool_age}s");
        }
        if let Some(last_error) = &logging.last_error {
            println!("  last_error:      {last_error}");
        }
        println!("  schema_version:  {}", logging_health.schema_version);
        println!("  log_root:        {}", logging_health.log_root);
        println!(
            "  dropped_events_total: {}",
            logging_health.dropped_events_total
        );
        println!("  spool_file_count: {}", logging_health.spool_file_count);
        if let Some(last_code) = &logging_health.last_error.code {
            println!("  last_error.code: {last_code}");
        }
        if let Some(last_message) = &logging_health.last_error.message {
            println!("  last_error.message: {last_message}");
        }
        if let Some(last_at) = &logging_health.last_error.at {
            println!("  last_error.at:   {last_at}");
        }
        if let Some(remediation) = logging_remediation(&logging.state) {
            println!("  remediation:     {remediation}");
        }
        println!();
        println!("OTel:");
        println!("  schema_version:  {}", otel_health.schema_version);
        println!("  enabled:         {}", otel_health.enabled);
        println!("  protocol:        {}", otel_health.protocol);
        println!("  collector_state: {}", otel_health.collector_state);
        println!("  local_mirror_state: {}", otel_health.local_mirror_state);
        println!("  local_mirror_path:  {}", otel_health.local_mirror_path);
        println!("  debug_local_export: {}", otel_health.debug_local_export);
        println!("  debug_local_state:  {}", otel_health.debug_local_state);
        if let Some(endpoint) = &otel_health.collector_endpoint {
            println!("  collector_endpoint: {endpoint}");
        }
        if let Some(code) = &otel_health.last_error.code {
            println!("  last_error.code: {code}");
        }
        if let Some(message) = &otel_health.last_error.message {
            println!("  last_error.message: {message}");
        }
        if let Some(at) = &otel_health.last_error.at {
            println!("  last_error.at:   {at}");
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

/// Count unread and pending-action messages in inboxes.
fn count_inbox_messages(
    team_dir: &std::path::Path,
    members: &[StatusMemberRow],
) -> Result<HashMap<String, InboxCounts>> {
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
                        let pending_count =
                            messages.iter().filter(|m| m.is_pending_action()).count();
                        counts.insert(
                            member.name.clone(),
                            InboxCounts {
                                unread: unread_count,
                                pending: pending_count,
                            },
                        );
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
    use serial_test::serial;

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

    #[test]
    #[serial]
    fn read_daemon_logging_health_parses_extended_fields() {
        let original_home = std::env::var("ATM_HOME").ok();
        let tmp = tempfile::tempdir().expect("temp dir");
        let daemon_dir = tmp.path().join(".atm/daemon");
        std::fs::create_dir_all(&daemon_dir).expect("create daemon dir");
        let sys_tmp = std::env::temp_dir();
        let spool_path = sys_tmp.join("spool").to_string_lossy().into_owned();
        let log_path = sys_tmp.join("atm.log.jsonl").to_string_lossy().into_owned();
        std::fs::write(
            daemon_dir.join("status.json"),
            serde_json::json!({
                "logging": {
                    "state": "degraded_spooling",
                    "dropped_counter": 2,
                    "spool_path": spool_path,
                    "last_error": "spool backlog",
                    "canonical_log_path": log_path,
                    "spool_count": 3,
                    "oldest_spool_age": 17
                }
            })
            .to_string(),
        )
        .expect("write status");
        unsafe { std::env::set_var("ATM_HOME", tmp.path()) };

        let logging = read_daemon_logging_health(tmp.path());
        assert_eq!(logging.state, "degraded_spooling");
        assert_eq!(logging.dropped_counter, 2);
        assert_eq!(logging.spool_count, 3);
        assert_eq!(logging.oldest_spool_age, Some(17));
        assert_eq!(logging.canonical_log_path, log_path);

        unsafe {
            match original_home {
                Some(value) => std::env::set_var("ATM_HOME", value),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    fn logging_remediation_returns_messages_for_degraded_and_unavailable() {
        assert!(logging_remediation("healthy").is_none());
        assert!(logging_remediation("degraded_spooling").is_some());
        assert!(logging_remediation("degraded_dropping").is_some());
        assert!(logging_remediation("unavailable").is_some());
    }

    #[test]
    fn logging_health_contract_contains_required_json_keys() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let temp_root = std::env::temp_dir();
        let spool_path = temp_root.join("spool").to_string_lossy().to_string();
        let canonical_log_path = temp_root
            .join("atm.log.jsonl")
            .to_string_lossy()
            .to_string();
        let contract = build_logging_health_contract(
            &crate::commands::logging_health::LoggingHealthSnapshot {
                state: "degraded_spooling".to_string(),
                dropped_counter: 1,
                spool_path: spool_path.clone(),
                last_error: Some("events are queued in spool awaiting merge".to_string()),
                canonical_log_path: canonical_log_path.clone(),
                spool_count: 2,
                oldest_spool_age: Some(10),
            },
            tmp.path(),
        );
        let value = serde_json::to_value(contract).expect("serialize logging_health");
        assert_eq!(value["schema_version"], "v1");
        assert_eq!(value["state"], "degraded_spooling");
        assert_eq!(value["canonical_log_path"], canonical_log_path);
        assert_eq!(value["spool_path"], spool_path);
        assert_eq!(value["dropped_events_total"], 1);
        assert_eq!(value["spool_file_count"], 2);
        assert_eq!(value["oldest_spool_age_seconds"], 10);
        assert!(value["log_root"].is_string());
        assert_eq!(value["last_error"]["code"], "DEGRADED_SPOOLING");
        assert_eq!(
            value["last_error"]["message"],
            "events are queued in spool awaiting merge"
        );
        assert!(value["last_error"]["at"].is_string());
    }
}
