//! Inbox command implementation - show inbox summaries and targeted cleanup

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::home::{config_team_dir_for, config_teams_root_dir_for, get_os_home_dir};
use agent_team_mail_core::retention::parse_duration;
use agent_team_mail_core::schema::InboxMessage;
use agent_team_mail_core::schema::TeamConfig;
use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::{ArgAction, Args, Subcommand};
use serde::Serialize;
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

    #[command(subcommand)]
    command: Option<InboxCommand>,
}

#[derive(Subcommand, Debug)]
enum InboxCommand {
    /// Clear selected messages from an inbox
    Clear(ClearArgs),
}

#[derive(Args, Debug)]
struct ClearArgs {
    /// Target agent inbox (defaults to current ATM identity)
    agent: Option<String>,

    /// Override default team
    #[arg(long)]
    team: Option<String>,

    /// Remove acknowledged messages
    #[arg(long)]
    acked: bool,

    /// Remove messages older than the given duration (e.g. 7d, 24h)
    #[arg(long, value_name = "DURATION")]
    older_than: Option<String>,

    /// Only remove idle notifications
    #[arg(long, conflicts_with = "acked", conflicts_with = "older_than")]
    idle_only: bool,

    /// Show what would be removed without mutating the inbox
    #[arg(long)]
    dry_run: bool,

    /// Output cleanup results as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
struct InboxClearResult {
    team: String,
    agent: String,
    dry_run: bool,
    inbox_path: String,
    removed_total: usize,
    remaining_total: usize,
    removed_idle_notifications: usize,
    removed_acked_messages: usize,
    removed_older_than: usize,
}

/// Execute the inbox command
pub fn execute(args: InboxArgs) -> Result<()> {
    if let Some(InboxCommand::Clear(mut clear_args)) = args.command {
        if clear_args.team.is_none() {
            clear_args.team = args.team.clone();
        }
        return execute_clear(clear_args);
    }

    let _runtime_home = get_home_dir()?;
    let config_home = get_os_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };

    let config = resolve_config(&overrides, &current_dir, &config_home)?;

    let teams_dir = config_teams_root_dir_for(&config_home);
    if !teams_dir.exists() {
        anyhow::bail!("Teams directory not found at {teams_dir:?}");
    }

    let use_since_last_seen = args.since_last_seen && !args.no_since_last_seen;

    if args.watch {
        watch_inboxes(
            &config_home,
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
            show_team_summary(&config_home, &team_name, use_since_last_seen)?;
            println!();
        }
    } else {
        // Show summary for single team
        let team_name = &config.core.default_team;
        show_team_summary(&config_home, team_name, use_since_last_seen)?;
    }

    Ok(())
}

fn execute_clear(args: ClearArgs) -> Result<()> {
    let _runtime_home = get_home_dir()?;
    let config_home = get_os_home_dir()?;
    let current_dir = std::env::current_dir()?;
    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };
    let config = resolve_config(&overrides, &current_dir, &config_home)?;
    let team_name = args
        .team
        .clone()
        .unwrap_or_else(|| config.core.default_team.clone());
    let agent_name = args
        .agent
        .clone()
        .unwrap_or_else(|| config.core.identity.clone());
    let inbox_path = config_team_dir_for(&config_home, &team_name)
        .join("inboxes")
        .join(format!("{agent_name}.json"));

    let result = clear_inbox_messages(&inbox_path, &team_name, &agent_name, &args)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if args.dry_run {
        println!(
            "Dry run - would remove {} message(s) from {}@{}",
            result.removed_total, agent_name, team_name
        );
        print_clear_counts(&result);
    } else {
        println!(
            "Cleared {} message(s) from {}@{}",
            result.removed_total, agent_name, team_name
        );
        print_clear_counts(&result);
    }

    Ok(())
}

fn print_clear_counts(result: &InboxClearResult) {
    println!(
        "  idle_notifications: {}",
        result.removed_idle_notifications
    );
    println!("  acked_messages: {}", result.removed_acked_messages);
    println!("  older_than: {}", result.removed_older_than);
    println!("  remaining_total: {}", result.remaining_total);
}

/// Show inbox summary for a single team
fn show_team_summary(config_home: &Path, team_name: &str, use_since_last_seen: bool) -> Result<()> {
    let team_dir = config_team_dir_for(config_home, team_name);

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

    let team_config: TeamConfig =
        serde_json::from_str(&std::fs::read_to_string(&team_config_path)?)?;

    // Load config to extract hostname registry (if bridge is configured)
    let config = agent_team_mail_core::config::resolve_config(
        &agent_team_mail_core::config::ConfigOverrides::default(),
        &std::env::current_dir()?,
        config_home,
    )?;
    let hostname_registry = extract_hostname_registry(&config);

    println!("Team: {team_name}\n");
    if use_since_last_seen {
        println!(
            "  {:<20} {:>8} {:>8} {:>12}",
            "Agent", "New", "Total", "Latest"
        );
    } else {
        println!(
            "  {:<20} {:>8} {:>8} {:>12}",
            "Agent", "Pending", "Total", "Latest"
        );
    }
    println!("  {}", "─".repeat(52));

    // Collect agent summaries
    let mut summaries = Vec::new();
    for member in &team_config.members {
        // Read merged messages (local + all origin files)
        let messages = agent_team_mail_core::io::inbox::inbox_read_merged(
            &team_dir,
            &member.name,
            hostname_registry.as_ref(),
        )?;

        let (pending, total, latest) = if !messages.is_empty() {
            let pending_count = if use_since_last_seen {
                let state = load_seen_state().unwrap_or_default();
                let last_seen = get_last_seen(&state, team_name, &member.name);
                match last_seen {
                    Some(last_seen_dt) => messages
                        .iter()
                        .filter(|m| {
                            m.is_pending_action()
                                || DateTime::parse_from_rfc3339(&m.timestamp)
                                    .map(|dt| dt > last_seen_dt)
                                    .unwrap_or(false)
                        })
                        .count(),
                    None => messages.iter().filter(|m| m.is_pending_action()).count(),
                }
            } else {
                messages.iter().filter(|m| m.is_pending_action()).count()
            };
            let total_count = messages.len();
            let latest_time = messages
                .last()
                .map(|m| format_relative_time(&m.timestamp))
                .unwrap_or_else(|| "-".to_string());

            (pending_count, total_count, latest_time)
        } else {
            (0, 0, "-".to_string())
        };

        summaries.push((member.name.clone(), pending, total, latest));
    }

    // Display summaries
    for (agent_name, pending, total, latest) in summaries {
        println!("  {agent_name:<20} {pending:>8} {total:>8} {latest:>12}");
    }

    Ok(())
}

fn clear_inbox_messages(
    inbox_path: &Path,
    team_name: &str,
    agent_name: &str,
    args: &ClearArgs,
) -> Result<InboxClearResult> {
    let messages: Vec<InboxMessage> = if inbox_path.exists() {
        serde_json::from_str(&std::fs::read_to_string(inbox_path)?)?
    } else {
        Vec::new()
    };
    let older_than = match args.older_than.as_deref() {
        Some(raw) => Some(parse_duration(raw)?),
        None => None,
    };
    let now = Utc::now();
    let mut result = InboxClearResult {
        team: team_name.to_string(),
        agent: agent_name.to_string(),
        dry_run: args.dry_run,
        inbox_path: inbox_path.display().to_string(),
        ..Default::default()
    };

    let mut kept = Vec::with_capacity(messages.len());
    for message in messages {
        let idle_match = message.is_idle_notification();
        let acked_match = args.acked && message.is_acknowledged();
        let older_match = older_than
            .as_ref()
            .is_some_and(|duration| message_is_older_than(&message, *duration, now));
        let should_remove = if args.idle_only {
            idle_match
        } else {
            idle_match || acked_match || older_match
        };

        if should_remove {
            result.removed_total += 1;
            if idle_match {
                result.removed_idle_notifications += 1;
            }
            if acked_match {
                result.removed_acked_messages += 1;
            }
            if older_match {
                result.removed_older_than += 1;
            }
        } else {
            kept.push(message);
        }
    }

    result.remaining_total = kept.len();

    if !args.dry_run && result.removed_total > 0 {
        agent_team_mail_core::io::inbox::inbox_update(
            inbox_path,
            team_name,
            agent_name,
            |stored| {
                stored.clear();
                stored.extend(kept.clone());
            },
        )?;
    }

    Ok(result)
}

fn message_is_older_than(
    message: &InboxMessage,
    max_age: chrono::Duration,
    now: chrono::DateTime<Utc>,
) -> bool {
    DateTime::parse_from_rfc3339(&message.timestamp)
        .map(|timestamp| now.signed_duration_since(timestamp.with_timezone(&Utc)) > max_age)
        .unwrap_or(true)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InboxSnapshot {
    pending: usize,
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
    config_home: &Path,
    default_team: &str,
    all_teams: bool,
    interval_ms: u64,
) -> Result<()> {
    let mut previous: std::collections::HashMap<(String, String), InboxSnapshot> =
        std::collections::HashMap::new();
    let mut message_states: std::collections::HashMap<(String, String, String), MessageState> =
        std::collections::HashMap::new();

    // Load config to extract hostname registry
    let config = agent_team_mail_core::config::resolve_config(
        &agent_team_mail_core::config::ConfigOverrides::default(),
        &std::env::current_dir()?,
        config_home,
    )?;
    let hostname_registry = extract_hostname_registry(&config);

    loop {
        let team_names = if all_teams {
            let entries = std::fs::read_dir(config_teams_root_dir_for(config_home))?;
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
            let team_dir = config_team_dir_for(config_home, &team_name);
            let team_config_path = team_dir.join("config.json");
            if !team_config_path.exists() {
                continue;
            }

            let team_config: TeamConfig =
                serde_json::from_str(&std::fs::read_to_string(&team_config_path)?)?;

            for member in &team_config.members {
                // Read merged messages (local + all origin files)
                let messages = agent_team_mail_core::io::inbox::inbox_read_merged(
                    &team_dir,
                    &member.name,
                    hostname_registry.as_ref(),
                )?;

                let snapshot = if !messages.is_empty() {
                    let pending = messages.iter().filter(|m| m.is_pending_action()).count();
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

                    InboxSnapshot {
                        pending,
                        total,
                        latest,
                    }
                } else {
                    InboxSnapshot {
                        pending: 0,
                        total: 0,
                        latest: "-".to_string(),
                    }
                };

                let key = (team_name.clone(), member.name.clone());
                if let Some(prev) = previous.get(&key)
                    && prev != &snapshot
                {
                    println!(
                        "[{}] {}@{} pending {}->{} total {}->{} latest {}",
                        chrono::Utc::now().to_rfc3339(),
                        member.name,
                        team_name,
                        prev.pending,
                        snapshot.pending,
                        prev.total,
                        snapshot.total,
                        snapshot.latest
                    );
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

/// Extract hostname registry from bridge plugin config
///
/// Returns None if bridge plugin is not configured or not enabled.
fn extract_hostname_registry(
    config: &agent_team_mail_core::config::Config,
) -> Option<agent_team_mail_core::config::HostnameRegistry> {
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
