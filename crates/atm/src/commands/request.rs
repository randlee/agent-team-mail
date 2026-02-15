//! Request command implementation (send + wait for response)

use anyhow::Result;
use atm_core::config::{resolve_config, ConfigOverrides};
use atm_core::io::inbox::{inbox_append, inbox_update};
use atm_core::schema::{InboxMessage, TeamConfig};
use chrono::Utc;
use clap::Args;
use std::collections::HashMap;
use std::thread::sleep;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::util::addressing::parse_address;
use crate::util::settings::get_home_dir;

/// Send a message and wait for a response (polling)
#[derive(Args, Debug)]
pub struct RequestArgs {
    /// Sender mailbox (name or name@team)
    from: String,

    /// Destination mailbox (name or name@team)
    to: String,

    /// Message text
    message: String,

    /// Override sender team (if --from is name only)
    #[arg(long)]
    from_team: Option<String>,

    /// Override destination team (if --to is name only)
    #[arg(long)]
    to_team: Option<String>,

    /// Timeout (seconds)
    #[arg(long, default_value_t = 30)]
    timeout: u64,

    /// Poll interval (milliseconds). Temporary until daemon watcher exists.
    #[arg(long, default_value_t = 200)]
    poll_interval: u64,
}

/// Execute the request command
pub fn execute(args: RequestArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let overrides = ConfigOverrides::default();
    let config = resolve_config(&overrides, &current_dir, &home_dir)?;

    // Enforce explicit team for both sender and destination
    if !args.from.contains('@') && args.from_team.is_none() {
        anyhow::bail!("Sender must include team (name@team) or use --from-team");
    }
    if !args.to.contains('@') && args.to_team.is_none() {
        anyhow::bail!("Destination must include team (name@team) or use --to-team");
    }

    let (from_agent, from_team) =
        parse_address(&args.from, &args.from_team, &config.core.default_team)?;
    let (to_agent, to_team) =
        parse_address(&args.to, &args.to_team, &config.core.default_team)?;

    // Resolve team dirs and verify both members exist
    let from_team_dir = home_dir.join(".claude/teams").join(&from_team);
    if !from_team_dir.exists() {
        anyhow::bail!("Team '{from_team}' not found (directory {from_team_dir:?} doesn't exist)");
    }
    let to_team_dir = home_dir.join(".claude/teams").join(&to_team);
    if !to_team_dir.exists() {
        anyhow::bail!("Team '{to_team}' not found (directory {to_team_dir:?} doesn't exist)");
    }

    let from_config_path = from_team_dir.join("config.json");
    let to_config_path = to_team_dir.join("config.json");
    if !from_config_path.exists() {
        anyhow::bail!("Team config not found at {from_config_path:?}");
    }
    if !to_config_path.exists() {
        anyhow::bail!("Team config not found at {to_config_path:?}");
    }

    let from_team_config: TeamConfig =
        serde_json::from_str(&std::fs::read_to_string(&from_config_path)?)?;
    let to_team_config: TeamConfig =
        serde_json::from_str(&std::fs::read_to_string(&to_config_path)?)?;

    if !from_team_config.members.iter().any(|m| m.name == from_agent) {
        anyhow::bail!("Sender '{from_agent}' not found in team '{from_team}'");
    }
    if !to_team_config.members.iter().any(|m| m.name == to_agent) {
        anyhow::bail!("Destination '{to_agent}' not found in team '{to_team}'");
    }

    // Build request message with correlation id
    let request_id = Uuid::new_v4().to_string();
    let start = Instant::now();
    let request_text = format!("{}\n\nRequest-ID: {}", args.message, request_id);
    let summary = generate_summary(&request_text);

    let inbox_message = InboxMessage {
        from: from_agent.clone(),
        text: request_text.clone(),
        timestamp: Utc::now().to_rfc3339(),
        read: false,
        summary: Some(summary),
        message_id: Some(request_id.clone()),
        unknown_fields: HashMap::new(),
    };

    // Send to destination inbox
    let inboxes_dir = to_team_dir.join("inboxes");
    if !inboxes_dir.exists() {
        std::fs::create_dir_all(&inboxes_dir)?;
    }
    let inbox_path = inboxes_dir.join(format!("{to_agent}.json"));
    let _ = inbox_append(&inbox_path, &inbox_message, &to_team, &to_agent)?;

    // Poll sender inbox for response containing the request id
    let sender_inbox = from_team_dir
        .join("inboxes")
        .join(format!("{from_agent}.json"));
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    let poll_interval = Duration::from_millis(args.poll_interval);

    loop {
        if let Some(msg) = read_and_mark_response(
            &sender_inbox,
            &from_team,
            &from_agent,
            &to_agent,
            &request_id,
        )? {
            let elapsed = start.elapsed();
            let elapsed_ms = elapsed.as_millis();
            println!(
                "Response from {}@{} ({} ms): {}",
                to_agent,
                to_team,
                elapsed_ms,
                one_line(&msg.text)
            );
            return Ok(());
        }

        if Instant::now() >= deadline {
            let elapsed = start.elapsed();
            anyhow::bail!(
                "Timed out after {}s ({} ms) waiting for response in {}@{}",
                args.timeout,
                elapsed.as_millis(),
                from_agent,
                from_team
            );
        }

        sleep(poll_interval);
    }
}

fn read_and_mark_response(
    inbox_path: &std::path::Path,
    team: &str,
    agent: &str,
    expected_from: &str,
    request_id: &str,
) -> Result<Option<InboxMessage>> {
    if !inbox_path.exists() {
        return Ok(None);
    }

    let mut found: Option<InboxMessage> = None;
    inbox_update(inbox_path, team, agent, |messages| {
        for msg in messages.iter_mut().rev() {
            if found.is_none()
                && msg.from == expected_from
                && msg.text.contains(request_id)
            {
                msg.read = true;
                found = Some(msg.clone());
                break;
            }
        }
    })?;

    Ok(found)
}

fn generate_summary(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "(empty message)".to_string();
    }
    let max_len = 100;
    if trimmed.len() <= max_len {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..max_len])
    }
}

fn one_line(text: &str) -> String {
    let single = text.replace(['\n', '\r'], " ");
    if single.len() > 120 {
        format!("{}...", &single[..120])
    } else {
        single
    }
}
