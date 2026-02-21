//! Read command implementation

use anyhow::Result;
use agent_team_mail_core::config::{resolve_config, ConfigOverrides};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::schema::TeamConfig;
use chrono::{DateTime, Utc};
use clap::{ArgAction, Args};

use crate::util::addressing::parse_address;
use crate::util::settings::get_home_dir;
use crate::util::state::{get_last_seen, load_seen_state, save_seen_state, update_last_seen};

use super::wait::{wait_for_message, WaitResult};

/// Read messages from an inbox
///
/// By default, shows unread messages from your own inbox and marks them as read.
/// Use --no-mark to read without marking, or --all to include already-read messages.
#[derive(Args, Debug)]
pub struct ReadArgs {
    /// Target agent (name or name@team), omit to read own inbox
    agent: Option<String>,

    /// Override default team
    #[arg(long)]
    team: Option<String>,

    /// Show all messages (not just unread)
    #[arg(long)]
    all: bool,

    /// Show messages since last seen (default: true)
    #[arg(long, default_value_t = true)]
    since_last_seen: bool,

    /// Disable since-last-seen filtering
    #[arg(long = "no-since-last-seen", action = ArgAction::SetTrue, overrides_with = "since_last_seen")]
    no_since_last_seen: bool,

    /// Don't mark messages as read
    #[arg(long)]
    no_mark: bool,

    /// Don't update last-seen state
    #[arg(long)]
    no_update_seen: bool,

    /// Show only last N messages
    #[arg(long)]
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
}

/// Execute the read command
pub fn execute(args: ReadArgs) -> Result<()> {
    // Resolve configuration
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };

    let config = resolve_config(&overrides, &current_dir, &home_dir)?;

    // Determine agent and team
    let (agent_name, team_name) = if let Some(ref agent_addr) = args.agent {
        parse_address(agent_addr, &args.team, &config.core.default_team)?
    } else {
        // Read own inbox
        (config.core.identity.clone(), config.core.default_team.clone())
    };

    // Resolve team directory
    let team_dir = home_dir.join(".claude/teams").join(&team_name);
    if !team_dir.exists() {
        anyhow::bail!("Team '{team_name}' not found (directory {team_dir:?} doesn't exist)");
    }

    // Load team config to verify agent exists
    let team_config_path = team_dir.join("config.json");
    if !team_config_path.exists() {
        anyhow::bail!("Team config not found at {team_config_path:?}");
    }

    let team_config: TeamConfig = serde_json::from_str(&std::fs::read_to_string(&team_config_path)?)?;

    // Verify agent exists in team
    if !team_config.members.iter().any(|m| m.name == agent_name) {
        anyhow::bail!("Agent '{agent_name}' not found in team '{team_name}'");
    }

    // Extract hostname registry from config (if bridge is configured)
    let hostname_registry = extract_hostname_registry(&config);

    // Read inbox messages (merged from local + all origin files)
    let messages = agent_team_mail_core::io::inbox::inbox_read_merged(
        &team_dir,
        &agent_name,
        hostname_registry.as_ref(),
    )?;

    // Apply filters
    let mut filtered_messages = messages.clone();

    // Resolve last-seen state (if enabled)
    let use_since_last_seen = args.since_last_seen && !args.no_since_last_seen;
    let last_seen = if use_since_last_seen && !args.all {
        let state = load_seen_state().unwrap_or_default();
        get_last_seen(&state, &team_name, &agent_name)
    } else {
        None
    };

    // Visibility filter:
    // - Default mode: unread only
    // - Since-last-seen mode: unread OR newer-than-last-seen
    if !args.all {
        if use_since_last_seen {
            if let Some(last_seen_dt) = last_seen {
                filtered_messages.retain(|m| {
                    !m.read
                        || DateTime::parse_from_rfc3339(&m.timestamp)
                            .map(|dt| dt > last_seen_dt)
                            .unwrap_or(false)
                });
            }
        } else {
            filtered_messages.retain(|m| !m.read);
        }
    }

    // Filter by sender
    if let Some(ref from_name) = args.from {
        filtered_messages.retain(|m| m.from == *from_name);
    }

    // Filter by timestamp
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

    // Apply limit
    if let Some(limit) = args.limit {
        let start = filtered_messages.len().saturating_sub(limit);
        filtered_messages = filtered_messages[start..].to_vec();
    }

    // If timeout specified and no messages found, wait for new messages
    if filtered_messages.is_empty() && let Some(timeout_secs) = args.timeout {
        let inboxes_dir = team_dir.join("inboxes");

        // Extract hostnames for bridge-synced messages
        let hostnames: Option<Vec<String>> = hostname_registry.as_ref().map(|reg| {
            reg.remotes().map(|r| r.hostname.clone()).collect()
        });

        eprintln!("Waiting for new messages (timeout: {timeout_secs}s)...");

        match wait_for_message(
            &inboxes_dir,
            &agent_name,
            timeout_secs,
            hostnames.as_ref(),
        )? {
            WaitResult::MessageReceived => {
                // Re-read messages and apply filters
                let new_messages = agent_team_mail_core::io::inbox::inbox_read_merged(
                    &team_dir,
                    &agent_name,
                    hostname_registry.as_ref(),
                )?;

                let mut new_filtered = new_messages.clone();

                // Re-apply the same filters
                if !args.all {
                    if use_since_last_seen {
                        if let Some(last_seen_dt) = last_seen {
                            new_filtered.retain(|m| {
                                !m.read
                                    || DateTime::parse_from_rfc3339(&m.timestamp)
                                        .map(|dt| dt > last_seen_dt)
                                        .unwrap_or(false)
                            });
                        }
                    } else {
                        new_filtered.retain(|m| !m.read);
                    }
                }

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

                if let Some(limit) = args.limit {
                    let start = new_filtered.len().saturating_sub(limit);
                    new_filtered = new_filtered[start..].to_vec();
                }

                // Use the new filtered messages
                filtered_messages = new_filtered;
            }
            WaitResult::Timeout => {
                if args.json {
                    let output = serde_json::json!({
                        "action": "read",
                        "agent": agent_name,
                        "team": team_name,
                        "messages": [],
                        "count": 0,
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
                    session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
                    agent_id: Some(agent_name.clone()),
                    agent_name: Some(agent_name.clone()),
                    result: Some("timeout".to_string()),
                    ..Default::default()
                });
                std::process::exit(1);
            }
        }
    }

    // Mark messages as read (unless --no-mark specified)
    let mut marked_count: u64 = 0;
    if !args.no_mark && !filtered_messages.is_empty() {
        // Find message IDs that need to be marked
        let filtered_ids: Vec<String> = filtered_messages
            .iter()
            .filter_map(|m| m.message_id.clone())
            .collect();

        let filtered_timestamps: Vec<String> = filtered_messages
            .iter()
            .map(|m| m.timestamp.clone())
            .collect();

        // Atomically update LOCAL inbox to mark messages as read
        // Note: we only mark in the local inbox, not in origin files
        let local_inbox_path = team_dir.join("inboxes").join(format!("{agent_name}.json"));
        if local_inbox_path.exists() {
            agent_team_mail_core::io::inbox::inbox_update(&local_inbox_path, &team_name, &agent_name, |msgs| {
                for msg in msgs.iter_mut() {
                    let should_mark = if let Some(ref msg_id) = msg.message_id {
                        filtered_ids.contains(msg_id) && !msg.read
                    } else {
                        // Fallback: match by timestamp
                        filtered_timestamps.contains(&msg.timestamp) && !msg.read
                    };

                    if should_mark {
                        msg.read = true;
                        marked_count += 1;
                    }
                }
            })?;
        }
    }

    // Update last-seen state (unless disabled)
    if use_since_last_seen && !args.no_update_seen
        && let Some(latest) = filtered_messages
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
        session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
        agent_id: Some(agent_name.clone()),
        agent_name: Some(agent_name.clone()),
        result: Some("ok".to_string()),
        count: Some(filtered_messages.len() as u64),
        ..Default::default()
    });
    if marked_count > 0 {
        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm",
            action: "read_mark",
            team: Some(team_name.clone()),
            session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
            agent_id: Some(agent_name.clone()),
            agent_name: Some(agent_name.clone()),
            result: Some("ok".to_string()),
            count: Some(marked_count),
            ..Default::default()
        });
    }

    // Output results
    if args.json {
        let output = serde_json::json!({
            "action": "read",
            "agent": agent_name,
            "team": team_name,
            "messages": filtered_messages,
            "count": filtered_messages.len(),
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if filtered_messages.is_empty() {
        println!("No messages found for {agent_name}@{team_name}");
    } else {
        println!("Messages for {agent_name}@{team_name}:\n");
        for msg in &filtered_messages {
            let time_ago = format_relative_time(&msg.timestamp);
            let summary = msg.summary.as_deref().unwrap_or("[no summary]");

            println!("From: {} | {} | {}", msg.from, time_ago, summary);
            println!("{}\n", msg.text);
        }
        println!("Total: {} message(s)", filtered_messages.len());
    }

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

/// Extract hostname registry from bridge plugin config
///
/// Returns None if bridge plugin is not configured or not enabled.
fn extract_hostname_registry(config: &agent_team_mail_core::config::Config) -> Option<agent_team_mail_core::config::HostnameRegistry> {
    use agent_team_mail_core::config::BridgeConfig;

    // Check if bridge plugin config exists
    let bridge_table = config.plugins.get("bridge")?;

    // Parse bridge config
    let bridge_config: BridgeConfig = match bridge_table.clone().try_into() {
        Ok(cfg) => cfg,
        Err(_) => return None,
    };

    // Check if bridge is enabled
    if !bridge_config.enabled {
        return None;
    }

    // Build hostname registry from remotes
    let mut registry = agent_team_mail_core::config::HostnameRegistry::new();
    for remote in bridge_config.remotes {
        let _ = registry.register(remote); // Ignore errors
    }

    Some(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_relative_time_seconds() {
        // Note: This test is approximate and may be flaky
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
}
