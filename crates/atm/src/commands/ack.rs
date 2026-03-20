use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::io::atomic::atomic_swap;
use agent_team_mail_core::io::error::InboxError;
use agent_team_mail_core::io::lock::acquire_lock;
use agent_team_mail_core::schema::{InboxMessage, TeamConfig};
use agent_team_mail_core::text::{DEFAULT_MAX_MESSAGE_BYTES, validate_message_text};
use anyhow::{Context, Result};
use chrono::Utc;
use clap::Args;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use uuid::Uuid;

use crate::commands::send::generate_summary;
use crate::util::addressing::parse_address;
use crate::util::hook_identity::read_hook_file_identity;
use crate::util::settings::{get_home_dir, teams_root_dir_for};

/// Acknowledge a single ATM task message and send a visible reply atomically.
#[derive(Args, Debug)]
pub struct AckArgs {
    /// Message ID to acknowledge in your own inbox
    message_id: String,

    /// Visible reply to send back to the original sender
    reply: String,

    /// Override default team
    #[arg(long)]
    team: Option<String>,

    /// Override reader identity (default: hook file → ATM_IDENTITY → .atm.toml → reject)
    #[arg(long = "as", value_name = "NAME")]
    reader_as: Option<String>,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

pub fn execute(args: AckArgs) -> Result<()> {
    if args.reply.trim().is_empty() {
        anyhow::bail!("Reply text cannot be empty");
    }
    validate_message_text(&args.reply, DEFAULT_MAX_MESSAGE_BYTES)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

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

    let team_name = config.core.default_team.clone();
    let agent_name = config.core.identity.clone();
    let teams_root = teams_root_dir_for(&home_dir);
    let team_dir = teams_root.join(&team_name);
    let inbox_path = team_dir.join("inboxes").join(format!("{agent_name}.json"));

    if !inbox_path.exists() {
        anyhow::bail!("Inbox not found for {agent_name}@{team_name}");
    }

    let (source_exists, source_original_bytes, mut source_messages) =
        load_inbox_messages(&inbox_path)
            .with_context(|| format!("load {agent_name}@{team_name}"))?;

    let source_message = source_messages
        .iter()
        .find(|message| message.message_id.as_deref() == Some(args.message_id.as_str()))
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Message ID {} not found in {agent_name}@{team_name}",
                args.message_id
            )
        })?;

    if source_message.is_acknowledged() {
        anyhow::bail!("Message {} is already acknowledged", args.message_id);
    }

    let (target_agent, target_team) =
        resolve_reply_target(&source_message, &team_name).context("resolve reply target")?;
    let target_team_dir = teams_root.join(&target_team);
    if !target_team_dir.exists() {
        anyhow::bail!("Reply target team '{target_team}' not found");
    }
    let target_config_path = target_team_dir.join("config.json");
    if !target_config_path.exists() {
        anyhow::bail!("Reply target team config not found at {target_config_path:?}");
    }

    let target_config: TeamConfig =
        serde_json::from_str(&fs::read_to_string(&target_config_path)?)?;
    if !target_config
        .members
        .iter()
        .any(|member| member.name == target_agent)
    {
        anyhow::bail!("Reply target '{target_agent}' not found in team '{target_team}'");
    }

    let target_inbox_path = target_team_dir
        .join("inboxes")
        .join(format!("{target_agent}.json"));
    if let Some(parent) = target_inbox_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let (target_exists, target_original_bytes, mut target_messages) =
        load_inbox_messages(&target_inbox_path)
            .with_context(|| format!("load {target_agent}@{target_team}"))?;

    let source_lock_path = inbox_path.with_extension("lock");
    let target_lock_path = target_inbox_path.with_extension("lock");
    if source_lock_path <= target_lock_path {
        let _source_lock = acquire_lock(&source_lock_path, 5)?;
        let _target_lock = if target_lock_path != source_lock_path {
            Some(acquire_lock(&target_lock_path, 5)?)
        } else {
            None
        };
        apply_ack_transaction(
            &inbox_path,
            source_exists,
            &source_original_bytes,
            &mut source_messages,
            &args.message_id,
            &target_inbox_path,
            target_exists,
            &target_original_bytes,
            &mut target_messages,
            build_reply_message(
                agent_name.clone(),
                team_name.clone(),
                args.reply.clone(),
                args.message_id.clone(),
            ),
        )?;
    } else {
        let _target_lock = acquire_lock(&target_lock_path, 5)?;
        let _source_lock = acquire_lock(&source_lock_path, 5)?;
        apply_ack_transaction(
            &inbox_path,
            source_exists,
            &source_original_bytes,
            &mut source_messages,
            &args.message_id,
            &target_inbox_path,
            target_exists,
            &target_original_bytes,
            &mut target_messages,
            build_reply_message(
                agent_name.clone(),
                team_name.clone(),
                args.reply.clone(),
                args.message_id.clone(),
            ),
        )?;
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
        message_id: Some(args.message_id.clone()),
        target: Some(format!("{target_agent}@{target_team}")),
        message_text: Some(args.reply.clone()),
        ..Default::default()
    });

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "action": "ack",
                "team": team_name,
                "agent": agent_name,
                "message_id": args.message_id,
                "reply_sent": true,
                "reply_target": format!("{target_agent}@{target_team}"),
            }))?
        );
    } else {
        println!(
            "Acknowledged {} for {}@{} and sent reply to {}@{}",
            args.message_id, agent_name, team_name, target_agent, target_team
        );
    }

    Ok(())
}

fn resolve_reply_target(message: &InboxMessage, current_team: &str) -> Result<(String, String)> {
    let fallback_team = message
        .source_team
        .clone()
        .unwrap_or_else(|| current_team.to_string());
    let (agent, team) = parse_address(&message.from, &Some(fallback_team), current_team)?;
    Ok((agent, team))
}

fn build_reply_message(
    from: String,
    source_team: String,
    text: String,
    acked_message_id: String,
) -> InboxMessage {
    let mut unknown_fields = HashMap::new();
    unknown_fields.insert(
        "acknowledgesMessageId".to_string(),
        serde_json::Value::String(acked_message_id),
    );

    InboxMessage {
        from,
        source_team: Some(source_team),
        text: text.clone(),
        timestamp: Utc::now().to_rfc3339(),
        read: false,
        summary: Some(generate_summary(&text)),
        message_id: Some(Uuid::new_v4().to_string()),
        unknown_fields,
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_ack_transaction(
    source_inbox_path: &Path,
    source_exists: bool,
    source_original_bytes: &[u8],
    source_messages: &mut [InboxMessage],
    source_message_id: &str,
    target_inbox_path: &Path,
    target_exists: bool,
    target_original_bytes: &[u8],
    target_messages: &mut Vec<InboxMessage>,
    reply_message: InboxMessage,
) -> Result<()> {
    let acknowledged_at = Utc::now().to_rfc3339();

    let source_entry = source_messages
        .iter_mut()
        .find(|message| message.message_id.as_deref() == Some(source_message_id))
        .ok_or_else(|| anyhow::anyhow!("Message {source_message_id} disappeared during ack"))?;
    source_entry.read = true;
    source_entry.mark_acknowledged(acknowledged_at);

    if source_inbox_path == target_inbox_path {
        let source_vec = source_messages
            .iter()
            .cloned()
            .chain(std::iter::once(reply_message))
            .collect::<Vec<_>>();
        persist_inbox_atomic(
            source_inbox_path,
            source_exists,
            source_original_bytes,
            &source_vec,
        )?;
        return Ok(());
    }

    target_messages.push(reply_message);
    let source_vec = source_messages.to_vec();

    persist_inbox_atomic(
        target_inbox_path,
        target_exists,
        target_original_bytes,
        target_messages,
    )?;

    if let Err(error) = persist_inbox_atomic(
        source_inbox_path,
        source_exists,
        source_original_bytes,
        &source_vec,
    ) {
        let _ = restore_inbox(target_inbox_path, target_exists, target_original_bytes);
        return Err(error);
    }

    Ok(())
}

fn load_inbox_messages(path: &Path) -> Result<(bool, Vec<u8>, Vec<InboxMessage>)> {
    if !path.exists() {
        return Ok((false, b"[]".to_vec(), Vec::new()));
    }

    let bytes = fs::read(path).map_err(|source| InboxError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let messages =
        serde_json::from_slice::<Vec<InboxMessage>>(&bytes).map_err(|source| InboxError::Json {
            path: path.to_path_buf(),
            source,
        })?;
    Ok((true, bytes, messages))
}

fn persist_inbox_atomic(
    path: &Path,
    path_exists: bool,
    _original_bytes: &[u8],
    messages: &[InboxMessage],
) -> Result<()> {
    let tmp_path = path.with_extension("acktmp");
    let content = serde_json::to_vec_pretty(messages)?;
    write_synced_file(&tmp_path, &content)?;

    if path_exists {
        atomic_swap(path, &tmp_path)?;
        let _ = fs::remove_file(&tmp_path);
    } else {
        fs::rename(&tmp_path, path).map_err(|source| InboxError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }

    Ok(())
}

fn restore_inbox(path: &Path, path_exists: bool, original_bytes: &[u8]) -> Result<()> {
    if path_exists {
        let rollback_path = path.with_extension("rollback");
        write_synced_file(&rollback_path, original_bytes)?;
        atomic_swap(path, &rollback_path)?;
        let _ = fs::remove_file(&rollback_path);
    } else if path.exists() {
        fs::remove_file(path).map_err(|source| InboxError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

fn write_synced_file(path: &Path, content: &[u8]) -> Result<()> {
    let mut file = fs::File::create(path).map_err(|source| InboxError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(content).map_err(|source| InboxError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| InboxError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}
