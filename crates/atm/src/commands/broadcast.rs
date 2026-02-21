//! Broadcast command implementation

use anyhow::Result;
use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::io::inbox::{WriteOutcome, inbox_append};
use agent_team_mail_core::schema::{InboxMessage, TeamConfig};
use chrono::Utc;
use clap::Args;
use std::collections::HashMap;
use uuid::Uuid;

use agent_team_mail_core::text::{truncate_chars_slice, validate_message_text, DEFAULT_MAX_MESSAGE_BYTES};

use crate::util::settings::get_home_dir;

/// Broadcast a message to all agents in a team
#[derive(Args, Debug)]
pub struct BroadcastArgs {
    /// Message text (or omit to use --stdin)
    message: Option<String>,

    /// Override default team
    #[arg(long)]
    team: Option<String>,

    /// Read message from stdin
    #[arg(long)]
    stdin: bool,

    /// Explicit summary (otherwise auto-generated)
    #[arg(long)]
    summary: Option<String>,

    /// Output as JSON
    #[arg(long)]
    json: bool,

    /// Show what would be written without actually writing
    #[arg(long)]
    dry_run: bool,

    /// Override sender identity (default: ATM_IDENTITY env or config identity)
    #[arg(long)]
    from: Option<String>,
}

/// Delivery status for a single agent
#[derive(Debug)]
struct DeliveryStatus {
    agent_name: String,
    outcome: Result<WriteOutcome>,
}

/// Execute the broadcast command
pub fn execute(args: BroadcastArgs) -> Result<()> {
    // Resolve configuration
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };

    let mut config = resolve_config(&overrides, &current_dir, &home_dir)?;

    // Override sender identity if --from provided
    if let Some(ref from) = args.from {
        config.core.identity = from.clone();
    }

    // Determine target team
    let team_name = args.team.as_ref().unwrap_or(&config.core.default_team);

    // Resolve team directory
    let team_dir = home_dir.join(".claude/teams").join(team_name);
    if !team_dir.exists() {
        anyhow::bail!("Team '{team_name}' not found (directory {team_dir:?} doesn't exist)");
    }

    // Load team config to get member list
    let team_config_path = team_dir.join("config.json");
    if !team_config_path.exists() {
        anyhow::bail!("Team config not found at {team_config_path:?}");
    }

    let team_config: TeamConfig =
        serde_json::from_str(&std::fs::read_to_string(&team_config_path)?)?;

    // Get message text from appropriate source
    let message_text = get_message_text(&args)?;

    validate_message_text(&message_text, DEFAULT_MAX_MESSAGE_BYTES)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Generate summary
    let summary = args
        .summary
        .unwrap_or_else(|| generate_summary(&message_text));

    // Create inbox message
    let inbox_message = InboxMessage {
        from: config.core.identity.clone(),
        text: message_text.clone(),
        timestamp: Utc::now().to_rfc3339(),
        read: false,
        summary: Some(summary.clone()),
        message_id: Some(Uuid::new_v4().to_string()),
        unknown_fields: HashMap::new(),
    };

    // Collect target agents (all members except self)
    let target_agents: Vec<String> = team_config
        .members
        .iter()
        .filter(|m| m.name != config.core.identity)
        .map(|m| m.name.clone())
        .collect();

    if target_agents.is_empty() {
        anyhow::bail!("No agents to broadcast to (team has no other members besides self)");
    }

    // Dry run output
    if args.dry_run {
        if args.json {
            let output = serde_json::json!({
                "action": "broadcast",
                "team": team_name,
                "targets": target_agents,
                "message": inbox_message,
                "dry_run": true
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("Dry run - would broadcast message:");
            println!("  Team: {team_name}");
            println!("  From: {}", inbox_message.from);
            println!("  Targets: {}", target_agents.join(", "));
            println!("  Summary: {summary}");
            println!("  Message: {message_text}");
        }
        return Ok(());
    }

    // Ensure inboxes directory exists
    let inboxes_dir = team_dir.join("inboxes");
    if !inboxes_dir.exists() {
        std::fs::create_dir_all(&inboxes_dir)?;
    }

    // Broadcast to all target agents
    let mut delivery_statuses: Vec<DeliveryStatus> = Vec::new();

    for agent_name in &target_agents {
        let inbox_path = inboxes_dir.join(format!("{agent_name}.json"));
        let outcome = inbox_append(&inbox_path, &inbox_message, team_name, agent_name)
            .map_err(|e| anyhow::anyhow!(e));

        delivery_statuses.push(DeliveryStatus {
            agent_name: agent_name.clone(),
            outcome,
        });
    }

    // Output results
    if args.json {
        output_json_results(&delivery_statuses, team_name, &inbox_message)?;
    } else {
        output_human_results(&delivery_statuses, team_name)?;
    }

    // Return error if any deliveries failed
    let failed_count = delivery_statuses
        .iter()
        .filter(|s| s.outcome.is_err())
        .count();
    emit_event_best_effort(EventFields {
        level: if failed_count > 0 { "warn" } else { "info" },
        source: "atm",
        action: "broadcast",
        team: Some(team_name.to_string()),
        agent_id: Some(config.core.identity.clone()),
        count: Some(target_agents.len() as u64),
        result: Some(if failed_count > 0 {
            format!("partial_failure:{failed_count}")
        } else {
            "ok".to_string()
        }),
        ..Default::default()
    });

    if failed_count > 0 {
        anyhow::bail!("Broadcast completed with {failed_count} failed deliveries");
    }

    Ok(())
}

/// Get message text from args or stdin
fn get_message_text(args: &BroadcastArgs) -> Result<String> {
    if args.stdin {
        // Read from stdin
        use std::io::Read;
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        Ok(buffer)
    } else if let Some(ref message) = args.message {
        // Direct message argument
        Ok(message.clone())
    } else {
        anyhow::bail!("Message required: provide message text or --stdin");
    }
}

/// Generate summary from message text (first ~100 chars)
fn generate_summary(text: &str) -> String {
    const MAX_LEN: usize = 100;

    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX_LEN {
        trimmed.to_string()
    } else {
        let slice = truncate_chars_slice(trimmed, MAX_LEN);
        if let Some(pos) = slice.rfind(|c: char| c.is_whitespace()) {
            format!("{}...", slice[..pos].trim())
        } else {
            format!("{slice}...")
        }
    }
}

/// Output results in JSON format
fn output_json_results(
    statuses: &[DeliveryStatus],
    team_name: &str,
    inbox_message: &InboxMessage,
) -> Result<()> {
    let mut successes = Vec::new();
    let mut failures = Vec::new();
    let mut queued = Vec::new();
    let mut conflicts = Vec::new();

    for status in statuses {
        match &status.outcome {
            Ok(WriteOutcome::Success) => {
                successes.push(&status.agent_name);
            }
            Ok(WriteOutcome::ConflictResolved { merged_messages }) => {
                conflicts.push(serde_json::json!({
                    "agent": &status.agent_name,
                    "merged_messages": merged_messages,
                }));
            }
            Ok(WriteOutcome::Queued { spool_path }) => {
                queued.push(serde_json::json!({
                    "agent": &status.agent_name,
                    "spool_path": spool_path,
                }));
            }
            Err(e) => {
                failures.push(serde_json::json!({
                    "agent": &status.agent_name,
                    "error": e.to_string(),
                }));
            }
        }
    }

    let output = serde_json::json!({
        "action": "broadcast",
        "team": team_name,
        "message_id": inbox_message.message_id,
        "summary": {
            "total": statuses.len(),
            "succeeded": successes.len(),
            "queued": queued.len(),
            "conflicts": conflicts.len(),
            "failed": failures.len(),
        },
        "successes": successes,
        "conflicts": conflicts,
        "queued": queued,
        "failures": failures,
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Output results in human-readable format
fn output_human_results(statuses: &[DeliveryStatus], team_name: &str) -> Result<()> {
    let mut success_count = 0;
    let mut conflict_count = 0;
    let mut queued_count = 0;
    let mut failed_count = 0;

    for status in statuses {
        match &status.outcome {
            Ok(WriteOutcome::Success) => {
                success_count += 1;
            }
            Ok(WriteOutcome::ConflictResolved { merged_messages }) => {
                conflict_count += 1;
                println!(
                    "  {} - delivered (merged {} concurrent messages)",
                    status.agent_name, merged_messages
                );
            }
            Ok(WriteOutcome::Queued { spool_path }) => {
                queued_count += 1;
                eprintln!("  {} - queued (spool: {spool_path:?})", status.agent_name);
            }
            Err(e) => {
                failed_count += 1;
                eprintln!("  {} - FAILED: {}", status.agent_name, e);
            }
        }
    }

    // Summary line
    if failed_count == 0 && queued_count == 0 && conflict_count == 0 {
        println!("Broadcast sent to {success_count} agents in team '{team_name}'");
    } else {
        println!(
            "Broadcast completed for team '{team_name}': {success_count} succeeded, {conflict_count} conflicts resolved, {queued_count} queued, {failed_count} failed"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_summary_short() {
        let text = "Short broadcast message";
        assert_eq!(generate_summary(text), "Short broadcast message");
    }

    #[test]
    fn test_generate_summary_long() {
        let text = "This is a very long broadcast message that exceeds the maximum summary length and should be truncated at a reasonable point with an ellipsis indicator";
        let summary = generate_summary(text);
        assert!(summary.len() <= 104); // 100 + "..."
        assert!(summary.ends_with("..."));
    }

    #[test]
    fn test_generate_summary_whitespace() {
        let text = "   Broadcast message with whitespace   ";
        assert_eq!(generate_summary(text), "Broadcast message with whitespace");
    }
}
