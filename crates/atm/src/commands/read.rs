//! Read command implementation

use anyhow::Result;
use atm_core::config::{resolve_config, ConfigOverrides};
use atm_core::schema::{InboxMessage, TeamConfig};
use chrono::{DateTime, Utc};
use clap::{ArgAction, Args};

use crate::util::addressing::parse_address;
use crate::util::settings::get_home_dir;
use crate::util::state::{get_last_seen, load_seen_state, save_seen_state, update_last_seen};

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

    // Read inbox messages
    let inbox_path = team_dir.join("inboxes").join(format!("{agent_name}.json"));
    let messages: Vec<InboxMessage> = if inbox_path.exists() {
        let content = std::fs::read_to_string(&inbox_path)?;
        serde_json::from_str(&content)?
    } else {
        // Empty inbox - not an error
        Vec::new()
    };

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

    // Filter by read status (unless --all or since-last-seen)
    if !args.all && !use_since_last_seen {
        filtered_messages.retain(|m| !m.read);
    }

    // Filter by last-seen timestamp
    if let Some(last_seen_dt) = last_seen {
        filtered_messages.retain(|m| {
            DateTime::parse_from_rfc3339(&m.timestamp)
                .map(|dt| dt > last_seen_dt)
                .unwrap_or(false)
        });
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

    // Mark messages as read (unless --no-mark specified)
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

        // Atomically update inbox to mark messages as read
        if inbox_path.exists() {
            atm_core::io::inbox::inbox_update(&inbox_path, &team_name, &agent_name, |msgs| {
                for msg in msgs.iter_mut() {
                    let should_mark = if let Some(ref msg_id) = msg.message_id {
                        filtered_ids.contains(msg_id) && !msg.read
                    } else {
                        // Fallback: match by timestamp
                        filtered_timestamps.contains(&msg.timestamp) && !msg.read
                    };

                    if should_mark {
                        msg.read = true;
                    }
                }
            })?;
        }
    }

    // Update last-seen state (unless disabled)
    if use_since_last_seen && !args.no_update_seen {
        if let Some(latest) = filtered_messages
            .iter()
            .filter_map(|m| DateTime::parse_from_rfc3339(&m.timestamp).ok())
            .max()
        {
            let mut state = load_seen_state().unwrap_or_default();
            update_last_seen(&mut state, &team_name, &agent_name, &latest.to_rfc3339());
            let _ = save_seen_state(&state);
        }
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
