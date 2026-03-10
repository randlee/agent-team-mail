//! Status command implementation

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::daemon_client::{
    canonical_liveness_bool, query_list_agents, query_team_member_states,
};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::schema::{InboxMessage, TeamConfig};
use anyhow::Result;
use clap::Args;
use serde::{Deserialize, Serialize};
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
    let logging = read_daemon_logging_health(&home_dir);
    let logging_health = build_logging_health_contract(&logging);

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
            },
            "logging": json!({
                "state": logging.state,
                "dropped_counter": logging.dropped_counter,
                "spool_path": logging.spool_path,
                "last_error": logging.last_error,
                "canonical_log_path": logging.canonical_log_path,
                "spool_count": logging.spool_count,
                "oldest_spool_age": logging.oldest_spool_age,
            }),
            "logging_health": json!({
                "status": logging_health.status,
                "otel_exporter": logging_health.otel_exporter,
                "local_structured": logging_health.local_structured,
                "last_export_error": logging_health.last_export_error,
            }),
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
        println!("  otel_status:     {}", logging_health.otel_exporter);
        if let Some(last_export_error) = &logging_health.last_export_error {
            println!("  otel_last_error: {last_export_error}");
        }
        if let Some(remediation) = logging_remediation(&logging.state) {
            println!("  remediation:     {remediation}");
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct LoggingHealth {
    state: String,
    dropped_counter: u64,
    spool_path: String,
    last_error: Option<String>,
    canonical_log_path: String,
    spool_count: u64,
    oldest_spool_age: Option<u64>,
}

impl Default for LoggingHealth {
    fn default() -> Self {
        Self {
            state: "unavailable".to_string(),
            dropped_counter: 0,
            spool_path: String::new(),
            last_error: None,
            canonical_log_path: String::new(),
            spool_count: 0,
            oldest_spool_age: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoggingHealthContract {
    status: String,
    otel_exporter: String,
    local_structured: bool,
    last_export_error: Option<String>,
}

impl Default for LoggingHealthContract {
    fn default() -> Self {
        Self {
            status: "unavailable".to_string(),
            otel_exporter: "unavailable".to_string(),
            local_structured: true,
            last_export_error: None,
        }
    }
}

fn logging_status_bucket(state: &str) -> &'static str {
    match state {
        "healthy" => "ok",
        "unavailable" => "unavailable",
        _ => "degraded",
    }
}

fn otel_enabled_from_env() -> bool {
    !matches!(
        std::env::var("ATM_OTEL_ENABLED")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase()),
        Some(ref v) if v == "0" || v == "false" || v == "off" || v == "disabled" || v == "no"
    )
}

fn build_logging_health_contract(logging: &LoggingHealth) -> LoggingHealthContract {
    let status = logging_status_bucket(&logging.state).to_string();
    let mut otel_exporter = status.clone();
    let mut last_export_error = if otel_exporter == "ok" {
        None
    } else {
        logging.last_error.clone()
    };

    if !otel_enabled_from_env() {
        otel_exporter = "unavailable".to_string();
        if last_export_error.is_none() {
            last_export_error = Some("otel exporter disabled by ATM_OTEL_ENABLED".to_string());
        }
    }

    LoggingHealthContract {
        status,
        otel_exporter,
        local_structured: true,
        last_export_error,
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DaemonStatusSnapshot {
    #[serde(default)]
    logging: LoggingHealth,
}

fn read_daemon_logging_health(home_dir: &std::path::Path) -> LoggingHealth {
    let status_path = home_dir.join(".claude/daemon/status.json");
    let Ok(content) = fs::read_to_string(status_path) else {
        return LoggingHealth::default();
    };
    serde_json::from_str::<DaemonStatusSnapshot>(&content)
        .map(|status| status.logging)
        .unwrap_or_default()
}

fn logging_remediation(state: &str) -> Option<&'static str> {
    match state {
        "degraded_dropping" => {
            Some("queue is dropping events; verify daemon health and reduce log burst load")
        }
        "degraded_spooling" => Some(
            "events are spooling locally; verify daemon socket/path and allow merge to catch up",
        ),
        "unavailable" => Some(
            "logging unavailable; check ATM_LOG value, daemon status, and log path permissions",
        ),
        _ => None,
    }
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
    fn read_daemon_logging_health_parses_extended_fields() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let daemon_dir = tmp.path().join(".claude/daemon");
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

        let logging = read_daemon_logging_health(tmp.path());
        assert_eq!(logging.state, "degraded_spooling");
        assert_eq!(logging.dropped_counter, 2);
        assert_eq!(logging.spool_count, 3);
        assert_eq!(logging.oldest_spool_age, Some(17));
        assert_eq!(logging.canonical_log_path, log_path);
    }

    #[test]
    fn logging_remediation_returns_messages_for_degraded_and_unavailable() {
        assert!(logging_remediation("healthy").is_none());
        assert!(logging_remediation("degraded_spooling").is_some());
        assert!(logging_remediation("degraded_dropping").is_some());
        assert!(logging_remediation("unavailable").is_some());
    }

    #[test]
    #[serial]
    fn logging_health_contract_contains_required_json_keys() {
        // SAFETY: test-scoped env mutation.
        unsafe {
            std::env::remove_var("ATM_OTEL_ENABLED");
        }
        let contract = build_logging_health_contract(&LoggingHealth {
            state: "degraded_spooling".to_string(),
            dropped_counter: 1,
            spool_path: "/tmp/spool".to_string(),
            last_error: Some("events are queued in spool awaiting merge".to_string()),
            canonical_log_path: "/tmp/atm.log.jsonl".to_string(),
            spool_count: 2,
            oldest_spool_age: Some(10),
        });
        let value = serde_json::to_value(contract).expect("serialize logging_health");
        assert_eq!(value["status"], "degraded");
        assert_eq!(value["otel_exporter"], "degraded");
        assert_eq!(value["local_structured"], true);
        assert!(value["last_export_error"].is_string());

        // SAFETY: test-scoped env mutation.
        unsafe {
            std::env::set_var("ATM_OTEL_ENABLED", "false");
        }
        let disabled = build_logging_health_contract(&LoggingHealth {
            state: "healthy".to_string(),
            ..LoggingHealth::default()
        });
        assert_eq!(disabled.status, "ok");
        assert_eq!(disabled.otel_exporter, "unavailable");
        assert_eq!(
            disabled.last_export_error.as_deref(),
            Some("otel exporter disabled by ATM_OTEL_ENABLED")
        );
        // SAFETY: cleanup after test.
        unsafe {
            std::env::remove_var("ATM_OTEL_ENABLED");
        }
    }
}
