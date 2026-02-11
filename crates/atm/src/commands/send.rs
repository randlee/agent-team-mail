//! Send command implementation

use anyhow::Result;
use atm_core::config::{resolve_config, ConfigOverrides};
use atm_core::io::inbox::{inbox_append, WriteOutcome};
use atm_core::schema::{InboxMessage, TeamConfig};
use chrono::Utc;
use clap::Args;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::util::addressing::parse_address;
use crate::util::file_policy::check_file_reference;
use crate::util::settings::get_home_dir;

/// Send a message to a specific agent
#[derive(Args, Debug)]
pub struct SendArgs {
    /// Target agent (name or name@team)
    agent: String,

    /// Message text (or omit to use --file or --stdin)
    message: Option<String>,

    /// Override default team
    #[arg(long)]
    team: Option<String>,

    /// Read message from file (file path is passed as reference)
    #[arg(long, conflicts_with = "stdin")]
    file: Option<PathBuf>,

    /// Read message from stdin
    #[arg(long, conflicts_with = "file")]
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
}

/// Execute the send command
pub fn execute(args: SendArgs) -> Result<()> {
    // Resolve configuration
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };

    let config = resolve_config(&overrides, &current_dir, &home_dir)?;

    // Parse addressing (agent@team or just agent)
    let (agent_name, team_name) = parse_address(&args.agent, &args.team, &config.core.default_team)?;

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

    // Get message text from appropriate source
    let message_text = get_message_text(&args)?;

    // Process file reference if provided
    let final_message_text = if let Some(ref file_path) = args.file {
        process_file_reference(file_path, &message_text, &team_name, &current_dir, &home_dir)?
    } else {
        message_text
    };

    // Generate summary
    let summary = args.summary.unwrap_or_else(|| generate_summary(&final_message_text));

    // Create inbox message
    let inbox_message = InboxMessage {
        from: config.core.identity.clone(),
        text: final_message_text.clone(),
        timestamp: Utc::now().to_rfc3339(),
        read: false,
        summary: Some(summary.clone()),
        message_id: Some(Uuid::new_v4().to_string()),
        unknown_fields: HashMap::new(),
    };

    // Dry run output
    if args.dry_run {
        if args.json {
            let output = serde_json::json!({
                "action": "send",
                "agent": agent_name,
                "team": team_name,
                "message": inbox_message,
                "dry_run": true
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("Dry run - would send message:");
            println!("  To: {agent_name}@{team_name}");
            println!("  From: {}", inbox_message.from);
            println!("  Summary: {summary}");
            println!("  Message: {final_message_text}");
        }
        return Ok(());
    }

    // Write to inbox
    let inbox_path = team_dir.join("inboxes").join(format!("{agent_name}.json"));

    // Ensure inboxes directory exists
    let inboxes_dir = team_dir.join("inboxes");
    if !inboxes_dir.exists() {
        std::fs::create_dir_all(&inboxes_dir)?;
    }

    let outcome = inbox_append(&inbox_path, &inbox_message, &team_name, &agent_name)?;

    // Output result
    if args.json {
        let output = serde_json::json!({
            "action": "send",
            "agent": agent_name,
            "team": team_name,
            "outcome": match outcome {
                WriteOutcome::Success => "success",
                WriteOutcome::ConflictResolved { .. } => "conflict_resolved",
                WriteOutcome::Queued { .. } => "queued",
            },
            "message_id": inbox_message.message_id,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        match outcome {
            WriteOutcome::Success => {
                println!("Message sent to {agent_name}@{team_name}");
            }
            WriteOutcome::ConflictResolved { merged_messages } => {
                println!("Message sent to {agent_name}@{team_name} (merged {merged_messages} concurrent messages)");
            }
            WriteOutcome::Queued { ref spool_path } => {
                eprintln!("Warning: Message queued for delivery (could not write to inbox immediately)");
                eprintln!("Spool path: {spool_path:?}");
            }
        }
    }

    Ok(())
}

/// Get message text from args, stdin, or file
fn get_message_text(args: &SendArgs) -> Result<String> {
    if args.stdin {
        // Read from stdin
        use std::io::Read;
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        Ok(buffer)
    } else if let Some(ref message) = args.message {
        // Direct message argument
        Ok(message.clone())
    } else if args.file.is_some() {
        // Message is file path reference
        Ok(String::new())
    } else {
        anyhow::bail!("Message required: provide message text, --file, or --stdin");
    }
}

/// Process file reference and check access policy
fn process_file_reference(
    file_path: &Path,
    message_text: &str,
    team_name: &str,
    current_dir: &Path,
    home_dir: &Path,
) -> Result<String> {
    // Verify file exists
    if !file_path.exists() {
        anyhow::bail!("File not found: {file_path:?}");
    }

    // Check file reference policy
    let (is_allowed, rewritten_message) = check_file_reference(
        file_path,
        message_text,
        team_name,
        current_dir,
        home_dir,
    )?;

    if is_allowed {
        // File path is allowed - use as reference
        Ok(if message_text.is_empty() {
            format!("File reference: {}", file_path.display())
        } else {
            format!("{}\n\nFile reference: {}", message_text, file_path.display())
        })
    } else {
        // File was copied to share directory - use rewritten message
        Ok(rewritten_message)
    }
}

/// Generate summary from message text (first ~100 chars)
fn generate_summary(text: &str) -> String {
    const MAX_LEN: usize = 100;

    let trimmed = text.trim();
    if trimmed.len() <= MAX_LEN {
        trimmed.to_string()
    } else {
        // Find a good break point (space, newline)
        let truncated = &trimmed[..MAX_LEN];
        if let Some(pos) = truncated.rfind(|c: char| c.is_whitespace()) {
            format!("{}...", truncated[..pos].trim())
        } else {
            format!("{truncated}...")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_summary_short() {
        let text = "Short message";
        assert_eq!(generate_summary(text), "Short message");
    }

    #[test]
    fn test_generate_summary_long() {
        let text = "This is a very long message that exceeds the maximum summary length and should be truncated at a reasonable point with an ellipsis indicator";
        let summary = generate_summary(text);
        assert!(summary.len() <= 104); // 100 + "..."
        assert!(summary.ends_with("..."));
    }

    #[test]
    fn test_generate_summary_whitespace() {
        let text = "   Message with whitespace   ";
        assert_eq!(generate_summary(text), "Message with whitespace");
    }
}
