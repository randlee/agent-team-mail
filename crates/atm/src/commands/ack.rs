use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use anyhow::Result;
use chrono::Utc;
use clap::Args;
use std::collections::HashSet;

use crate::util::hook_identity::read_hook_file_identity;
use crate::util::settings::get_home_dir;

/// Acknowledge previously-read ATM messages as actioned/completed.
#[derive(Args, Debug)]
pub struct AckArgs {
    /// Message IDs to acknowledge in your own inbox
    message_ids: Vec<String>,

    /// Override default team
    #[arg(long)]
    team: Option<String>,

    /// Acknowledge all pending messages in your own inbox
    #[arg(long)]
    all_pending: bool,

    /// Override reader identity (default: hook file → ATM_IDENTITY → .atm.toml → reject)
    #[arg(long = "as", value_name = "NAME")]
    reader_as: Option<String>,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

pub fn execute(args: AckArgs) -> Result<()> {
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
            Ok(Some(identity)) => config.core.identity = identity,
            Ok(None) => {
                anyhow::bail!(
                    "Cannot determine reader identity: hook file not found. Use --as <name> to specify identity explicitly."
                );
            }
            Err(e) => {
                anyhow::bail!(
                    "Cannot determine reader identity: hook file validation failed: {e}. Use --as <name> to specify identity explicitly."
                );
            }
        }
    }

    if !args.all_pending && args.message_ids.is_empty() {
        anyhow::bail!("Provide at least one message ID or use --all-pending");
    }

    let team_name = config.core.default_team.clone();
    let agent_name = config.core.identity.clone();
    let team_dir = home_dir.join(".claude/teams").join(&team_name);
    let inbox_path = team_dir.join("inboxes").join(format!("{agent_name}.json"));

    if !inbox_path.exists() {
        anyhow::bail!("Inbox not found for {agent_name}@{team_name}");
    }

    let ids: HashSet<String> = args.message_ids.iter().cloned().collect();
    let acknowledged_at = Utc::now().to_rfc3339();
    let mut matched = 0usize;
    let mut changed = 0usize;

    agent_team_mail_core::io::inbox::inbox_update(&inbox_path, &team_name, &agent_name, |msgs| {
        for msg in msgs.iter_mut() {
            let selected = if args.all_pending {
                msg.is_pending_action()
            } else {
                msg.message_id
                    .as_ref()
                    .is_some_and(|message_id| ids.contains(message_id))
            };

            if selected {
                matched += 1;
                if !msg.is_acknowledged() {
                    msg.read = true;
                    msg.mark_acknowledged(acknowledged_at.clone());
                    changed += 1;
                }
            }
        }
    })?;

    if !args.all_pending && matched == 0 {
        anyhow::bail!("No matching message IDs found in {agent_name}@{team_name}");
    }

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "ack",
        team: Some(team_name.clone()),
        session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
        agent_id: Some(agent_name.clone()),
        agent_name: Some(agent_name.clone()),
        result: Some("ok".to_string()),
        count: Some(changed as u64),
        ..Default::default()
    });

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "action": "ack",
                "team": team_name,
                "agent": agent_name,
                "matched": matched,
                "acknowledged": changed,
                "allPending": args.all_pending,
            }))?
        );
    } else if args.all_pending {
        println!("Acknowledged {changed} pending message(s) for {agent_name}@{team_name}");
    } else {
        println!(
            "Acknowledged {changed} of {matched} selected message(s) for {agent_name}@{team_name}"
        );
    }

    Ok(())
}
