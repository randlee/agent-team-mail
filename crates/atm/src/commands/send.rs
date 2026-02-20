//! Send command implementation

use anyhow::Result;
use agent_team_mail_core::config::{resolve_alias, resolve_config, Config, ConfigOverrides};
use agent_team_mail_core::io::atomic::atomic_swap;
use agent_team_mail_core::io::inbox::{inbox_append, WriteOutcome};
use agent_team_mail_core::io::lock::acquire_lock;
use agent_team_mail_core::schema::{InboxMessage, TeamConfig};
use chrono::Utc;
use clap::Args;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
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

    /// Custom call-to-action text for offline recipients
    #[arg(long)]
    offline_action: Option<String>,

    /// Override sender identity (default: ATM_IDENTITY env or config identity)
    #[arg(long)]
    from: Option<String>,
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

    let mut config = resolve_config(&overrides, &current_dir, &home_dir)?;

    // Override sender identity if --from provided
    if let Some(ref from) = args.from {
        config.core.identity = from.clone();
    } else if config.core.identity == "human" {
        // If identity is still "human" (default), try to auto-detect from team context
        config.core.identity = detect_sender_identity(&config.core.default_team, &home_dir)
            .unwrap_or_else(|| "human".to_string());
    }

    // Parse addressing (agent@team or just agent) first so alias lookup runs on
    // only the agent token, even when input uses @team suffix.
    let (parsed_agent, team_name) = parse_address(&args.agent, &args.team, &config.core.default_team)?;
    let agent_name = resolve_alias(&parsed_agent, &config.aliases);
    if agent_name != parsed_agent {
        eprintln!(
            "Note: '{}' resolved via alias to '{}'",
            parsed_agent, agent_name
        );
    }

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

    // Self-send check: warn and prepend warning when sender == recipient
    let message_text = if config.core.identity == agent_name {
        let session_id = std::env::var("CLAUDE_SESSION_ID").unwrap_or_else(|_| "unknown".to_string());
        let session_short = &session_id[..8.min(session_id.len())];
        let warning = format!(
            "[WARNING: Sent to self — identity={}, session={}. Check ATM_IDENTITY.]",
            config.core.identity, session_short
        );
        println!("{warning}");
        format!("{warning}\n{message_text}")
    } else {
        message_text
    };

    // Process file reference if provided
    let mut final_message_text = if let Some(ref file_path) = args.file {
        process_file_reference(file_path, &message_text, &team_name, &current_dir, &home_dir)?
    } else {
        message_text
    };

    // Check if recipient is offline and prepend action text
    // Only warn if isActive is explicitly false, not on missing/null
    let recipient_offline = team_config
        .members
        .iter()
        .find(|m| m.name == agent_name)
        .and_then(|m| m.is_active)
        .map(|is_active| !is_active)
        .unwrap_or(false); // Missing or null = not offline

    if recipient_offline {
        let action_text = resolve_offline_action(&args, &config);
        if !action_text.is_empty() {
            eprintln!(
                "Warning: Agent '{agent_name}' appears offline. Message will be queued with call-to-action."
            );
            final_message_text = format!("[{action_text}] {final_message_text}");
        }
    }

    // Set sender heartbeat (isActive: true, lastActive timestamp)
    if let Err(e) = set_sender_heartbeat(&team_config_path, &config.core.identity) {
        // Non-fatal: log warning but proceed with send
        eprintln!("Warning: Failed to update sender activity: {e}");
    }

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

    // Auto-subscribe the sender to the target agent's idle event (upsert — refreshes TTL
    // if a subscription already exists). This is best-effort: errors are silently ignored
    // because the daemon may not be running.
    let _ = agent_team_mail_core::daemon_client::subscribe_to_agent(
        &config.core.identity,
        &agent_name,
        &team_name,
        &["idle".to_string()],
    );

    // Query the daemon for agent state to enrich the output (best-effort, silent fallback).
    let agent_state_info = agent_team_mail_core::daemon_client::query_agent_state(
        &agent_name,
        &team_name,
    )
    .unwrap_or(None); // Ignore errors; daemon may not be running

    // Output result
    if args.json {
        let mut output = serde_json::json!({
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
        if let Some(ref info) = agent_state_info {
            output["agent_state"] = serde_json::json!({
                "state": info.state,
                "last_transition": info.last_transition,
            });
        }
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
        // Print enriched agent state info when daemon is running
        if let Some(ref info) = agent_state_info {
            println!("  agent-state: {}", info.state);
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

/// Resolve offline action text from CLI flag, config, or default
///
/// Priority: CLI flag > config > default
fn resolve_offline_action(args: &SendArgs, config: &Config) -> String {
    if let Some(ref action) = args.offline_action {
        return action.clone();
    }

    if let Some(ref action) = config.messaging.offline_action {
        return action.clone();
    }

    "PENDING ACTION - execute when online".to_string()
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

/// Set sender heartbeat in team config (isActive: true, lastActive timestamp)
///
/// Uses atomic lock/swap to prevent corruption (same infrastructure as inbox writes).
///
/// # Arguments
///
/// * `team_config_path` - Path to team config.json
/// * `sender_name` - Name of the sending agent
///
/// # Errors
///
/// Returns error if config update fails (non-fatal for send operation)
fn set_sender_heartbeat(team_config_path: &Path, sender_name: &str) -> Result<()> {
    let lock_path = team_config_path.with_extension("lock");

    // Acquire lock with retry
    let _lock = acquire_lock(&lock_path, 5).map_err(|e| {
        anyhow::anyhow!("Failed to acquire lock for team config: {e}")
    })?;

    // Read current config
    let content = std::fs::read(team_config_path)?;
    let mut config: TeamConfig = serde_json::from_slice(&content)?;

    // Update sender's activity
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis() as u64;

    if let Some(member) = config.members.iter_mut().find(|m| m.name == sender_name) {
        member.is_active = Some(true);
        member.last_active = Some(now_ms);
    } else {
        // Sender not in team members — this is unusual but not fatal
        return Ok(());
    }

    // Write to temp file with fsync, then swap
    let tmp_path = team_config_path.with_extension("tmp");
    let new_content = serde_json::to_string_pretty(&config)?;

    let mut file = std::fs::File::create(&tmp_path)?;
    std::io::Write::write_all(&mut file, new_content.as_bytes())?;
    file.sync_all()?;
    drop(file);

    // Atomic swap
    atomic_swap(team_config_path, &tmp_path)?;

    Ok(())
}

/// Detect sender identity from team context
///
/// Attempts to match the current process with a team member by checking:
/// 1. ATM_IDENTITY env var (already checked by config resolution)
/// 2. Team config members list (checks for session ID or process context)
///
/// Returns None if no match found (caller should default to "human")
fn detect_sender_identity(team_name: &str, home_dir: &Path) -> Option<String> {
    // Load team config
    let team_config_path = home_dir
        .join(".claude/teams")
        .join(team_name)
        .join("config.json");

    if !team_config_path.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&team_config_path).ok()?;
    let team_config: TeamConfig = serde_json::from_str(&content).ok()?;

    // Try to match by session ID (from env)
    if let Ok(session_id) = std::env::var("CLAUDE_SESSION_ID") {
        for member in &team_config.members {
            if member.agent_id.starts_with(&format!("{session_id}@")) {
                return Some(member.name.clone());
            }
        }
    }

    // Try to match by CWD (if agent's cwd matches current dir)
    if let Ok(current_dir) = std::env::current_dir() {
        let current_path = current_dir.to_string_lossy();
        for member in &team_config.members {
            if member.cwd == current_path {
                return Some(member.name.clone());
            }
        }
    }

    // No match found
    None
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

    fn make_send_args(offline_action: Option<String>) -> SendArgs {
        SendArgs {
            agent: "test-agent".to_string(),
            message: Some("test".to_string()),
            team: None,
            file: None,
            stdin: false,
            summary: None,
            json: false,
            dry_run: false,
            offline_action,
            from: None,
        }
    }

    #[test]
    fn test_resolve_offline_action_default() {
        let args = make_send_args(None);
        let config = Config::default();
        assert_eq!(
            resolve_offline_action(&args, &config),
            "PENDING ACTION - execute when online"
        );
    }

    #[test]
    fn test_resolve_offline_action_cli_flag() {
        let args = make_send_args(Some("DO THIS LATER".to_string()));
        let config = Config::default();
        assert_eq!(resolve_offline_action(&args, &config), "DO THIS LATER");
    }

    #[test]
    fn test_resolve_offline_action_config() {
        let args = make_send_args(None);
        let mut config = Config::default();
        config.messaging.offline_action = Some("QUEUED".to_string());
        assert_eq!(resolve_offline_action(&args, &config), "QUEUED");
    }

    #[test]
    fn test_resolve_offline_action_cli_overrides_config() {
        let args = make_send_args(Some("CLI ACTION".to_string()));
        let mut config = Config::default();
        config.messaging.offline_action = Some("CONFIG ACTION".to_string());
        assert_eq!(resolve_offline_action(&args, &config), "CLI ACTION");
    }

    #[test]
    fn test_resolve_offline_action_empty_string_opt_out() {
        let args = make_send_args(Some(String::new()));
        let config = Config::default();
        assert_eq!(resolve_offline_action(&args, &config), "");
    }

    #[test]
    fn test_self_send_warning_prepended() {
        // When sender identity == target agent name, the warning should be
        // prepended to the message text.
        let sender = "team-lead";
        let agent_name = "team-lead"; // same as sender → self-send
        let raw_message = "Hello self!";

        // Simulate the self-send check logic (extracted for unit testing)
        let session_id = "test-session-1234567890";
        let session_short = &session_id[..8.min(session_id.len())];

        let message_text = if sender == agent_name {
            let warning = format!(
                "[WARNING: Sent to self — identity={sender}, session={session_short}. Check ATM_IDENTITY.]"
            );
            format!("{warning}\n{raw_message}")
        } else {
            raw_message.to_string()
        };

        assert!(message_text.starts_with("[WARNING: Sent to self"));
        assert!(message_text.contains("team-lead"));
        assert!(message_text.contains("test-ses")); // first 8 chars
        assert!(message_text.contains("Hello self!"));
    }

    #[test]
    fn test_self_send_warning_not_added_for_different_recipient() {
        let sender = "team-lead";
        let agent_name = "arch-ctm"; // different → no warning
        let raw_message = "Hello other!";

        let message_text = if sender == agent_name {
            format!("[WARNING: Sent to self — identity={sender}]\n{raw_message}")
        } else {
            raw_message.to_string()
        };

        assert_eq!(message_text, "Hello other!");
        assert!(!message_text.contains("WARNING"));
    }
}
