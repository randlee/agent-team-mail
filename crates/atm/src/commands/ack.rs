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
use std::collections::{BTreeMap, HashMap};
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
    let source_states = load_source_inbox_states(&team_dir, &agent_name, &args.message_id)
        .with_context(|| format!("load merged inbox surface for {agent_name}@{team_name}"))?;
    let source_message = source_states
        .values()
        .flat_map(|state| state.messages.iter())
        .find(|message| message.message_id.as_deref() == Some(args.message_id.as_str()))
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Message ID {} not found in merged inbox surface for {agent_name}@{team_name}",
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
    let mut inbox_states = source_states;
    if !inbox_states.contains_key(&target_inbox_path) {
        let (exists, original_bytes, messages) = load_inbox_messages(&target_inbox_path)
            .with_context(|| format!("load {target_agent}@{target_team}"))?;
        inbox_states.insert(
            target_inbox_path.clone(),
            InboxFileState {
                exists,
                original_bytes,
                messages,
            },
        );
    }

    let reply_message = build_reply_message(
        agent_name.clone(),
        team_name.clone(),
        args.reply.clone(),
        args.message_id.clone(),
    );
    apply_ack_transaction(
        &mut inbox_states,
        &args.message_id,
        &target_inbox_path,
        reply_message,
    )?;

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
    let (agent, team) = if message.from.contains('@') {
        parse_address(&message.from, &None, current_team)?
    } else {
        let fallback_team = message
            .source_team
            .clone()
            .unwrap_or_else(|| current_team.to_string());
        parse_address(&message.from, &Some(fallback_team), current_team)?
    };
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

fn apply_ack_transaction(
    inbox_states: &mut BTreeMap<std::path::PathBuf, InboxFileState>,
    source_message_id: &str,
    target_inbox_path: &Path,
    reply_message: InboxMessage,
) -> Result<()> {
    let acknowledged_at = Utc::now().to_rfc3339();
    let mut matched_any = false;
    for state in inbox_states.values_mut() {
        for message in state.messages.iter_mut() {
            if message.message_id.as_deref() == Some(source_message_id) {
                message.read = true;
                message.mark_acknowledged(acknowledged_at.clone());
                matched_any = true;
            }
        }
    }
    if !matched_any {
        anyhow::bail!("Message {source_message_id} disappeared during ack");
    }

    inbox_states
        .get_mut(target_inbox_path)
        .ok_or_else(|| anyhow::anyhow!("Reply target inbox disappeared during ack"))?
        .messages
        .push(reply_message);

    let lock_paths: Vec<_> = inbox_states
        .keys()
        .map(|path| path.with_extension("lock"))
        .collect();
    let _locks = acquire_locks_in_order(&lock_paths)?;

    let mut persisted_paths: Vec<std::path::PathBuf> = Vec::new();
    for (path, state) in inbox_states.iter() {
        if let Err(error) =
            persist_inbox_atomic(path, state.exists, &state.original_bytes, &state.messages)
        {
            for persisted in persisted_paths.iter().rev() {
                if let Some(previous) = inbox_states.get(persisted) {
                    let _ = restore_inbox(persisted, previous.exists, &previous.original_bytes);
                }
            }
            return Err(error);
        }
        persisted_paths.push(path.clone());
    }

    Ok(())
}

struct InboxFileState {
    exists: bool,
    original_bytes: Vec<u8>,
    messages: Vec<InboxMessage>,
}

fn load_source_inbox_states(
    team_dir: &Path,
    agent_name: &str,
    message_id: &str,
) -> Result<BTreeMap<std::path::PathBuf, InboxFileState>> {
    let mut states = BTreeMap::new();
    for path in candidate_inbox_paths(team_dir, agent_name)? {
        let (exists, original_bytes, messages) = load_inbox_messages(&path)?;
        if messages
            .iter()
            .any(|message| message.message_id.as_deref() == Some(message_id))
        {
            states.insert(
                path,
                InboxFileState {
                    exists,
                    original_bytes,
                    messages,
                },
            );
        }
    }
    Ok(states)
}

fn candidate_inbox_paths(team_dir: &Path, agent_name: &str) -> Result<Vec<std::path::PathBuf>> {
    let inboxes_dir = team_dir.join("inboxes");
    if !inboxes_dir.exists() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();
    for entry in fs::read_dir(&inboxes_dir).map_err(|source| InboxError::Io {
        path: inboxes_dir.clone(),
        source,
    })? {
        let entry = entry.map_err(|source| InboxError::Io {
            path: inboxes_dir.clone(),
            source,
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if file_name == format!("{agent_name}.json") {
            paths.push(path);
            continue;
        }
        if let Some(stem) = file_name.strip_suffix(".json")
            && stem.starts_with(&format!("{agent_name}."))
        {
            paths.push(path);
        }
    }

    paths.sort();
    Ok(paths)
}

fn acquire_locks_in_order(
    paths: &[std::path::PathBuf],
) -> Result<Vec<agent_team_mail_core::io::lock::FileLock>> {
    let mut unique_paths = paths.to_vec();
    unique_paths.sort();
    unique_paths.dedup();

    let mut guards = Vec::with_capacity(unique_paths.len());
    for lock_path in unique_paths {
        guards.push(acquire_lock(&lock_path, 5)?);
    }
    Ok(guards)
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
    let tmp_path = path.with_extension("json.tmp");
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
        let rollback_path = path.with_extension("json.rollback");
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn message(from: &str, source_team: Option<&str>) -> InboxMessage {
        InboxMessage {
            from: from.to_string(),
            source_team: source_team.map(str::to_string),
            text: "task".to_string(),
            timestamp: "2026-03-20T00:00:00Z".to_string(),
            read: false,
            summary: None,
            message_id: Some("msg-1".to_string()),
            unknown_fields: HashMap::new(),
        }
    }

    #[test]
    fn resolve_reply_target_prefers_qualified_from_team() {
        let (agent, team) =
            resolve_reply_target(&message("team-lead@src-dev", None), "atm-dev").unwrap();
        assert_eq!(agent, "team-lead");
        assert_eq!(team, "src-dev");
    }

    #[test]
    fn resolve_reply_target_falls_back_to_source_team_for_unqualified_from() {
        let (agent, team) =
            resolve_reply_target(&message("team-lead", Some("src-dev")), "atm-dev").unwrap();
        assert_eq!(agent, "team-lead");
        assert_eq!(team, "src-dev");
    }

    #[test]
    fn candidate_inbox_paths_include_origin_files() {
        let temp = TempDir::new().unwrap();
        let team_dir = temp.path().join("team");
        let inboxes = team_dir.join("inboxes");
        fs::create_dir_all(&inboxes).unwrap();
        fs::write(inboxes.join("agent.json"), "[]").unwrap();
        fs::write(inboxes.join("agent.remote-a.json"), "[]").unwrap();
        fs::write(inboxes.join("agent.remote-b.json"), "[]").unwrap();
        fs::write(inboxes.join("other.json"), "[]").unwrap();

        let paths = candidate_inbox_paths(&team_dir, "agent").unwrap();
        let names: Vec<_> = paths
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "agent.json".to_string(),
                "agent.remote-a.json".to_string(),
                "agent.remote-b.json".to_string()
            ]
        );
    }

    #[test]
    fn load_source_inbox_states_finds_origin_message_copy() {
        let temp = TempDir::new().unwrap();
        let team_dir = temp.path().join("team");
        let inboxes = team_dir.join("inboxes");
        fs::create_dir_all(&inboxes).unwrap();
        fs::write(inboxes.join("agent.json"), "[]").unwrap();
        fs::write(
            inboxes.join("agent.remote-a.json"),
            serde_json::to_string_pretty(&vec![serde_json::json!({
                "from": "team-lead",
                "text": "remote task",
                "timestamp": "2026-03-20T00:00:00Z",
                "read": false,
                "message_id": "msg-remote-1"
            })])
            .unwrap(),
        )
        .unwrap();

        let states = load_source_inbox_states(&team_dir, "agent", "msg-remote-1").unwrap();
        assert_eq!(states.len(), 1);
        let only_path = states.keys().next().unwrap();
        assert_eq!(
            only_path.file_name().unwrap().to_string_lossy(),
            "agent.remote-a.json"
        );
    }
}
