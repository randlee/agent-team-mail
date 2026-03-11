//! `atm register` command — register this agent session with a team.
//!
//! ## Synopsis
//!
//! ```text
//! atm register <team>           # team-lead self-registration
//! atm register <team> <name>    # teammate registration
//! ```
//!
//! The command reads the PID-based hook file written by `atm-identity-write.py`
//! to obtain the session ID automatically through the shared caller resolver,
//! with `ATM_SESSION_ID` as the explicit override.

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::io::atomic::atomic_swap;
use agent_team_mail_core::io::inbox::inbox_append;
use agent_team_mail_core::io::lock::acquire_lock;
use agent_team_mail_core::schema::{InboxMessage, TeamConfig};
use anyhow::Result;
use chrono::Utc;
use clap::Args;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use tracing::warn;
use uuid::Uuid;

use crate::util::caller_identity::resolve_caller_session_id_required;
use crate::util::settings::get_home_dir;

/// Register this agent session with a team.
///
/// For team-lead: updates `leadSessionId` in the team config.
/// For teammates: updates the member's `sessionId` field.
#[derive(Args, Debug)]
pub struct RegisterArgs {
    /// Team name
    team: String,

    /// Agent name for teammate registration (omit for team-lead self-registration)
    name: Option<String>,

    /// Force re-registration even if the session appears live
    #[arg(long)]
    force: bool,
}

/// Execute the register command.
pub fn execute(args: RegisterArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let team_dir = home_dir.join(".claude/teams").join(&args.team);
    let config_path = team_dir.join("config.json");

    if !config_path.exists() {
        anyhow::bail!(
            "Team '{}' not found (directory {} doesn't exist). \
             Call TeamCreate(team_name=\"{}\") to create it.",
            args.team,
            team_dir.display(),
            args.team
        );
    }

    // Resolve session ID through shared caller identity resolver.
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let config = resolve_config(&ConfigOverrides::default(), &current_dir, &home_dir)?;
    let caller_identity = &config.core.identity;
    let session_id = resolve_caller_session_id_required(Some(&args.team), Some(caller_identity))?;

    if let Some(ref name) = args.name {
        register_teammate(&config_path, &args.team, name, &session_id)
    } else {
        register_team_lead(&home_dir, &config_path, &args.team, &session_id, args.force)
    }
}

/// Register as team-lead: update `leadSessionId` in team config.
fn register_team_lead(
    home_dir: &std::path::Path,
    config_path: &std::path::Path,
    team: &str,
    session_id: &str,
    force: bool,
) -> Result<()> {
    // Verify caller identity is "team-lead".
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let config = resolve_config(&ConfigOverrides::default(), &current_dir, home_dir)?;
    let caller_identity = &config.core.identity;
    if caller_identity != "team-lead" {
        anyhow::bail!(
            "caller identity is '{}'. Only team-lead may call `atm register <team>` \
             without specifying an agent name.\n\
             Hint: set `identity = \"team-lead\"` in `.atm.toml` or set ATM_IDENTITY=team-lead.",
            caller_identity
        );
    }

    // Check liveness of existing session if not forced.
    if !force {
        let existing: TeamConfig = serde_json::from_str(&std::fs::read_to_string(config_path)?)?;
        if !existing.lead_session_id.is_empty() && existing.lead_session_id == session_id {
            println!(
                "Already registered as team-lead for team '{}'. Session: {}",
                team, session_id
            );
            return Ok(());
        }

        if !existing.lead_session_id.is_empty() {
            // Query daemon for liveness — proceed silently if daemon unreachable.
            use agent_team_mail_core::daemon_client::query_session;
            match query_session("team-lead") {
                Ok(Some(ref info)) if info.alive && info.session_id != session_id => {
                    let short = &info.session_id[..8.min(info.session_id.len())];
                    anyhow::bail!(
                        "team-lead is already active in session {}... (use --force to override)",
                        short
                    );
                }
                Ok(Some(_)) => {}
                Ok(None) => {
                    anyhow::bail!(
                        "WARNING: cannot confirm liveness of existing team-lead session {}. \
                         Use --force to take over explicitly.",
                        existing.lead_session_id
                    );
                }
                Err(e) => {
                    anyhow::bail!(
                        "WARNING: daemon liveness check failed ({e}). \
                         Cannot safely replace existing team-lead session {}; use --force to override.",
                        existing.lead_session_id
                    );
                }
            }
        }
    }

    // Atomically update config.
    {
        let lock_path = config_path.with_extension("lock");
        let _lock = acquire_lock(&lock_path, 5)
            .map_err(|e| anyhow::anyhow!("Failed to acquire lock for team config: {e}"))?;

        let mut team_config: TeamConfig =
            serde_json::from_str(&std::fs::read_to_string(config_path)?)?;
        team_config.lead_session_id = session_id.to_string();

        write_team_config(config_path, &team_config)?;
    }

    // Reload and notify members.
    let team_config: TeamConfig = serde_json::from_str(&std::fs::read_to_string(config_path)?)?;

    let team_dir = config_path
        .parent()
        .expect("config_path must have a parent");
    let inboxes_dir = team_dir.join("inboxes");
    if !inboxes_dir.exists() {
        std::fs::create_dir_all(&inboxes_dir)?;
    }

    let mut notified = 0usize;
    let notify_text = "Team-lead has registered for this session. Context may have been reset. Please provide a brief status update.";

    for member in &team_config.members {
        if member.name == "team-lead" {
            continue;
        }
        let inbox_path = inboxes_dir.join(format!("{}.json", member.name));
        let msg = InboxMessage {
            from: "team-lead".to_string(),
            text: notify_text.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            read: false,
            summary: Some("Team lead session registered".to_string()),
            message_id: Some(Uuid::new_v4().to_string()),
            unknown_fields: HashMap::new(),
        };
        match inbox_append(&inbox_path, &msg, team, &member.name) {
            Ok(_) => notified += 1,
            Err(e) => warn!("Failed to notify {}: {e}", member.name),
        }
    }

    println!(
        "Registered as team-lead for team '{}'. Session: {}. {} member(s) notified.",
        team, session_id, notified
    );
    Ok(())
}

/// Register as a named teammate: update the member's `sessionId` field.
fn register_teammate(
    config_path: &std::path::Path,
    team: &str,
    name: &str,
    session_id: &str,
) -> Result<()> {
    // Verify member exists in team config.
    let existing: TeamConfig = serde_json::from_str(&std::fs::read_to_string(config_path)?)?;

    if !existing.members.iter().any(|m| m.name == name) {
        anyhow::bail!(
            "Agent '{}' not found in team '{}'. \
             Ask the user who you are on this team.",
            name,
            team
        );
    }

    // Warn if team-lead hasn't registered yet.
    if existing.lead_session_id.is_empty() {
        println!(
            "WARNING: team-lead is not registered for team '{}'. \
             ATM messaging will work, but Claude Code team messaging (SendMessage) \
             will not function until team-lead registers. \
             If this is unexpected, ask the user: \"Who am I on this team?\"",
            team
        );
    }

    // Atomically update the member's session ID.
    {
        let lock_path = config_path.with_extension("lock");
        let _lock = acquire_lock(&lock_path, 5)
            .map_err(|e| anyhow::anyhow!("Failed to acquire lock for team config: {e}"))?;

        let mut team_config: TeamConfig =
            serde_json::from_str(&std::fs::read_to_string(config_path)?)?;

        let member = team_config
            .members
            .iter_mut()
            .find(|m| m.name == name)
            .expect("member existence already verified above");
        member.session_id = Some(session_id.to_string());
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        member.last_active = Some(now_ms);

        write_team_config(config_path, &team_config)?;
    }

    println!(
        "Registered as '{}' in team '{}'. Session: {}",
        name, team, session_id
    );
    Ok(())
}

/// Write team config atomically using a temp file + fsync + swap.
fn write_team_config(config_path: &std::path::Path, config: &TeamConfig) -> Result<()> {
    let serialized = serde_json::to_string_pretty(config)?;
    let tmp_path = config_path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp_path)?;
    file.write_all(serialized.as_bytes())?;
    file.sync_all()?;
    drop(file);
    atomic_swap(config_path, &tmp_path)?;
    Ok(())
}
