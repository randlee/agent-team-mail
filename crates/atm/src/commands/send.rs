//! Send command implementation

use agent_team_mail_core::config::{Config, ConfigOverrides, resolve_config, resolve_identity};
use agent_team_mail_core::daemon_client::{RegisterHintOutcome, SessionQueryResult};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::home::{config_team_dir_for, get_os_home_dir};
use agent_team_mail_core::io::inbox::{WriteOutcome, inbox_append};
use agent_team_mail_core::schema::{AgentMember, BackendType, InboxMessage, TeamConfig};
use anyhow::Result;
use chrono::Utc;
use clap::Args;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info};
use uuid::Uuid;

use agent_team_mail_core::text::{
    DEFAULT_MAX_MESSAGE_BYTES, truncate_chars_slice, validate_message_text,
};

use crate::consts::MESSAGE_MAX_LEN;
use crate::util::addressing::parse_address;
use crate::util::caller_identity::resolve_caller_session_id_optional;
use crate::util::file_policy::check_file_reference;
use crate::util::hook_identity::{read_hook_file, read_hook_file_identity};
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
    debug!("send command start");
    // Resolve configuration
    let home_dir = get_home_dir()?;
    let config_home = get_os_home_dir()?;
    let current_dir = std::env::current_dir()?;
    let sender_config = resolve_config(&ConfigOverrides::default(), &current_dir, &config_home)?;
    let sender_team = sender_config.core.default_team.clone();

    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };

    let mut config = resolve_config(&overrides, &current_dir, &config_home)?;

    // Override sender identity if --from provided; otherwise resolve via hook file.
    if let Some(ref from) = args.from {
        // --from always wins.
        config.core.identity = from.clone();
    } else if config.core.identity == "human" {
        // Default identity — must resolve via PID-based hook file.
        match read_hook_file_identity() {
            Ok(Some(identity)) => {
                config.core.identity = identity;
            }
            Ok(None) => {
                anyhow::bail!(
                    "Cannot determine sender identity: hook file not found. \
                     Ensure the atm-identity-write.py PreToolUse hook is configured in \
                     .claude/settings.json, or use --from <name> to specify identity explicitly."
                );
            }
            Err(e) => {
                anyhow::bail!(
                    "Cannot determine sender identity: hook file validation failed: {e}. \
                     Use --from <name> to specify identity explicitly."
                );
            }
        }
    }
    // else: identity was explicitly configured (ATM_IDENTITY, .atm.toml) — use as-is.

    // Parse addressing (agent@team or just agent) first so alias lookup runs on
    // only the agent token, even when input uses @team suffix.
    let (parsed_agent, team_name) =
        parse_address(&args.agent, &args.team, &config.core.default_team)?;
    let agent_name = resolve_identity(&parsed_agent, &config.roles, &config.aliases);
    if agent_name != parsed_agent {
        eprintln!(
            "Note: '{}' resolved via roles/alias to '{}'",
            parsed_agent, agent_name
        );
    }

    // Resolve team directory
    let team_dir = config_team_dir_for(&config_home, &team_name);
    if !team_dir.exists() {
        anyhow::bail!("Team '{team_name}' not found (directory {team_dir:?} doesn't exist)");
    }

    // Load team config to verify agent exists
    let team_config_path = team_dir.join("config.json");
    if !team_config_path.exists() {
        anyhow::bail!("Team config not found at {team_config_path:?}");
    }

    let team_config: TeamConfig =
        serde_json::from_str(&std::fs::read_to_string(&team_config_path)?)?;

    // Verify agent exists in team
    if !team_config.members.iter().any(|m| m.name == agent_name) {
        anyhow::bail!("Agent '{agent_name}' not found in team '{team_name}'");
    }

    // Get message text from appropriate source
    let message_text = get_message_text(&args)?;

    // Resolve sender session once so concurrent same-identity sessions can be
    // disambiguated deterministically.
    let sender_session_id =
        resolve_sender_session_id_with_context(Some(&sender_team), Some(&config.core.identity))?;

    // Self-send check: warn and prepend warning only when sender/recipient are
    // same identity in same team and daemon session ownership points to this
    // session (or ownership is unknown).
    let message_text = if should_warn_self_send(
        &config.core.identity,
        &sender_team,
        &agent_name,
        &team_name,
        sender_session_id.as_deref(),
    ) {
        let session_id = sender_session_id
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| "unknown".to_string());
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
        process_file_reference(
            file_path,
            &message_text,
            &team_name,
            &current_dir,
            &home_dir,
        )?
    } else {
        message_text
    };

    // Check if recipient is offline and prepend action text.
    // Liveness truth comes from daemon session state; isActive is activity-only.
    let recipient_offline = recipient_has_dead_session(&team_name, &agent_name);

    if recipient_offline {
        let action_text = resolve_offline_action(&args, &config);
        if !action_text.is_empty() {
            eprintln!(
                "Warning: Agent '{agent_name}' appears offline. Message will be queued with call-to-action."
            );
            final_message_text = format!("[{action_text}] {final_message_text}");
        }
    }

    // Validate final payload text after all expansions/rewrites.
    validate_message_text(&final_message_text, DEFAULT_MAX_MESSAGE_BYTES)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    register_sender_hint(
        &team_name,
        &config.core.identity,
        &team_config,
        sender_session_id.as_deref(),
    )?;

    // Generate summary
    let summary = args
        .summary
        .unwrap_or_else(|| generate_summary(&final_message_text));

    // Create inbox message
    let inbox_message = build_inbox_message(
        config.core.identity.clone(),
        Some(sender_team.clone()),
        final_message_text.clone(),
        Some(summary.clone()),
    );

    // Dry run output
    if args.dry_run {
        let destination = destination_target(&agent_name, &team_name);
        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm",
            action: "send_dry_run",
            team: Some(team_name.clone()),
            session_id: sender_session_id.clone(),
            agent_id: Some(config.core.identity.clone()),
            agent_name: Some(config.core.identity.clone()),
            target: Some(destination),
            result: Some("ok".to_string()),
            message_text: Some(final_message_text.clone()),
            ..Default::default()
        });
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
    let (result_text, conflict_count): (&str, Option<u64>) = match &outcome {
        WriteOutcome::Success => ("success", None),
        WriteOutcome::ConflictResolved { merged_messages } => {
            ("conflict_resolved", Some(*merged_messages as u64))
        }
        WriteOutcome::Queued { .. } => ("queued", None),
    };
    let destination = destination_target(&agent_name, &team_name);
    let sender_pid = team_config
        .members
        .iter()
        .find(|m| m.name == config.core.identity)
        .and_then(detect_sender_process_pid);
    let recipient_pid = member_pid_hint(&team_config, &agent_name);
    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "send",
        team: Some(team_name.clone()),
        session_id: sender_session_id.clone(),
        agent_id: Some(config.core.identity.clone()),
        agent_name: Some(config.core.identity.clone()),
        target: Some(destination),
        result: Some(result_text.to_string()),
        message_id: inbox_message.message_id.clone(),
        count: conflict_count,
        message_text: Some(final_message_text.clone()),
        sender_agent: Some(config.core.identity.clone()),
        sender_team: Some(sender_team.clone()),
        sender_pid,
        recipient_agent: Some(agent_name.clone()),
        recipient_team: Some(team_name.clone()),
        recipient_pid,
        ..Default::default()
    });
    info!(
        "send outcome: team={} agent={} result={}",
        team_name, agent_name, result_text
    );

    // Best-effort daemon heartbeat when message delivery reaches inbox write
    // success path so session last_seen_at updates are not limited to explicit
    // session-query commands.
    if matches!(
        outcome,
        WriteOutcome::Success | WriteOutcome::ConflictResolved { .. }
    ) {
        let _ = touch_sender_session_heartbeat(&team_name, &config.core.identity);
        if should_emit_post_send_idle_transition() {
            if let Some(ref session_id) = sender_session_id {
                let _ = agent_team_mail_core::daemon_client::emit_teammate_idle_best_effort(
                    &team_name,
                    &config.core.identity,
                    session_id,
                );
            }
        }
    }

    // Query the daemon for agent state to enrich the output (best-effort, silent fallback).
    let agent_state_info =
        agent_team_mail_core::daemon_client::query_agent_state(&agent_name, &team_name)
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
                println!(
                    "Message sent to {agent_name}@{team_name} (merged {merged_messages} concurrent messages)"
                );
            }
            WriteOutcome::Queued { ref spool_path } => {
                eprintln!(
                    "Warning: Message queued for delivery (could not write to inbox immediately)"
                );
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
        // Direct message argument — reject blank messages at CLI layer
        if message.trim().is_empty() {
            anyhow::bail!("Message text cannot be empty");
        }
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
    let (is_allowed, rewritten_message) =
        check_file_reference(file_path, message_text, team_name, current_dir, home_dir)?;

    if is_allowed {
        // File path is allowed - use as reference
        Ok(if message_text.is_empty() {
            format!("File reference: {}", file_path.display())
        } else {
            format!(
                "{}\n\nFile reference: {}",
                message_text,
                file_path.display()
            )
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

    String::new()
}

fn is_self_send(
    sender_identity: &str,
    sender_team: &str,
    recipient_agent: &str,
    recipient_team: &str,
) -> bool {
    sender_identity == recipient_agent && sender_team == recipient_team
}

fn destination_target(agent_name: &str, team_name: &str) -> String {
    format!("{agent_name}@{team_name}")
}

fn register_sender_hint(
    team: &str,
    sender: &str,
    cfg: &TeamConfig,
    sender_session_id: Option<&str>,
) -> Result<()> {
    let Some(member) = cfg.members.iter().find(|m| m.name == sender) else {
        return Ok(());
    };
    let Some(process_id) = detect_sender_process_pid(member) else {
        return Ok(());
    };
    if process_id <= 1 {
        return Ok(());
    }

    let Some(session_id) = sender_session_id.map(str::to_string) else {
        // Never fabricate synthetic session IDs in command paths.
        return Ok(());
    };

    let runtime = member.effective_backend_type().and_then(|bt| match bt {
        BackendType::ClaudeCode => Some("claude"),
        BackendType::Codex => Some("codex"),
        BackendType::Gemini => Some("gemini"),
        BackendType::External | BackendType::Human(_) => None,
    });
    let runtime_session_id = runtime.map(|_| session_id.as_str());

    match agent_team_mail_core::daemon_client::register_hint(
        team,
        sender,
        &session_id,
        process_id,
        runtime,
        runtime_session_id,
        None,
        None,
    ) {
        Ok(RegisterHintOutcome::Registered | RegisterHintOutcome::DaemonUnavailable) => Ok(()),
        Ok(RegisterHintOutcome::UnsupportedDaemon) => {
            eprintln!(
                "Warning: Connected daemon does not support 'register-hint'. \
                 Upgrade atm-daemon to this ATM version and retry; continuing without daemon session sync."
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("Warning: Failed to register daemon session hint: {e}");
            Ok(())
        }
    }
}

fn member_pid_hint(cfg: &TeamConfig, name: &str) -> Option<u32> {
    cfg.members
        .iter()
        .find(|m| m.name == name)
        .and_then(AgentMember::process_id_hint)
}

fn recipient_has_dead_session(team: &str, agent_name: &str) -> bool {
    recipient_has_dead_session_with_query(team, agent_name, |team, agent| {
        agent_team_mail_core::daemon_client::query_session_for_team(team, agent)
    })
}

fn recipient_has_dead_session_with_query<F>(team: &str, agent_name: &str, query: F) -> bool
where
    F: Fn(&str, &str) -> anyhow::Result<Option<SessionQueryResult>>,
{
    match query(team, agent_name) {
        Ok(Some(session)) => !session.alive,
        Ok(None) => false, // Unknown session state; do not label as offline.
        Err(_) => false,   // Best-effort: send must proceed even if daemon query fails.
    }
}

/// Generate summary from message text (first ~100 chars)
pub(crate) fn generate_summary(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= MESSAGE_MAX_LEN {
        trimmed.to_string()
    } else {
        let slice = truncate_chars_slice(trimmed, MESSAGE_MAX_LEN);
        if let Some(pos) = slice.rfind(|c: char| c.is_whitespace()) {
            format!("{}...", slice[..pos].trim())
        } else {
            format!("{slice}...")
        }
    }
}

fn build_inbox_message(
    from: String,
    source_team: Option<String>,
    text: String,
    summary: Option<String>,
) -> InboxMessage {
    InboxMessage {
        from,
        source_team,
        text,
        timestamp: Utc::now().to_rfc3339(),
        read: false,
        summary,
        message_id: Some(Uuid::new_v4().to_string()),
        unknown_fields: HashMap::new(),
    }
}

fn resolve_sender_session_id_with_context(
    team: Option<&str>,
    identity: Option<&str>,
) -> Result<Option<String>> {
    resolve_caller_session_id_optional(team, identity)
}

fn touch_sender_session_heartbeat(team: &str, sender: &str) -> Result<()> {
    touch_sender_session_heartbeat_with_query(team, sender, |team, sender| {
        agent_team_mail_core::daemon_client::query_session_for_team(team, sender)
    })
}

fn touch_sender_session_heartbeat_with_query<F>(team: &str, sender: &str, query: F) -> Result<()>
where
    F: Fn(&str, &str) -> anyhow::Result<Option<SessionQueryResult>>,
{
    let _ = query(team, sender)?;
    Ok(())
}

fn should_emit_post_send_idle_transition() -> bool {
    matches!(
        std::env::var("ATM_RUNTIME")
            .ok()
            .map(|runtime| runtime.trim().to_ascii_lowercase()),
        Some(runtime)
            if matches!(runtime.as_str(), "codex" | "codex-cli" | "gemini" | "gemini-cli" | "opencode")
    )
}

fn should_warn_self_send(
    sender_identity: &str,
    sender_team: &str,
    recipient_agent: &str,
    recipient_team: &str,
    sender_session_id: Option<&str>,
) -> bool {
    should_warn_self_send_with_query(
        sender_identity,
        sender_team,
        recipient_agent,
        recipient_team,
        sender_session_id,
        agent_team_mail_core::daemon_client::query_session_for_team,
    )
}

fn should_warn_self_send_with_query<F>(
    sender_identity: &str,
    sender_team: &str,
    recipient_agent: &str,
    recipient_team: &str,
    sender_session_id: Option<&str>,
    query: F,
) -> bool
where
    F: Fn(&str, &str) -> anyhow::Result<Option<SessionQueryResult>>,
{
    if !is_self_send(
        sender_identity,
        sender_team,
        recipient_agent,
        recipient_team,
    ) {
        return false;
    }

    let Some(sender_session_id) = sender_session_id else {
        // Unknown sender session: retain warning to avoid silent misrouting.
        return true;
    };

    match query(recipient_team, recipient_agent) {
        Ok(Some(session)) if session.alive => session.session_id == sender_session_id,
        Ok(Some(_)) => true,
        Ok(None) => true,
        Err(_) => true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendRule {
    Claude,
    Codex,
    Gemini,
}

fn backend_expected_rule(member: &AgentMember) -> Option<BackendRule> {
    match member.effective_backend_type() {
        Some(BackendType::ClaudeCode) => Some(BackendRule::Claude),
        Some(BackendType::Codex) => Some(BackendRule::Codex),
        Some(BackendType::Gemini) => Some(BackendRule::Gemini),
        Some(BackendType::External) | Some(BackendType::Human(_)) => None,
        None => {
            let agent_type = member.agent_type.to_ascii_lowercase();
            match agent_type.as_str() {
                "codex" => Some(BackendRule::Codex),
                "gemini" => Some(BackendRule::Gemini),
                "general-purpose" | "explore" | "plan" => Some(BackendRule::Claude),
                _ if member.name == "team-lead" => Some(BackendRule::Claude),
                _ => None,
            }
        }
    }
}

fn process_observation(sys: &sysinfo::System, pid: sysinfo::Pid) -> Option<(String, String)> {
    let process = sys.process(pid)?;
    let comm = process.name().to_string_lossy().to_lowercase();
    let args = process
        .cmd()
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    Some((comm, args))
}

fn process_matches_rule(rule: BackendRule, comm: &str, args: &str) -> bool {
    let comm_basename = comm.rsplit('/').next().unwrap_or(comm);
    match rule {
        // sysinfo may return the full executable path; normalize to basename.
        BackendRule::Claude => comm_basename == "claude",
        BackendRule::Codex => comm_basename == "codex",
        BackendRule::Gemini => comm_basename == "node" && args.contains("gemini"),
    }
}

fn detect_sender_process_pid(member: &AgentMember) -> Option<u32> {
    use sysinfo::{Pid, System};

    let expected_rule = backend_expected_rule(member)?;

    let sys = System::new_all();
    if let Ok(Some(hook)) = read_hook_file()
        && hook.pid > 1
        && let Some((comm, args)) = process_observation(&sys, Pid::from_u32(hook.pid))
        && process_matches_rule(expected_rule, &comm, &args)
    {
        return Some(hook.pid);
    }

    let mut cursor = Pid::from_u32(std::process::id());

    for _ in 0..16 {
        let Some(proc_info) = sys.process(cursor) else {
            break;
        };
        let Some(parent) = proc_info.parent() else {
            break;
        };
        let parent_u32 = parent.as_u32();
        if let Some((comm, args)) = process_observation(&sys, parent)
            && process_matches_rule(expected_rule, &comm, &args)
        {
            return Some(parent_u32);
        }
        cursor = parent;
    }

    // Never register the short-lived `atm` command process as the agent PID.
    // If no matching runtime process is visible, skip hint registration.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use serial_test::serial;
    use std::collections::HashMap;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::TempDir;

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

    #[test]
    fn test_build_inbox_message_preserves_cross_team_source() {
        let msg = build_inbox_message(
            "team-lead".to_string(),
            Some("src-gen".to_string()),
            "cross-team note".to_string(),
            Some("cross-team note".to_string()),
        );

        assert_eq!(msg.from, "team-lead");
        assert_eq!(msg.source_team.as_deref(), Some("src-gen"));
        assert_eq!(msg.text, "cross-team note");
        assert!(!msg.read);
    }

    #[test]
    fn test_build_inbox_message_preserves_same_team_source() {
        let msg = build_inbox_message(
            "team-lead".to_string(),
            Some("atm-dev".to_string()),
            "same-team note".to_string(),
            Some("same-team note".to_string()),
        );

        assert_eq!(msg.from, "team-lead");
        assert_eq!(msg.source_team.as_deref(), Some("atm-dev"));
        assert_eq!(msg.text, "same-team note");
        assert!(!msg.read);
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

    fn make_member(name: &str, agent_type: &str, backend: Option<BackendType>) -> AgentMember {
        AgentMember {
            agent_id: format!("{name}@atm-dev"),
            name: name.to_string(),
            agent_type: agent_type.to_string(),
            model: "unknown".to_string(),
            prompt: None,
            color: None,
            plan_mode_required: None,
            joined_at: 0,
            tmux_pane_id: None,
            cwd: ".".to_string(),
            subscriptions: Vec::new(),
            backend_type: None,
            is_active: None,
            last_active: None,
            session_id: None,
            external_backend_type: backend,
            external_model: None,
            unknown_fields: HashMap::new(),
        }
    }

    #[test]
    fn test_backend_expected_rule_prefers_backend_type() {
        let codex_member = make_member("arch-ctm", "codex", Some(BackendType::Codex));
        assert_eq!(
            backend_expected_rule(&codex_member),
            Some(BackendRule::Codex)
        );
    }

    #[test]
    fn test_backend_expected_rule_defaults_team_lead_to_claude() {
        let lead = make_member("team-lead", "general-purpose", None);
        assert_eq!(backend_expected_rule(&lead), Some(BackendRule::Claude));
    }

    #[test]
    fn test_backend_expected_rule_uses_legacy_agent_type_for_codex() {
        let codex_member = make_member("arch-ctm", "codex", None);
        assert_eq!(
            backend_expected_rule(&codex_member),
            Some(BackendRule::Codex)
        );
    }

    #[test]
    fn test_process_matches_rule_matches_basename_for_path_commands() {
        assert!(process_matches_rule(BackendRule::Claude, "claude", ""));
        assert!(!process_matches_rule(BackendRule::Claude, "node", "claude"));
        assert!(process_matches_rule(
            BackendRule::Claude,
            "/usr/local/bin/claude",
            ""
        ));
        assert!(process_matches_rule(BackendRule::Codex, "codex", ""));
        assert!(process_matches_rule(
            BackendRule::Codex,
            "/Users/randlee/.npm-global/bin/codex",
            ""
        ));
        assert!(process_matches_rule(
            BackendRule::Gemini,
            "node",
            "gemini --model 2.5-pro"
        ));
        assert!(process_matches_rule(
            BackendRule::Gemini,
            "/opt/homebrew/bin/node",
            "gemini --model 2.5-pro"
        ));
        assert!(!process_matches_rule(
            BackendRule::Gemini,
            "gemini",
            "gemini --model 2.5-pro"
        ));
    }

    #[test]
    #[serial]
    fn test_detect_sender_process_pid_does_not_fall_back_to_atm_subprocess_pid() {
        let old_identity = std::env::var("ATM_IDENTITY").ok();
        unsafe { std::env::set_var("ATM_IDENTITY", "arch-ctm") };

        let member = make_member(
            "team-lead",
            "general-purpose",
            Some(BackendType::ClaudeCode),
        );
        let detected = detect_sender_process_pid(&member);
        assert!(
            detected != Some(std::process::id()),
            "sender PID detection must not register the transient atm subprocess"
        );

        unsafe {
            match old_identity {
                Some(v) => std::env::set_var("ATM_IDENTITY", v),
                None => std::env::remove_var("ATM_IDENTITY"),
            }
        }
    }

    #[test]
    fn test_resolve_offline_action_default() {
        let args = make_send_args(None);
        let config = Config::default();
        assert_eq!(resolve_offline_action(&args, &config), "");
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
    fn test_self_send_warning_not_added_for_different_recipient() {
        let sender = "team-lead";
        let sender_team = "atm-dev";
        let agent_name = "arch-ctm"; // different → no warning
        let target_team = "atm-dev";
        let raw_message = "Hello other!";

        let message_text = if is_self_send(sender, sender_team, agent_name, target_team) {
            format!("[WARNING: Sent to self — identity={sender}]\n{raw_message}")
        } else {
            raw_message.to_string()
        };

        assert_eq!(message_text, "Hello other!");
        assert!(!message_text.contains("WARNING"));
    }

    #[test]
    fn test_self_send_warning_not_added_for_cross_team_same_name() {
        let sender = "team-lead";
        let sender_team = "p3-setup";
        let agent_name = "team-lead";
        let target_team = "atm-dev";
        assert!(!is_self_send(sender, sender_team, agent_name, target_team));
    }

    // Issue #482: cross-team self-send false positive — same agent name on different
    // teams must NOT trigger the self-send warning.

    #[test]
    fn test_is_self_send_same_name_different_teams_no_warning() {
        // team-lead@schook sending to team-lead@scmux — different teams, must NOT warn.
        assert!(!is_self_send("team-lead", "schook", "team-lead", "scmux"));
    }

    #[test]
    fn test_is_self_send_same_name_same_team_warns() {
        // team-lead@atm-dev sending to team-lead@atm-dev — same identity, MUST warn.
        assert!(is_self_send("team-lead", "atm-dev", "team-lead", "atm-dev"));
    }

    #[test]
    fn test_is_self_send_different_name_same_team_no_warning() {
        // team-lead@atm-dev sending to arch-ctm@atm-dev — different agent, must NOT warn.
        assert!(!is_self_send("team-lead", "atm-dev", "arch-ctm", "atm-dev"));
    }

    #[test]
    fn test_recipient_has_dead_session_true_for_dead_session() {
        let is_offline = recipient_has_dead_session_with_query("atm-dev", "team-lead", |_, _| {
            Ok(Some(SessionQueryResult {
                session_id: "s".to_string(),
                process_id: 123,
                alive: false,
                last_seen_at: None,
                runtime: None,
                runtime_session_id: None,
                pane_id: None,
                runtime_home: None,
            }))
        });
        assert!(is_offline);
    }

    #[test]
    fn test_recipient_has_dead_session_false_for_alive_session() {
        let is_offline = recipient_has_dead_session_with_query("atm-dev", "team-lead", |_, _| {
            Ok(Some(SessionQueryResult {
                session_id: "s".to_string(),
                process_id: 123,
                alive: true,
                last_seen_at: None,
                runtime: None,
                runtime_session_id: None,
                pane_id: None,
                runtime_home: None,
            }))
        });
        assert!(!is_offline);
    }

    #[test]
    fn test_recipient_has_dead_session_false_when_session_unknown() {
        let is_offline =
            recipient_has_dead_session_with_query("atm-dev", "team-lead", |_, _| Ok(None));
        assert!(!is_offline);
    }

    #[test]
    fn test_recipient_has_dead_session_false_on_query_error() {
        let is_offline = recipient_has_dead_session_with_query("atm-dev", "team-lead", |_, _| {
            Err(anyhow!("daemon unavailable"))
        });
        assert!(!is_offline);
    }

    #[test]
    fn test_destination_target_formats_agent_at_team() {
        assert_eq!(
            destination_target("team-lead", "atm-dev"),
            "team-lead@atm-dev"
        );
    }

    #[test]
    fn test_touch_sender_session_heartbeat_invokes_query() {
        let called = std::cell::Cell::new(false);
        let result =
            touch_sender_session_heartbeat_with_query("atm-dev", "arch-ctm", |team, name| {
                called.set(true);
                assert_eq!(team, "atm-dev");
                assert_eq!(name, "arch-ctm");
                Ok(None)
            });
        assert!(result.is_ok());
        assert!(called.get(), "heartbeat query should be invoked");
    }

    #[test]
    fn test_touch_sender_session_heartbeat_propagates_query_error() {
        let result = touch_sender_session_heartbeat_with_query("atm-dev", "arch-ctm", |_, _| {
            Err(anyhow!("daemon unavailable"))
        });
        assert!(result.is_err());
    }

    fn current_ppid_hook_path() -> std::path::PathBuf {
        let ppid = crate::util::hook_identity::get_parent_pid();
        std::env::temp_dir().join(format!("atm-hook-{ppid}.json"))
    }

    fn write_fresh_hook_file(session_id: &str) {
        let ppid = crate::util::hook_identity::get_parent_pid();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let data = serde_json::json!({
            "pid": ppid,
            "session_id": session_id,
            "agent_name": "team-lead",
            "created_at": now,
        });
        std::fs::write(
            current_ppid_hook_path(),
            serde_json::to_string(&data).unwrap(),
        )
        .unwrap();
    }

    fn write_session_file(home: &std::path::Path, team: &str, identity: &str, session_id: &str) {
        let sessions_dir = home
            .join(".claude")
            .join("teams")
            .join(team)
            .join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let data = serde_json::json!({
            "session_id": session_id,
            "team": team,
            "identity": identity,
            "pid": std::process::id(),
            "created_at": now,
            "updated_at": now,
        });
        std::fs::write(
            sessions_dir.join(format!("{session_id}.json")),
            serde_json::to_string(&data).unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[serial]
    fn test_resolve_sender_session_id_prefers_hook_file_over_session_and_env() {
        let temp = TempDir::new().unwrap();
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);

        write_fresh_hook_file("sid-from-hook");
        write_session_file(temp.path(), "atm-dev", "team-lead", "sid-from-session");

        unsafe {
            std::env::set_var("ATM_HOME", temp.path());
            std::env::set_var("ATM_TEST_HOME", temp.path());
            std::env::set_var("ATM_RUNTIME", "claude");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::set_var("CLAUDE_SESSION_ID", "sid-from-env");
            std::env::set_var("ATM_TEST_ENABLE_HOOK_RESOLUTION", "1");
        }

        let actual =
            resolve_sender_session_id_with_context(Some("atm-dev"), Some("team-lead")).unwrap();

        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("ATM_TEST_HOME");
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("ATM_TEST_ENABLE_HOOK_RESOLUTION");
        }
        let _ = std::fs::remove_file(&hook_path);

        assert_eq!(actual.as_deref(), Some("sid-from-hook"));
    }

    #[test]
    #[serial]
    fn test_resolve_sender_session_id_prefers_env_when_hook_missing() {
        let temp = TempDir::new().unwrap();
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);

        write_session_file(temp.path(), "atm-dev", "team-lead", "sid-from-session");

        unsafe {
            std::env::set_var("ATM_HOME", temp.path());
            std::env::set_var("ATM_TEST_HOME", temp.path());
            std::env::set_var("ATM_RUNTIME", "claude");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::set_var("CLAUDE_SESSION_ID", "sid-from-env");
        }

        let actual =
            resolve_sender_session_id_with_context(Some("atm-dev"), Some("team-lead")).unwrap();

        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("ATM_TEST_HOME");
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        assert_eq!(actual.as_deref(), Some("sid-from-env"));
    }

    #[test]
    #[serial]
    fn test_resolve_sender_session_id_uses_session_file_when_hook_and_env_missing() {
        let temp = TempDir::new().unwrap();
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);

        write_session_file(temp.path(), "atm-dev", "team-lead", "sid-from-session");

        unsafe {
            std::env::set_var("ATM_HOME", temp.path());
            std::env::set_var("ATM_TEST_HOME", temp.path());
            std::env::set_var("ATM_RUNTIME", "claude");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        let actual =
            resolve_sender_session_id_with_context(Some("atm-dev"), Some("team-lead")).unwrap();

        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("ATM_TEST_HOME");
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        assert_eq!(actual.as_deref(), Some("sid-from-session"));
    }

    #[test]
    #[serial]
    fn test_resolve_sender_session_id_uses_env_when_hook_and_session_missing() {
        let temp = TempDir::new().unwrap();
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);

        unsafe {
            std::env::set_var("ATM_HOME", temp.path());
            std::env::set_var("ATM_TEST_HOME", temp.path());
            std::env::set_var("ATM_RUNTIME", "claude");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::set_var("CLAUDE_SESSION_ID", "sid-from-env");
        }

        let actual =
            resolve_sender_session_id_with_context(Some("atm-dev"), Some("team-lead")).unwrap();

        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("ATM_TEST_HOME");
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        assert_eq!(actual.as_deref(), Some("sid-from-env"));
    }

    #[test]
    #[serial]
    fn test_resolve_sender_session_id_errors_on_ambiguous_session_files_without_env() {
        let temp = TempDir::new().unwrap();
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);

        write_session_file(temp.path(), "atm-dev", "team-lead", "sid-a");
        write_session_file(temp.path(), "atm-dev", "team-lead", "sid-b");

        unsafe {
            std::env::set_var("ATM_HOME", temp.path());
            std::env::set_var("ATM_TEST_HOME", temp.path());
            std::env::set_var("ATM_RUNTIME", "claude");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        let err = resolve_sender_session_id_with_context(Some("atm-dev"), Some("team-lead"))
            .expect_err("ambiguous session files should error");

        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("ATM_TEST_HOME");
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        assert!(err.to_string().contains("CALLER_AMBIGUOUS"));
        assert!(err.to_string().contains("ATM_SESSION_ID"));
    }

    #[test]
    fn test_should_warn_self_send_true_when_same_session_owned() {
        let warn = should_warn_self_send_with_query(
            "team-lead",
            "atm-dev",
            "team-lead",
            "atm-dev",
            Some("sid-123"),
            |_, _| {
                Ok(Some(SessionQueryResult {
                    session_id: "sid-123".to_string(),
                    process_id: 100,
                    alive: true,
                    last_seen_at: None,
                    runtime: None,
                    runtime_session_id: None,
                    pane_id: None,
                    runtime_home: None,
                }))
            },
        );
        assert!(warn);
    }

    #[test]
    fn test_should_warn_self_send_false_when_different_active_session_owns_identity() {
        let warn = should_warn_self_send_with_query(
            "team-lead",
            "atm-dev",
            "team-lead",
            "atm-dev",
            Some("sid-local"),
            |_, _| {
                Ok(Some(SessionQueryResult {
                    session_id: "sid-remote".to_string(),
                    process_id: 101,
                    alive: true,
                    last_seen_at: None,
                    runtime: None,
                    runtime_session_id: None,
                    pane_id: None,
                    runtime_home: None,
                }))
            },
        );
        assert!(!warn);
    }

    #[test]
    fn test_should_warn_self_send_true_when_sender_session_unknown() {
        let warn = should_warn_self_send_with_query(
            "team-lead",
            "atm-dev",
            "team-lead",
            "atm-dev",
            None,
            |_, _| Ok(None),
        );
        assert!(warn);
    }
}
