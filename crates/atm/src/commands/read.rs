//! Read command implementation

use agent_team_mail_core::config::{ConfigOverrides, resolve_config, resolve_identity};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::schema::{InboxMessage, TeamConfig};
use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::{ArgAction, Args};

use crate::util::addressing::parse_address;
use crate::util::caller_identity::resolve_caller_session_id_optional;
use crate::util::hook_identity::read_hook_file_identity;
use crate::util::settings::{get_home_dir, teams_root_dir_for};
use crate::util::state::{get_last_seen, load_seen_state, save_seen_state, update_last_seen};

use super::wait::{WaitResult, wait_for_message};

/// Read messages from an inbox
///
/// By default, shows unread and pending-ack queue items from your own inbox and
/// collapses history to a count line. Messages remain pending until explicitly
/// acknowledged with `atm ack <message-id> "<reply>"`.
#[derive(Args, Debug)]
pub struct ReadArgs {
    /// Target agent (name or name@team), omit to read own inbox
    agent: Option<String>,

    /// Override default team
    #[arg(long)]
    team: Option<String>,

    /// Show all messages, including history
    #[arg(long)]
    all: bool,

    /// Show only unread messages
    #[arg(long, conflicts_with_all = ["pending_ack_only", "history", "all"])]
    unread_only: bool,

    /// Show only read-but-pending-ack messages
    #[arg(long, conflicts_with_all = ["unread_only", "history", "all"])]
    pending_ack_only: bool,

    /// Expand view to include the history bucket (shows unread, pending-ack, and historical messages)
    #[arg(long, conflicts_with_all = ["unread_only", "pending_ack_only"])]
    history: bool,

    /// Show messages since last seen (default: true)
    #[arg(long, default_value_t = true)]
    since_last_seen: bool,

    /// Disable since-last-seen filtering and watermark updates
    #[arg(long = "no-since-last-seen", action = ArgAction::SetTrue, overrides_with = "since_last_seen")]
    no_since_last_seen: bool,

    /// Don't mark unread messages as read/pending-ack
    #[arg(long)]
    no_mark: bool,

    /// Don't update last-seen state
    #[arg(long)]
    no_update_seen: bool,

    /// Show only last N displayed messages (`--count` accepted as compatibility alias)
    #[arg(long, visible_alias = "count")]
    limit: Option<usize>,

    /// Show messages after timestamp (ISO 8601 format)
    #[arg(long)]
    since: Option<String>,

    /// Filter by sender
    #[arg(long)]
    from: Option<String>,

    /// Output as JSON
    #[arg(long)]
    json: bool,

    /// Wait for new messages (timeout in seconds). Exit 0 if message received, 1 if timeout
    #[arg(long)]
    timeout: Option<u64>,

    /// Override reader identity (default: hook file → ATM_IDENTITY → .atm.toml → reject)
    #[arg(long = "as", value_name = "NAME")]
    reader_as: Option<String>,
}

/// Execute the read command
pub fn execute(args: ReadArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };

    let mut config = resolve_config(&overrides, &current_dir, &home_dir)?;

    if let Some(ref name) = args.reader_as {
        config.core.identity = name.clone();
    } else if config.core.identity == "human" {
        match read_hook_file_identity() {
            Ok(Some(identity)) => {
                config.core.identity = identity;
            }
            Ok(None) => {
                anyhow::bail!(
                    "Cannot determine reader identity: hook file not found. \
                     Ensure the atm-identity-write.py PreToolUse hook is configured in \
                     .claude/settings.json, or use --as <name> to specify identity explicitly. \
                     Ask the user who you are on this team."
                );
            }
            Err(e) => {
                anyhow::bail!(
                    "Cannot determine reader identity: hook file validation failed: {e}. \
                     Use --as <name> to specify identity explicitly. \
                     Ask the user who you are on this team."
                );
            }
        }
    }

    let (agent_name, team_name) = if let Some(ref agent_addr) = args.agent {
        let (parsed_agent, parsed_team) =
            parse_address(agent_addr, &args.team, &config.core.default_team)?;
        let resolved = resolve_identity(&parsed_agent, &config.roles, &config.aliases);
        if resolved != parsed_agent {
            eprintln!(
                "Note: '{}' resolved via roles/alias to '{}'",
                parsed_agent, resolved
            );
        }
        (resolved, parsed_team)
    } else {
        (
            config.core.identity.clone(),
            config.core.default_team.clone(),
        )
    };

    let caller_session_id =
        resolve_caller_session_id_optional(Some(&team_name), Some(&config.core.identity))
            .ok()
            .flatten();

    let team_dir = teams_root_dir_for(&home_dir).join(&team_name);
    if !team_dir.exists() {
        anyhow::bail!("Team '{team_name}' not found (directory {team_dir:?} doesn't exist)");
    }

    let team_config_path = team_dir.join("config.json");
    if !team_config_path.exists() {
        anyhow::bail!("Team config not found at {team_config_path:?}");
    }

    let team_config: TeamConfig =
        serde_json::from_str(&std::fs::read_to_string(&team_config_path)?)?;

    let agent_exists = team_config.members.iter().any(|m| m.name == agent_name);
    if !agent_exists && args.agent.is_some() {
        anyhow::bail!("Agent '{agent_name}' not found in team '{team_name}'");
    }

    let hostname_registry = extract_hostname_registry(&config);
    let mut filtered_messages = agent_team_mail_core::io::inbox::inbox_read_merged(
        &team_dir,
        &agent_name,
        hostname_registry.as_ref(),
    )?;

    let use_since_last_seen = args.since_last_seen && !args.no_since_last_seen;
    let last_seen = if use_since_last_seen {
        let state = load_seen_state().unwrap_or_default();
        get_last_seen(&state, &team_name, &agent_name)
    } else {
        None
    };

    if let Some(ref from_name) = args.from {
        filtered_messages.retain(|m| m.from == *from_name);
    }

    if let Some(ref since_ts) = args.since {
        let since_dt = DateTime::parse_from_rfc3339(since_ts)
            .map_err(|e| anyhow::anyhow!("Invalid timestamp format: {e}"))?;
        filtered_messages.retain(|m| {
            if let Ok(msg_dt) = DateTime::parse_from_rfc3339(&m.timestamp) {
                msg_dt > since_dt
            } else {
                false
            }
        });
    }

    let mut buckets = bucket_messages(filtered_messages.clone());
    let mut displayed_messages = select_display_messages(&buckets, &args);
    apply_limit(&mut displayed_messages, args.limit);

    if displayed_messages.is_empty()
        && let Some(timeout_secs) = args.timeout
    {
        let inboxes_dir = team_dir.join("inboxes");
        let hostnames: Option<Vec<String>> = hostname_registry
            .as_ref()
            .map(|reg| reg.remotes().map(|r| r.hostname.clone()).collect());

        eprintln!("Waiting for new messages (timeout: {timeout_secs}s)...");

        match wait_for_message(&inboxes_dir, &agent_name, timeout_secs, hostnames.as_ref())? {
            WaitResult::MessageReceived => {
                let mut new_filtered = agent_team_mail_core::io::inbox::inbox_read_merged(
                    &team_dir,
                    &agent_name,
                    hostname_registry.as_ref(),
                )?;

                if let Some(ref from_name) = args.from {
                    new_filtered.retain(|m| m.from == *from_name);
                }

                if let Some(ref since_ts) = args.since {
                    let since_dt = DateTime::parse_from_rfc3339(since_ts)
                        .map_err(|e| anyhow::anyhow!("Invalid timestamp format: {e}"))?;
                    new_filtered.retain(|m| {
                        if let Ok(msg_dt) = DateTime::parse_from_rfc3339(&m.timestamp) {
                            msg_dt > since_dt
                        } else {
                            false
                        }
                    });
                }

                filtered_messages = new_filtered;
                buckets = bucket_messages(filtered_messages.clone());
                displayed_messages = select_display_messages(&buckets, &args);
                apply_limit(&mut displayed_messages, args.limit);
            }
            WaitResult::Timeout => {
                if args.json {
                    let output = serde_json::json!({
                        "action": "read",
                        "agent": agent_name,
                        "team": team_name,
                        "messages": [],
                        "count": 0,
                        "bucket_counts": {
                            "unread": 0,
                            "pending_ack": 0,
                            "history": 0,
                        },
                        "timeout": true,
                    });
                    println!("{}", serde_json::to_string_pretty(&output)?);
                } else {
                    println!("Timeout: No new messages for {agent_name}@{team_name}");
                }
                emit_event_best_effort(EventFields {
                    level: "info",
                    source: "atm",
                    action: "read_timeout",
                    team: Some(team_name.clone()),
                    session_id: caller_session_id.clone(),
                    agent_id: Some(agent_name.clone()),
                    agent_name: Some(agent_name.clone()),
                    result: Some("timeout".to_string()),
                    ..Default::default()
                });
                std::process::exit(1);
            }
        }
    }

    let calling_identity = config.core.identity.clone();
    let mut marked_count: u64 = 0;
    if !args.no_mark && !displayed_messages.is_empty() && agent_name == calling_identity {
        let filtered_ids: Vec<String> = displayed_messages
            .iter()
            .filter(|m| !m.read)
            .filter_map(|m| m.message_id.clone())
            .collect();
        let filtered_timestamps: Vec<String> = displayed_messages
            .iter()
            .filter(|m| !m.read)
            .map(|m| m.timestamp.clone())
            .collect();
        let pending_timestamp = Utc::now().to_rfc3339();

        let local_inbox_path = team_dir.join("inboxes").join(format!("{agent_name}.json"));
        if local_inbox_path.exists() {
            agent_team_mail_core::io::inbox::inbox_update(
                &local_inbox_path,
                &team_name,
                &agent_name,
                |msgs| {
                    for msg in msgs.iter_mut() {
                        let should_mark = if let Some(ref msg_id) = msg.message_id {
                            filtered_ids.contains(msg_id) && !msg.read
                        } else {
                            filtered_timestamps.contains(&msg.timestamp) && !msg.read
                        };

                        if should_mark {
                            msg.read = true;
                            msg.mark_pending_ack(pending_timestamp.clone());
                            marked_count += 1;
                        }
                    }
                },
            )?;
        }
    }

    if use_since_last_seen
        && !args.no_update_seen
        && let Some(latest) = displayed_messages
            .iter()
            .filter_map(|m| DateTime::parse_from_rfc3339(&m.timestamp).ok())
            .max()
    {
        let mut state = load_seen_state().unwrap_or_default();
        update_last_seen(&mut state, &team_name, &agent_name, &latest.to_rfc3339());
        let _ = save_seen_state(&state);
    }

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "read",
        team: Some(team_name.clone()),
        session_id: caller_session_id.clone(),
        agent_id: Some(agent_name.clone()),
        agent_name: Some(agent_name.clone()),
        result: Some("ok".to_string()),
        count: Some(displayed_messages.len() as u64),
        ..Default::default()
    });
    if marked_count > 0 {
        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm",
            action: "read_mark",
            team: Some(team_name.clone()),
            session_id: caller_session_id,
            agent_id: Some(agent_name.clone()),
            agent_name: Some(agent_name.clone()),
            result: Some("ok".to_string()),
            count: Some(marked_count),
            ..Default::default()
        });
    }

    if args.json {
        let output = serde_json::json!({
            "action": "read",
            "agent": agent_name,
            "team": team_name,
            "messages": displayed_messages,
            "count": displayed_messages.len(),
            "bucket_counts": {
                "unread": buckets.unread.len(),
                "pending_ack": buckets.pending_ack.len(),
                "history": buckets.history.len(),
            },
            "history_collapsed": !args.history && !args.all,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if displayed_messages.is_empty() {
        println!("No messages found for {agent_name}@{team_name}");
    } else {
        println!("Queue for {agent_name}@{team_name}");
        println!(
            "Unread: {} | Pending Ack: {} | History: {}\n",
            buckets.unread.len(),
            buckets.pending_ack.len(),
            buckets.history.len()
        );

        let bucket_views = display_bucket_views(&displayed_messages);
        print_bucket("Unread", &bucket_views.unread);
        print_bucket("Pending Ack", &bucket_views.pending_ack);
        if args.history || args.all {
            print_bucket("History", &bucket_views.history);
        } else if !buckets.history.is_empty() {
            println!(
                "{} historical message(s) hidden (use --history to expand)\n",
                buckets.history.len()
            );
        }

        println!("Total displayed: {} message(s)", displayed_messages.len());
    }

    let _ = last_seen;
    Ok(())
}

/// Format timestamp as relative time (e.g., "2m ago", "1h ago")
fn format_relative_time(timestamp_str: &str) -> String {
    let timestamp = DateTime::parse_from_rfc3339(timestamp_str).ok();
    if let Some(ts) = timestamp {
        let now = Utc::now();
        let duration = now.signed_duration_since(ts.with_timezone(&Utc));

        if duration.num_seconds() < 0 {
            "in the future".to_string()
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

struct MessageBuckets {
    unread: Vec<InboxMessage>,
    pending_ack: Vec<InboxMessage>,
    history: Vec<InboxMessage>,
}

type DisplayBuckets = MessageBuckets;

fn bucket_messages(messages: Vec<InboxMessage>) -> MessageBuckets {
    let mut buckets = MessageBuckets {
        unread: Vec::new(),
        pending_ack: Vec::new(),
        history: Vec::new(),
    };

    for message in messages {
        if !message.read {
            buckets.unread.push(message);
        } else if message.pending_ack_at().is_some() && !message.is_acknowledged() {
            buckets.pending_ack.push(message);
        } else {
            buckets.history.push(message);
        }
    }

    sort_bucket_newest_first(&mut buckets.unread);
    sort_bucket_newest_first(&mut buckets.pending_ack);
    sort_bucket_newest_first(&mut buckets.history);
    buckets
}

fn sort_bucket_newest_first(messages: &mut [InboxMessage]) {
    messages.sort_by(|a, b| {
        b.timestamp.cmp(&a.timestamp).then_with(|| {
            b.message_id
                .as_deref()
                .unwrap_or_default()
                .cmp(a.message_id.as_deref().unwrap_or_default())
        })
    });
}

fn select_display_messages(buckets: &MessageBuckets, args: &ReadArgs) -> Vec<InboxMessage> {
    let mut displayed = Vec::new();

    if args.unread_only {
        displayed.extend(buckets.unread.clone());
        return displayed;
    }

    if args.pending_ack_only {
        displayed.extend(buckets.pending_ack.clone());
        return displayed;
    }

    displayed.extend(buckets.unread.clone());
    displayed.extend(buckets.pending_ack.clone());
    if args.history || args.all {
        displayed.extend(buckets.history.clone());
    }

    displayed
}

fn apply_limit(displayed_messages: &mut Vec<InboxMessage>, limit: Option<usize>) {
    if let Some(limit) = limit {
        displayed_messages.truncate(limit);
    }
}

fn display_bucket_views(displayed_messages: &[InboxMessage]) -> DisplayBuckets {
    bucket_messages(displayed_messages.to_vec())
}

fn print_bucket(name: &str, messages: &[InboxMessage]) {
    if messages.is_empty() {
        return;
    }

    println!("{name}:\n");
    for msg in messages {
        let time_ago = format_relative_time(&msg.timestamp);
        let summary = msg.summary.as_deref().unwrap_or("[no summary]");
        let status = if msg.is_acknowledged() {
            "[acknowledged]"
        } else if msg.read {
            if msg.pending_ack_at().is_some() {
                "[read, pending ack]"
            } else {
                "[read]"
            }
        } else {
            "[unread]"
        };

        println!("From: {} | {} | {} {}", msg.from, time_ago, summary, status);
        if let Some(message_id) = msg.message_id.as_deref() {
            println!("Message ID: {message_id}");
        }
        println!("{}\n", msg.text);
    }
}

/// Extract hostname registry from bridge plugin config
///
/// Returns None if bridge plugin is not configured or not enabled.
fn extract_hostname_registry(
    config: &agent_team_mail_core::config::Config,
) -> Option<agent_team_mail_core::config::HostnameRegistry> {
    use agent_team_mail_core::config::BridgeConfig;

    let bridge_table = config.plugins.get("bridge")?;
    let bridge_config: BridgeConfig = match bridge_table.clone().try_into() {
        Ok(cfg) => cfg,
        Err(_) => return None,
    };

    if !bridge_config.enabled {
        return None;
    }

    let mut registry = agent_team_mail_core::config::HostnameRegistry::new();
    for remote in bridge_config.remotes {
        let _ = registry.register(remote);
    }

    Some(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn inbox_message(
        message_id: &str,
        timestamp: &str,
        read: bool,
        pending_ack: bool,
    ) -> InboxMessage {
        let mut unknown_fields = HashMap::new();
        if pending_ack {
            unknown_fields.insert(
                "pendingAckAt".to_string(),
                serde_json::Value::String("2026-02-11T11:05:00Z".to_string()),
            );
        }
        InboxMessage {
            from: "team-lead".to_string(),
            source_team: None,
            text: format!("message {message_id}"),
            timestamp: timestamp.to_string(),
            read,
            summary: None,
            message_id: Some(message_id.to_string()),
            unknown_fields,
        }
    }

    #[test]
    fn test_format_relative_time_seconds() {
        let now = Utc::now();
        let ts = now - chrono::Duration::seconds(30);
        let formatted = format_relative_time(&ts.to_rfc3339());
        assert!(formatted.contains("s ago") || formatted.contains("1m ago"));
    }

    #[test]
    fn test_format_relative_time_minutes() {
        let now = Utc::now();
        let ts = now - chrono::Duration::minutes(5);
        let formatted = format_relative_time(&ts.to_rfc3339());
        assert!(formatted.contains("m ago"));
    }

    #[test]
    fn test_format_relative_time_hours() {
        let now = Utc::now();
        let ts = now - chrono::Duration::hours(3);
        let formatted = format_relative_time(&ts.to_rfc3339());
        assert!(formatted.contains("h ago"));
    }

    #[test]
    fn test_format_relative_time_days() {
        let now = Utc::now();
        let ts = now - chrono::Duration::days(2);
        let formatted = format_relative_time(&ts.to_rfc3339());
        assert!(formatted.contains("d ago"));
    }

    #[test]
    fn test_format_relative_time_invalid() {
        let formatted = format_relative_time("invalid-timestamp");
        assert_eq!(formatted, "unknown");
    }

    #[test]
    fn sort_bucket_newest_first_orders_by_timestamp_then_message_id_desc() {
        let mut messages = vec![
            inbox_message("msg-001", "2026-02-11T10:00:00Z", false, false),
            inbox_message("msg-003", "2026-02-11T11:00:00Z", false, false),
            inbox_message("msg-002", "2026-02-11T11:00:00Z", false, false),
        ];

        sort_bucket_newest_first(&mut messages);

        let ids: Vec<&str> = messages
            .iter()
            .map(|message| message.message_id.as_deref().unwrap())
            .collect();
        assert_eq!(ids, vec!["msg-003", "msg-002", "msg-001"]);
    }

    #[test]
    fn history_flag_expands_active_view_instead_of_filtering() {
        let buckets = MessageBuckets {
            unread: vec![inbox_message(
                "msg-u1",
                "2026-02-11T12:00:00Z",
                false,
                false,
            )],
            pending_ack: vec![inbox_message("msg-p1", "2026-02-11T11:00:00Z", true, true)],
            history: vec![inbox_message("msg-h1", "2026-02-11T10:00:00Z", true, false)],
        };

        let args = ReadArgs {
            agent: None,
            team: None,
            all: false,
            unread_only: false,
            pending_ack_only: false,
            history: true,
            since_last_seen: false,
            no_since_last_seen: true,
            no_mark: true,
            no_update_seen: true,
            limit: None,
            since: None,
            from: None,
            json: false,
            timeout: None,
            reader_as: None,
        };

        let displayed = select_display_messages(&buckets, &args);
        let ids: Vec<&str> = displayed
            .iter()
            .map(|message| message.message_id.as_deref().unwrap())
            .collect();
        assert_eq!(ids, vec!["msg-u1", "msg-p1", "msg-h1"]);
    }
}
