//! Inbox command implementation - show inbox summaries

use anyhow::Result;
use atm_core::config::{resolve_config, ConfigOverrides};
use atm_core::schema::{InboxMessage, TeamConfig};
use chrono::DateTime;
use clap::{ArgAction, Args};
use std::path::Path;

use crate::util::settings::get_home_dir;
use crate::util::state::{get_last_seen, load_seen_state};

/// Show inbox summary for team members
#[derive(Args, Debug)]
pub struct InboxArgs {
    /// Override default team
    #[arg(long)]
    team: Option<String>,

    /// Show summary across all teams
    #[arg(long)]
    all_teams: bool,

    /// Show counts since last seen (default: true)
    #[arg(long, default_value_t = true)]
    since_last_seen: bool,

    /// Disable since-last-seen filtering
    #[arg(long = "no-since-last-seen", action = ArgAction::SetTrue, overrides_with = "since_last_seen")]
    no_since_last_seen: bool,

    /// Watch inboxes and log changes (polling)
    #[arg(long)]
    watch: bool,

    /// Poll interval for --watch (milliseconds)
    #[arg(long, default_value_t = 200)]
    interval_ms: u64,
}

/// Execute the inbox command
pub fn execute(args: InboxArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };

    let config = resolve_config(&overrides, &current_dir, &home_dir)?;

    let teams_dir = home_dir.join(".claude/teams");
    if !teams_dir.exists() {
        anyhow::bail!("Teams directory not found at {teams_dir:?}");
    }

    let use_since_last_seen = args.since_last_seen && !args.no_since_last_seen;

    if args.watch {
        watch_inboxes(
            &home_dir,
            &config.core.default_team,
            args.all_teams,
            args.interval_ms,
        )?;
        return Ok(());
    }

    if args.all_teams {
        // Show summary for all teams
        let entries = std::fs::read_dir(&teams_dir)?;
        let mut team_names: Vec<String> = Vec::new();

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir()
                && let Some(name_str) = path.file_name().and_then(|n| n.to_str())
            {
                team_names.push(name_str.to_string());
            }
        }

        team_names.sort();

        for team_name in team_names {
            show_team_summary(&home_dir, &team_name, use_since_last_seen)?;
            println!();
        }
    } else {
        // Show summary for single team
        let team_name = &config.core.default_team;
        show_team_summary(&home_dir, team_name, use_since_last_seen)?;
    }

    Ok(())
}

/// Show inbox summary for a single team
fn show_team_summary(home_dir: &Path, team_name: &str, use_since_last_seen: bool) -> Result<()> {
    let team_dir = home_dir.join(".claude/teams").join(team_name);

    if !team_dir.exists() {
        println!("Team: {team_name} (not found)");
        return Ok(());
    }

    // Load team config
    let team_config_path = team_dir.join("config.json");
    if !team_config_path.exists() {
        println!("Team: {team_name} (config not found)");
        return Ok(());
    }

    let team_config: TeamConfig = serde_json::from_str(&std::fs::read_to_string(&team_config_path)?)?;

    println!("Team: {team_name}\n");
    if use_since_last_seen {
        println!("  {:<20} {:>8} {:>8} {:>12}", "Agent", "New", "Total", "Latest");
    } else {
        println!("  {:<20} {:>8} {:>8} {:>12}", "Agent", "Unread", "Total", "Latest");
    }
    println!("  {}", "─".repeat(52));

    // Collect agent summaries
    let mut summaries = Vec::new();
    for member in &team_config.members {
        let inbox_path = team_dir.join("inboxes").join(format!("{}.json", member.name));

        let (unread, total, latest) = if inbox_path.exists() {
            let content = std::fs::read_to_string(&inbox_path)?;
            let messages: Vec<InboxMessage> = serde_json::from_str(&content)?;

            let unread_count = if use_since_last_seen {
                let state = load_seen_state().unwrap_or_default();
                let last_seen = get_last_seen(&state, team_name, &member.name);
                match last_seen {
                    Some(last_seen_dt) => messages
                        .iter()
                        .filter(|m| {
                            DateTime::parse_from_rfc3339(&m.timestamp)
                                .map(|dt| dt > last_seen_dt)
                                .unwrap_or(false)
                        })
                        .count(),
                    None => messages.len(),
                }
            } else {
                messages.iter().filter(|m| !m.read).count()
            };
            let total_count = messages.len();
            let latest_time = messages
                .last()
                .map(|m| format_relative_time(&m.timestamp))
                .unwrap_or_else(|| "-".to_string());

            (unread_count, total_count, latest_time)
        } else {
            (0, 0, "-".to_string())
        };

        summaries.push((member.name.clone(), unread, total, latest));
    }

    // Display summaries
    for (agent_name, unread, total, latest) in summaries {
        println!("  {agent_name:<20} {unread:>8} {total:>8} {latest:>12}");
    }

    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InboxSnapshot {
    unread: usize,
    total: usize,
    latest: String,
}

#[derive(Clone, Debug)]
struct MessageState {
    read: bool,
    summary: Option<String>,
    from: String,
}

fn watch_inboxes(
    home_dir: &Path,
    default_team: &str,
    all_teams: bool,
    interval_ms: u64,
) -> Result<()> {
    let mut previous: std::collections::HashMap<(String, String), InboxSnapshot> =
        std::collections::HashMap::new();
    let mut message_states: std::collections::HashMap<(String, String, String), MessageState> =
        std::collections::HashMap::new();

    loop {
        let team_names = if all_teams {
            let entries = std::fs::read_dir(home_dir.join(".claude/teams"))?;
            let mut names = Vec::new();
            for entry in entries {
                let entry = entry?;
                if entry.path().is_dir()
                    && let Some(name_str) = entry.file_name().to_str()
                {
                    names.push(name_str.to_string());
                }
            }
            names.sort();
            names
        } else {
            vec![default_team.to_string()]
        };

        for team_name in team_names {
            let team_dir = home_dir.join(".claude/teams").join(&team_name);
            let team_config_path = team_dir.join("config.json");
            if !team_config_path.exists() {
                continue;
            }

            let team_config: TeamConfig =
                serde_json::from_str(&std::fs::read_to_string(&team_config_path)?)?;

            for member in &team_config.members {
                let inbox_path = team_dir.join("inboxes").join(format!("{}.json", member.name));
                let snapshot = if inbox_path.exists() {
                    let content = std::fs::read_to_string(&inbox_path)?;
                    let messages: Vec<InboxMessage> = serde_json::from_str(&content)?;
                    let unread = messages.iter().filter(|m| !m.read).count();
                    let total = messages.len();
                    let latest = messages
                        .last()
                        .map(|m| format_relative_time(&m.timestamp))
                        .unwrap_or_else(|| "-".to_string());

                    for msg in &messages {
                        let key = (
                            team_name.clone(),
                            member.name.clone(),
                            msg.message_id
                                .clone()
                                .unwrap_or_else(|| msg.timestamp.clone()),
                        );
                        let state = MessageState {
                            read: msg.read,
                            summary: msg.summary.clone(),
                            from: msg.from.clone(),
                        };

                        if let Some(prev) = message_states.get(&key) {
                            if prev.read != state.read {
                                println!(
                                    "[{}] {}@{} message read {}->{} from={} summary={}",
                                    chrono::Utc::now().to_rfc3339(),
                                    member.name,
                                    team_name,
                                    prev.read,
                                    state.read,
                                    state.from,
                                    state.summary.as_deref().unwrap_or("-")
                                );
                            }
                        } else {
                            println!(
                                "[{}] {}@{} new message read={} from={} summary={}",
                                chrono::Utc::now().to_rfc3339(),
                                member.name,
                                team_name,
                                state.read,
                                state.from,
                                state.summary.as_deref().unwrap_or("-")
                            );
                        }

                        message_states.insert(key, state);
                    }

                    InboxSnapshot { unread, total, latest }
                } else {
                    InboxSnapshot {
                        unread: 0,
                        total: 0,
                        latest: "-".to_string(),
                    }
                };

                let key = (team_name.clone(), member.name.clone());
                if let Some(prev) = previous.get(&key) {
                    if prev != &snapshot {
                        println!(
                            "[{}] {}@{} unread {}->{} total {}->{} latest {}",
                            chrono::Utc::now().to_rfc3339(),
                            member.name,
                            team_name,
                            prev.unread,
                            snapshot.unread,
                            prev.total,
                            snapshot.total,
                            snapshot.latest
                        );
                    }
                }
                previous.insert(key, snapshot);
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
    }
}

/// Format timestamp as relative time (e.g., "2m ago", "1h ago")
fn format_relative_time(timestamp_str: &str) -> String {
    let timestamp = DateTime::parse_from_rfc3339(timestamp_str).ok();
    if let Some(ts) = timestamp {
        let now = chrono::Utc::now();
        let duration = now.signed_duration_since(ts.with_timezone(&chrono::Utc));

        if duration.num_seconds() < 0 {
            "future".to_string()
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
