//! Teams command implementation

use anyhow::Result;
use agent_team_mail_core::config::{resolve_config, ConfigOverrides};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::io::atomic::atomic_swap;
use agent_team_mail_core::io::inbox::inbox_append;
use agent_team_mail_core::io::lock::acquire_lock;
use agent_team_mail_core::schema::{InboxMessage, TeamConfig};
use chrono::{DateTime, Utc};
use clap::Args;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::io::Write;
use tracing::warn;
use uuid::Uuid;

use crate::util::settings::get_home_dir;

/// Number of backups to retain per team. Older snapshots are pruned after
/// each backup or auto-backup-on-resume. Increase if longer rollback windows
/// are needed.
const BACKUP_RETENTION_COUNT: usize = 5;

/// List all teams on this machine
#[derive(Args, Debug)]
pub struct TeamsArgs {
    /// Subcommand (e.g., add-member)
    #[command(subcommand)]
    command: Option<TeamsCommand>,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

#[derive(clap::Subcommand, Debug)]
pub enum TeamsCommand {
    /// Add a member to a team without launching an agent
    AddMember(AddMemberArgs),
    /// Resume as team-lead for an existing team (updates leadSessionId)
    Resume(ResumeArgs),
    /// Remove stale (dead) members from a team
    Cleanup(CleanupArgs),
    /// Back up a team's config, inboxes, and tasks to a timestamped snapshot directory
    Backup(BackupArgs),
    /// Restore a team's members, inboxes, and tasks from a backup snapshot
    Restore(RestoreArgs),
}

/// Add a member to a team (no agent spawn)
#[derive(Args, Debug)]
pub struct AddMemberArgs {
    /// Team name
    team: String,

    /// Agent name (unique within team)
    agent: String,

    /// Agent type (e.g., "codex", "human", "plugin:ci_monitor")
    #[arg(long, default_value = "codex")]
    agent_type: String,

    /// Model identifier (defaults to "unknown")
    #[arg(long, default_value = "unknown")]
    model: String,

    /// Working directory (defaults to current directory)
    #[arg(long)]
    cwd: Option<PathBuf>,

    /// Mark agent as inactive
    #[arg(long)]
    inactive: bool,

    /// tmux pane ID for message injection (e.g. "%235")
    #[arg(long)]
    pane_id: Option<String>,
}

/// Resume as team-lead for an existing team
#[derive(Args, Debug)]
pub struct ResumeArgs {
    /// Team name
    team: String,

    /// Optional message to send to members on resume
    message: Option<String>,

    /// Override alive-session check and proceed even if old session appears alive
    #[arg(long)]
    force: bool,

    /// SIGTERM the old process before resuming (requires --force)
    #[arg(long, requires = "force")]
    kill: bool,

    /// Explicit session ID override (e.g., from the SessionStart hook output).
    /// If omitted, the session ID is resolved from CLAUDE_SESSION_ID env var or
    /// /tmp/atm-session-id (written by the gate hook on every tool call).
    #[arg(long)]
    session_id: Option<String>,
}

/// Remove stale (dead-session) members from a team
#[derive(Args, Debug)]
pub struct CleanupArgs {
    /// Team name
    team: String,

    /// Specific agent to clean up (if omitted, cleans all dead members)
    agent: Option<String>,

    /// Remove members even when the daemon is unreachable (unsafe — use only when daemon is known to be stopped)
    #[arg(long)]
    force: bool,
}

/// Back up a team's config, inboxes, and tasks
#[derive(Args, Debug)]
pub struct BackupArgs {
    /// Team name
    team: String,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

/// Restore a team from a backup snapshot (members, inboxes, and tasks)
#[derive(Args, Debug)]
pub struct RestoreArgs {
    /// Team name
    team: String,

    /// Restore from a specific backup path (default: latest backup)
    #[arg(long)]
    from: Option<PathBuf>,

    /// Show what would be restored without making changes
    #[arg(long)]
    dry_run: bool,

    /// Skip restoring the task list (default: restore tasks if present in backup)
    #[arg(long)]
    skip_tasks: bool,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

/// Team summary information
#[derive(Debug)]
struct TeamSummary {
    name: String,
    member_count: usize,
    created_at: u64,
}

/// Execute the teams command
pub fn execute(args: TeamsArgs) -> Result<()> {
    if let Some(command) = args.command {
        return match command {
            TeamsCommand::AddMember(add_args) => add_member(add_args),
            TeamsCommand::Resume(resume_args) => resume(resume_args),
            TeamsCommand::Cleanup(cleanup_args) => cleanup(cleanup_args),
            TeamsCommand::Backup(backup_args) => backup(backup_args),
            TeamsCommand::Restore(restore_args) => restore(restore_args),
        };
    }

    let home_dir = get_home_dir()?;
    let teams_dir = home_dir.join(".claude/teams");

    // Check if teams directory exists
    if !teams_dir.exists() {
        if args.json {
            println!("{}", json!({"teams": []}));
        } else {
            let teams_path = teams_dir.display();
            println!("No teams found (directory {teams_path} doesn't exist)");
        }
        return Ok(());
    }

    // Scan for teams
    let mut teams = Vec::new();

    for entry in fs::read_dir(&teams_dir)? {
        let entry = entry?;
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let config_path = path.join("config.json");
        if !config_path.exists() {
            continue;
        }

        // Try to read team config
        match read_team_config(&config_path) {
            Ok(config) => {
                teams.push(TeamSummary {
                    name: config.name,
                    member_count: config.members.len(),
                    created_at: config.created_at,
                });
            }
            Err(e) => {
                let path_display = path.display();
                warn!("Failed to read config for {path_display}: {e}");
            }
        }
    }

    // Sort teams by name
    teams.sort_by(|a, b| a.name.cmp(&b.name));

    // Output results
    if args.json {
        let output = json!({
            "teams": teams.iter().map(|t| json!({
                "name": t.name,
                "memberCount": t.member_count,
                "createdAt": t.created_at,
            })).collect::<Vec<_>>()
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if teams.is_empty() {
        println!("No teams found");
    } else {
        println!("Teams:");
        for team in &teams {
            let age = format_age(team.created_at);
            let name = &team.name;
            let count = team.member_count;
            println!("  {name:20}  {count} members    Created {age}");
        }
    }

    Ok(())
}

fn add_member(args: AddMemberArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let team_dir = home_dir.join(".claude/teams").join(&args.team);
    if !team_dir.exists() {
        anyhow::bail!(
            "Team '{}' not found (directory {} doesn't exist)",
            args.team,
            team_dir.display()
        );
    }

    let config_path = team_dir.join("config.json");
    if !config_path.exists() {
        anyhow::bail!("Team config not found at {}", config_path.display());
    }

    let lock_path = config_path.with_extension("lock");
    let _lock = acquire_lock(&lock_path, 5)
        .map_err(|e| anyhow::anyhow!("Failed to acquire lock for team config: {e}"))?;

    let mut team_config: TeamConfig = serde_json::from_str(&fs::read_to_string(&config_path)?)?;

    let agent_id = format!("{}@{}", args.agent, args.team);
    let existing_idx = team_config
        .members
        .iter()
        .position(|m| m.agent_id == agent_id || m.name == args.agent);
    if let Some(idx) = existing_idx {
        // If --pane-id provided, update it on the existing member
        if let Some(ref pane_id) = args.pane_id {
            team_config.members[idx].tmux_pane_id = Some(pane_id.clone());
            let serialized = serde_json::to_string_pretty(&team_config)?;
            let tmp_path = config_path.with_extension("tmp");
            let mut file = std::fs::File::create(&tmp_path)?;
            file.write_all(serialized.as_bytes())?;
            file.sync_all()?;
            drop(file);
            atomic_swap(&config_path, &tmp_path)?;
            println!("Updated tmuxPaneId for '{}' in team '{}' (paneId='{}')", args.agent, args.team, pane_id);
        } else {
            println!("Member '{}' already exists in team '{}'", args.agent, args.team);
        }
        return Ok(());
    }

    let cwd = match args.cwd {
        Some(path) => path,
        None => std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf()),
    };

    let now_ms = chrono::Utc::now().timestamp_millis() as u64;

    let agent_type = args.agent_type.clone();
    let member = agent_team_mail_core::schema::AgentMember {
        agent_id,
        name: args.agent.clone(),
        agent_type,
        model: args.model,
        prompt: None,
        color: None,
        plan_mode_required: None,
        joined_at: now_ms,
        tmux_pane_id: args.pane_id,
        cwd: cwd.to_string_lossy().to_string(),
        subscriptions: Vec::new(),
        backend_type: None,
        is_active: Some(!args.inactive),
        last_active: if !args.inactive { Some(now_ms) } else { None },
        unknown_fields: std::collections::HashMap::new(),
    };

    team_config.members.push(member);
    write_team_config(&config_path, &team_config)?;

    println!(
        "Added member '{}' to team '{}' (agentType='{}')",
        args.agent, args.team, args.agent_type
    );

    Ok(())
}

/// Resolve the current Claude Code session ID using a priority chain:
///
/// 1. Explicit value passed via `--session-id` flag
/// 2. `CLAUDE_SESSION_ID` env var (available when the Rust binary is executed
///    directly by Claude Code, but **not** exported to bash subshells)
/// 3. `/tmp/atm-session-id` file written by the gate hook on every `Task` tool
///    call — this acts as a persistent fallback after the first tool call fires
///
/// Returns `None` if no non-empty session ID can be found through any channel.
fn resolve_session_id(explicit: Option<&str>) -> Option<String> {
    if let Some(id) = explicit {
        let trimmed = id.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    if let Ok(id) = std::env::var("CLAUDE_SESSION_ID") {
        let trimmed = id.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    let session_file_path = std::env::temp_dir().join("atm-session-id");
    if let Ok(content) = std::fs::read_to_string(&session_file_path) {
        let trimmed = content.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    None
}

/// Implement `atm teams resume <team> [message]`
///
/// Updates `leadSessionId` in the team's `config.json` so that the current
/// Claude Code session becomes the team-lead.  Notifies all members with an
/// optional or default message via ATM inbox delivery.
fn resume(args: ResumeArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let team_dir = home_dir.join(".claude/teams").join(&args.team);

    // Check team exists
    let config_path = team_dir.join("config.json");
    if !config_path.exists() {
        return Err(anyhow::anyhow!(
            "No team '{}' found. Call TeamCreate(team_name=\"{}\") to create it.",
            args.team, args.team
        ));
    }

    // Resolve caller identity via full config pipeline (.atm.toml → env → flag)
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let config = resolve_config(&ConfigOverrides::default(), &current_dir, &home_dir)?;
    let caller_identity = config.core.identity.clone();

    // Only team-lead may call resume
    if caller_identity != "team-lead" {
        return Err(anyhow::anyhow!(
            "caller identity is '{}'. Only team-lead may call atm teams resume.",
            caller_identity
        ));
    }

    // Load team config
    let team_config: TeamConfig = serde_json::from_str(&fs::read_to_string(&config_path)?)?;
    let old_session_id = team_config.lead_session_id.clone();

    // Auto-backup before making any state changes — provides a safety net for every resume.
    // Non-fatal: a backup failure logs a warning but does not abort the resume.
    match do_backup(&home_dir, &args.team, false) {
        Ok(backup_path) => {
            eprintln!("Auto-backup created: {}", backup_path.display());
            // Also prune old backups after a successful auto-backup (non-fatal).
            if let Err(e) = prune_old_backups(&home_dir, &args.team, BACKUP_RETENTION_COUNT) {
                eprintln!("Warning: Auto-prune failed (proceeding anyway): {e}");
            }
        }
        Err(e) => {
            eprintln!("Warning: Auto-backup failed (proceeding anyway): {e}");
        }
    }

    // Check existing session liveness if there is one.
    // NOTE: query_session uses a bare agent name ("team-lead") without team qualification.
    // The daemon session registry is currently scoped by process/name only, not by team.
    // If a user runs multiple teams that each have a "team-lead" member, liveness queries
    // may collide across teams. This is an accepted limitation; team-scoped session queries
    // require a daemon API change that is tracked separately.
    if !old_session_id.is_empty() {
        use agent_team_mail_core::daemon_client::query_session;

        let session_result = query_session("team-lead");

        match session_result {
            Ok(Some(ref info)) => {
                // Daemon responded — check if old session is the same as ours
                let current_session_id =
                    resolve_session_id(args.session_id.as_deref()).unwrap_or_default();

                if info.session_id == current_session_id && !current_session_id.is_empty() {
                    // Re-read config for description
                    let existing_config: TeamConfig =
                        serde_json::from_str(&fs::read_to_string(&config_path)?)?;
                    let description = existing_config
                        .description
                        .clone()
                        .unwrap_or_else(|| format!("{} team", args.team));
                    println!(
                        "Already active session, no-op.\n\nTo re-establish as team-lead, call:\n  TeamCreate(team_name=\"{}\", description=\"{}\")",
                        args.team, description
                    );
                    return Ok(());
                }

                if info.alive && !args.force {
                    let short = &info.session_id[..8.min(info.session_id.len())];
                    return Err(anyhow::anyhow!(
                        "team-lead is already active in session {}... (use --force to override)",
                        short
                    ));
                }

                if info.alive && args.kill {
                    kill_process(info.process_id);
                }
            }
            Ok(None) => {
                // Daemon not running or agent not found — proceed
                warn!("daemon not running, assuming old session is dead");
            }
            Err(_) => {
                warn!("daemon not running, assuming old session is dead");
            }
        }
    }

    // Determine new session ID using a priority chain:
    // 1. --session-id flag  2. CLAUDE_SESSION_ID env  3. /tmp/atm-session-id file
    let new_session_id = resolve_session_id(args.session_id.as_deref()).ok_or_else(|| {
        anyhow::anyhow!(
            "Could not determine current session ID.\n\
             Try one of:\n\
             1. Pass --session-id <uuid> explicitly (find it in the SessionStart hook output)\n\
             2. Make any tool call first (gate hook writes the ID to /tmp/atm-session-id)\n\
             3. Ensure CLAUDE_SESSION_ID is set in your environment"
        )
    })?;

    // Atomically write updated config.json
    {
        let lock_path = config_path.with_extension("lock");
        let _lock = acquire_lock(&lock_path, 5)
            .map_err(|e| anyhow::anyhow!("Failed to acquire lock for team config: {e}"))?;

        // Re-read under lock (could have changed)
        let mut config: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path)?)?;
        config.lead_session_id = new_session_id.clone();
        // Mark all non-team-lead members as inactive to clear stale active status.
        for member in config.members.iter_mut() {
            if member.name != "team-lead" {
                member.is_active = Some(false);
            }
        }
        write_team_config(&config_path, &config)?;
    }

    // Reload config for member iteration
    let team_config: TeamConfig = serde_json::from_str(&fs::read_to_string(&config_path)?)?;

    // Send notification to all members except self
    let notify_text = args.message.clone().unwrap_or_else(|| {
        "Team-lead has rejoined the session. Context may have been reset. Please provide a brief status update.".to_string()
    });

    let mut notified = 0usize;
    let inboxes_dir = team_dir.join("inboxes");
    if !inboxes_dir.exists() {
        fs::create_dir_all(&inboxes_dir)?;
    }

    for member in &team_config.members {
        if member.name == "team-lead" {
            continue; // Don't send to self
        }

        let inbox_path = inboxes_dir.join(format!("{}.json", member.name));
        let msg = InboxMessage {
            from: "team-lead".to_string(),
            text: notify_text.clone(),
            timestamp: Utc::now().to_rfc3339(),
            read: false,
            summary: Some("Team lead session resumed".to_string()),
            message_id: Some(Uuid::new_v4().to_string()),
            unknown_fields: HashMap::new(),
        };

        match inbox_append(&inbox_path, &msg, &args.team, &member.name) {
            Ok(_) => {
                notified += 1;
            }
            Err(e) => {
                warn!("Failed to notify {}: {e}", member.name);
            }
        }
    }

    let description = team_config
        .description
        .clone()
        .unwrap_or_else(|| format!("{} team", args.team));

    println!(
        "{} resumed. leadSessionId updated. {} members notified.\n\nTo re-establish as team-lead, call:\n  TeamCreate(team_name=\"{}\", description=\"{}\")",
        args.team, notified, args.team, description
    );

    Ok(())
}

/// Send SIGTERM to a process by PID on Unix. No-op on Windows.
#[cfg_attr(not(unix), expect(unused_variables, reason = "pid unused on non-Unix platforms"))]
fn kill_process(pid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: SIGTERM (15) is a well-defined signal. We inline the extern
        // declaration to avoid a compile-time libc dependency at the crate level.
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        // SAFETY: pid is a valid OS process ID, SIGTERM=15 is well-defined.
        let _ = unsafe { kill(pid as i32, 15) }; // 15 = SIGTERM
    }

    #[cfg(not(unix))]
    {
        warn!("--kill is not supported on this platform; skipping SIGTERM");
    }
}

/// Implement `atm teams cleanup <team> [agent]`
///
/// Removes members whose daemon session record indicates the process is dead
/// (or for which no daemon session record exists), deletes their inbox files,
/// and writes the updated `config.json`.
fn cleanup(args: CleanupArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let team_dir = home_dir.join(".claude/teams").join(&args.team);
    let config_path = team_dir.join("config.json");

    if !config_path.exists() {
        return Err(anyhow::anyhow!("No team '{}' found.", args.team));
    }

    let lock_path = config_path.with_extension("lock");
    let _lock = acquire_lock(&lock_path, 5)
        .map_err(|e| anyhow::anyhow!("Failed to acquire lock for team config: {e}"))?;

    let mut team_config: TeamConfig =
        serde_json::from_str(&fs::read_to_string(&config_path)?)?;

    let inboxes_dir = team_dir.join("inboxes");
    let mut removed_names: Vec<String> = Vec::new();

    let members_to_check: Vec<_> = if let Some(ref agent_name) = args.agent {
        team_config
            .members
            .iter()
            .filter(|m| &m.name == agent_name)
            .cloned()
            .collect()
    } else {
        team_config.members.clone()
    };

    // Check daemon reachability once before iterating members.
    // NOTE: query_session uses bare agent names without team qualification because
    // the current daemon session registry is process/name scoped, not team scoped.
    // If two teams have a member with the same name, liveness queries may collide.
    // This is an accepted limitation; team-scoped queries require a daemon API change.
    let daemon_running = agent_team_mail_core::daemon_client::daemon_is_running();
    let mut skipped_names: Vec<String> = Vec::new();

    for member in &members_to_check {
        // Safety rule: never remove team-lead via cleanup.
        if member.name == "team-lead" {
            if args.agent.is_some() {
                println!("Warning: team-lead is protected and cannot be removed by cleanup");
            }
            continue;
        }

        // Query daemon for liveness.
        // Safety rule: only remove a member when the daemon *explicitly* confirms
        // the session is gone.  If the daemon is unreachable we cannot determine
        // liveness, so we skip the member unless --force is given.
        let is_dead = match agent_team_mail_core::daemon_client::query_session(&member.name) {
            Ok(Some(ref info)) => {
                // Daemon responded with an explicit record — trust it.
                !info.alive
            }
            Ok(None) if daemon_running => {
                // Daemon is running but has no record for this member — session is gone.
                true
            }
            Ok(None) => {
                // Daemon is not running: we cannot confirm liveness.
                // Skip unless --force.
                if args.force {
                    true
                } else {
                    warn!(
                        "Warning: daemon unreachable, skipping {} — use --force to override",
                        member.name
                    );
                    skipped_names.push(member.name.clone());
                    continue;
                }
            }
            Err(e) => {
                // Unexpected I/O error after connection was established — cannot
                // determine liveness; skip the member to avoid unsafe removal.
                warn!(
                    "Warning: daemon query error for {}, skipping: {e}",
                    member.name
                );
                skipped_names.push(member.name.clone());
                continue;
            }
        };

        if is_dead {
            // Remove inbox file(s) for this member
            let inbox_path = inboxes_dir.join(format!("{}.json", member.name));
            if inbox_path.exists() {
                if let Err(e) = fs::remove_file(&inbox_path) {
                    warn!("Failed to remove inbox for {}: {e}", member.name);
                }
            }
            removed_names.push(member.name.clone());
        } else {
            // Only print a warning when cleaning up a specific named agent.
            // In full-team-cleanup mode, alive members are silently skipped.
            if args.agent.is_some() {
                println!("Warning: {} is alive, skipping", member.name);
            }
        }
    }

    // Remove dead members from config
    if !removed_names.is_empty() {
        team_config
            .members
            .retain(|m| !removed_names.contains(&m.name));
        write_team_config(&config_path, &team_config)?;
    }

    if removed_names.is_empty() && skipped_names.is_empty() {
        println!("No stale members found.");
    } else {
        if !removed_names.is_empty() {
            let names = removed_names.join(", ");
            let count = removed_names.len();
            println!("Removed {count} stale member(s): {names}");
        }
        if !skipped_names.is_empty() {
            let names = skipped_names.join(", ");
            let count = skipped_names.len();
            println!("Skipped {count} member(s) (daemon unreachable): {names}");
        }
    }

    // Return an error if any members were skipped due to an unreachable daemon,
    // so the caller knows the cleanup was incomplete.
    if !skipped_names.is_empty() {
        emit_event_best_effort(EventFields {
            level: "warn",
            source: "atm",
            action: "teams_cleanup",
            team: Some(args.team.clone()),
            session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
            agent_id: std::env::var("ATM_IDENTITY").ok(),
            agent_name: std::env::var("ATM_IDENTITY").ok(),
            result: Some("incomplete".to_string()),
            count: Some(removed_names.len() as u64),
            error: Some(format!(
                "skipped={} daemon_unreachable",
                skipped_names.len()
            )),
            ..Default::default()
        });
        return Err(anyhow::anyhow!(
            "Cleanup incomplete: {} member(s) skipped because daemon liveness could not be determined. \
             Start the daemon or use --force to remove without confirmation.",
            skipped_names.len()
        ));
    }

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "teams_cleanup",
        team: Some(args.team.clone()),
        session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
        agent_id: std::env::var("ATM_IDENTITY").ok(),
        agent_name: std::env::var("ATM_IDENTITY").ok(),
        result: Some("ok".to_string()),
        count: Some(removed_names.len() as u64),
        ..Default::default()
    });

    Ok(())
}

/// Core backup logic: creates a timestamped snapshot directory and returns its path.
///
/// Copies `config.json`, all inbox files, and the tasks directory (if present) for
/// `team` into `~/.claude/teams/.backups/<team>/<timestamp>/`.
///
/// This function is extracted so that `resume()` can call it directly (auto-backup)
/// without needing a full `BackupArgs` struct.
///
/// # Errors
///
/// Returns an error if:
/// - The team directory does not exist
/// - `config.json` is missing from the team directory
/// - Any filesystem I/O operation fails (directory creation, file copy)
fn do_backup(home_dir: &Path, team: &str, json: bool) -> Result<PathBuf> {
    let team_dir = home_dir.join(".claude/teams").join(team);

    if !team_dir.exists() {
        anyhow::bail!(
            "Team '{}' not found (directory {} doesn't exist)",
            team,
            team_dir.display()
        );
    }

    let config_path = team_dir.join("config.json");
    if !config_path.exists() {
        anyhow::bail!("Team config not found at {}", config_path.display());
    }

    // Build timestamped backup directory
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let backup_dir = home_dir
        .join(".claude/teams/.backups")
        .join(team)
        .join(&timestamp);

    let backup_inboxes_dir = backup_dir.join("inboxes");
    fs::create_dir_all(&backup_inboxes_dir)?;

    // Copy config.json
    let backup_config = backup_dir.join("config.json");
    fs::copy(&config_path, &backup_config)?;

    // Copy all inbox files
    let inboxes_dir = team_dir.join("inboxes");
    let mut inbox_count = 0usize;
    if inboxes_dir.exists() {
        for entry in fs::read_dir(&inboxes_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file()
                && path.extension().and_then(|e| e.to_str()) == Some("json")
            {
                let dest = backup_inboxes_dir.join(path.file_name().unwrap());
                fs::copy(&path, &dest)?;
                inbox_count += 1;
            }
        }
    }

    // Copy tasks directory if it exists
    let tasks_src = home_dir.join(".claude/tasks").join(team);
    let tasks_dst = backup_dir.join("tasks");
    if tasks_src.exists() {
        fs::create_dir_all(&tasks_dst)?;
        for entry in fs::read_dir(&tasks_src)? {
            let entry = entry?;
            if entry.path().is_file() {
                fs::copy(entry.path(), tasks_dst.join(entry.file_name()))?;
            }
        }
    }

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "teams_backup",
        team: Some(team.to_string()),
        session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
        agent_id: std::env::var("ATM_IDENTITY").ok(),
        agent_name: std::env::var("ATM_IDENTITY").ok(),
        result: Some("ok".to_string()),
        count: Some(inbox_count as u64),
        ..Default::default()
    });

    if json {
        let output = serde_json::json!({
            "action": "backup",
            "team": team,
            "backup_path": backup_dir.to_string_lossy(),
            "inbox_count": inbox_count,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Backup created: {}", backup_dir.display());
        println!("  config.json + {} inbox file(s) copied", inbox_count);
    }

    Ok(backup_dir)
}

/// Implement `atm teams backup <team>`
///
/// Creates a timestamped snapshot of the team's `config.json`, all inbox files,
/// and the tasks directory under `~/.claude/teams/.backups/<team>/<timestamp>/`.
/// Automatically prunes old backups keeping only the last 5.
fn backup(args: BackupArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    do_backup(&home_dir, &args.team, args.json)?;
    prune_old_backups(&home_dir, &args.team, BACKUP_RETENTION_COUNT)?;
    Ok(())
}

/// Prune old backups for a team, keeping the most recent `keep` backups.
///
/// Backup directories are named with ISO timestamps so lexicographic sort equals
/// chronological order.  All but the last `keep` entries are removed.
fn prune_old_backups(home_dir: &Path, team: &str, keep: usize) -> Result<()> {
    let backups_dir = home_dir.join(".claude/teams/.backups").join(team);
    if !backups_dir.exists() {
        return Ok(());
    }

    let mut entries: Vec<_> = fs::read_dir(&backups_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();

    // Sort lexicographically by name (ISO timestamp dirs sort chronologically)
    entries.sort_by_key(|e| e.file_name());

    // Remove all but the last `keep` entries
    let to_remove = entries.len().saturating_sub(keep);
    for entry in entries.iter().take(to_remove) {
        fs::remove_dir_all(entry.path())?;
    }

    Ok(())
}

/// Implement `atm teams restore <team>`
///
/// Restores non-team-lead members and their inbox files from a backup snapshot.
/// The current `leadSessionId` is never overwritten.
fn restore(args: RestoreArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let team_dir = home_dir.join(".claude/teams").join(&args.team);
    let config_path = team_dir.join("config.json");

    if !config_path.exists() {
        anyhow::bail!(
            "Team '{}' not found (directory {} doesn't exist)",
            args.team,
            team_dir.display()
        );
    }

    // Locate the backup directory to restore from
    let backup_dir: PathBuf = if let Some(ref from_path) = args.from {
        from_path.clone()
    } else {
        // Find the latest backup by lexicographic sort of timestamps
        let backups_root = home_dir
            .join(".claude/teams/.backups")
            .join(&args.team);

        if !backups_root.exists() {
            anyhow::bail!("No backup found for team '{}'", args.team);
        }

        let mut entries: Vec<PathBuf> = fs::read_dir(&backups_root)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();

        entries.sort();
        entries.into_iter().last().ok_or_else(|| {
            anyhow::anyhow!("No backup found for team '{}'", args.team)
        })?
    };

    if !backup_dir.exists() {
        anyhow::bail!("Backup directory not found: {}", backup_dir.display());
    }

    let backup_config_path = backup_dir.join("config.json");
    if !backup_config_path.exists() {
        anyhow::bail!("Backup config not found at {}", backup_config_path.display());
    }

    // Load backup config
    let backup_config: TeamConfig =
        serde_json::from_str(&fs::read_to_string(&backup_config_path)?)?;

    // Load current config (to preserve leadSessionId and existing members)
    let current_config: TeamConfig =
        serde_json::from_str(&fs::read_to_string(&config_path)?)?;

    let backup_inboxes_dir = backup_dir.join("inboxes");
    let team_inboxes_dir = team_dir.join("inboxes");

    // Determine which members to restore (skip team-lead, skip already-present)
    let restore_members: Vec<&agent_team_mail_core::schema::AgentMember> = backup_config
        .members
        .iter()
        .filter(|m| m.name != "team-lead")
        .filter(|m| {
            !current_config
                .members
                .iter()
                .any(|cm| cm.name == m.name)
        })
        .collect();

    // Determine which inboxes to restore
    let restore_inboxes: Vec<PathBuf> = backup_config
        .members
        .iter()
        .filter(|m| m.name != "team-lead")
        .map(|m| backup_inboxes_dir.join(format!("{}.json", m.name)))
        .filter(|p| p.exists())
        .collect();

    // Count tasks that would be restored for dry-run reporting
    let tasks_backup = backup_dir.join("tasks");
    let task_count_would_restore: usize = if !args.skip_tasks && tasks_backup.exists() {
        fs::read_dir(&tasks_backup)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .count()
    } else {
        0
    };

    if args.dry_run {
        if args.json {
            let output = serde_json::json!({
                "action": "restore",
                "team": args.team,
                "backup_path": backup_dir.to_string_lossy(),
                "dry_run": true,
                "would_restore_members": restore_members.iter().map(|m| &m.name).collect::<Vec<_>>(),
                "would_restore_inboxes": restore_inboxes.iter()
                    .filter_map(|p| p.file_name())
                    .map(|n| n.to_string_lossy())
                    .collect::<Vec<_>>(),
                "would_restore_tasks": task_count_would_restore,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("Dry run — would restore from: {}", backup_dir.display());
            println!("  Members to add: {}", restore_members.len());
            for m in &restore_members {
                println!("    + {}", m.name);
            }
            println!("  Inboxes to restore: {}", restore_inboxes.len());
            for p in &restore_inboxes {
                if let Some(name) = p.file_name() {
                    println!("    + {}", name.to_string_lossy());
                }
            }
            println!("  Tasks to restore: {task_count_would_restore}");
        }
        return Ok(());
    }

    // Restore members by updating config under lock
    let lock_path = config_path.with_extension("lock");
    let member_count = restore_members.len();
    {
        let _lock = acquire_lock(&lock_path, 5)
            .map_err(|e| anyhow::anyhow!("Failed to acquire lock for team config: {e}"))?;

        let mut config: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path)?)?;

        // NEVER overwrite leadSessionId
        for member in restore_members {
            if !config.members.iter().any(|m| m.name == member.name) {
                config.members.push(member.clone());
            }
        }

        write_team_config(&config_path, &config)?;
    }

    // Restore inbox files
    if !team_inboxes_dir.exists() {
        fs::create_dir_all(&team_inboxes_dir)?;
    }

    let mut inbox_count = 0usize;
    for backup_inbox in &restore_inboxes {
        if let Some(filename) = backup_inbox.file_name() {
            let dest = team_inboxes_dir.join(filename);
            fs::copy(backup_inbox, &dest)?;
            inbox_count += 1;
        }
    }

    // Restore tasks directory unless --skip-tasks is set
    let mut task_count = 0usize;
    if !args.skip_tasks && tasks_backup.exists() {
        let tasks_dst = home_dir.join(".claude/tasks").join(&args.team);
        fs::create_dir_all(&tasks_dst)?;
        for entry in fs::read_dir(&tasks_backup)? {
            let entry = entry?;
            if entry.path().is_file() {
                fs::copy(entry.path(), tasks_dst.join(entry.file_name()))?;
                task_count += 1;
            }
        }
    }

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "teams_restore",
        team: Some(args.team.clone()),
        session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
        agent_id: std::env::var("ATM_IDENTITY").ok(),
        agent_name: std::env::var("ATM_IDENTITY").ok(),
        result: Some("ok".to_string()),
        count: Some((member_count + inbox_count + task_count) as u64),
        ..Default::default()
    });

    if args.json {
        let output = serde_json::json!({
            "action": "restore",
            "team": args.team,
            "backup_path": backup_dir.to_string_lossy(),
            "members_restored": member_count,
            "inboxes_restored": inbox_count,
            "tasks_restored": task_count,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "Restored from: {}",
            backup_dir.display()
        );
        println!(
            "  {} member(s) added, {} inbox file(s) restored, {} task file(s) restored",
            member_count, inbox_count, task_count
        );
    }

    Ok(())
}

/// Atomically write a `TeamConfig` to `config_path` via tmp file + swap.
fn write_team_config(config_path: &Path, config: &TeamConfig) -> Result<()> {
    let serialized = serde_json::to_string_pretty(config)?;
    let tmp_path = config_path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp_path)?;
    file.write_all(serialized.as_bytes())?;
    file.sync_all()?;
    drop(file);
    atomic_swap(config_path, &tmp_path)?;
    Ok(())
}

/// Read team config from file
fn read_team_config(path: &PathBuf) -> Result<TeamConfig> {
    let content = fs::read_to_string(path)?;
    let config: TeamConfig = serde_json::from_str(&content)?;
    Ok(config)
}

/// Format age as human-readable string (e.g., "2 days ago")
fn format_age(timestamp_ms: u64) -> String {
    let created = DateTime::from_timestamp((timestamp_ms / 1000) as i64, 0);

    match created {
        Some(created_dt) => {
            let now = Utc::now();
            let duration = now.signed_duration_since(created_dt);

            let days = duration.num_days();
            let hours = duration.num_hours();
            let minutes = duration.num_minutes();

            if days > 0 {
                if days == 1 {
                    "1 day ago".to_string()
                } else {
                    format!("{days} days ago")
                }
            } else if hours > 0 {
                if hours == 1 {
                    "1 hour ago".to_string()
                } else {
                    format!("{hours} hours ago")
                }
            } else if minutes > 0 {
                if minutes == 1 {
                    "1 minute ago".to_string()
                } else {
                    format!("{minutes} minutes ago")
                }
            } else {
                "just now".to_string()
            }
        }
        None => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    fn create_test_team(temp_dir: &TempDir, team_name: &str) -> PathBuf {
        let team_dir = temp_dir
            .path()
            .join(".claude/teams")
            .join(team_name);
        let inboxes_dir = team_dir.join("inboxes");
        fs::create_dir_all(&inboxes_dir).unwrap();

        let config = serde_json::json!({
            "name": team_name,
            "description": "Test team",
            "createdAt": 1739284800000i64,
            "leadAgentId": format!("team-lead@{team_name}"),
            "leadSessionId": "old-session-id-1234",
            "members": [
                {
                    "agentId": format!("team-lead@{team_name}"),
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "claude-opus-4-6",
                    "joinedAt": 1739284800000i64,
                    "tmuxPaneId": "",
                    "cwd": temp_dir.path().to_str().unwrap(),
                    "subscriptions": []
                },
                {
                    "agentId": format!("publisher@{team_name}"),
                    "name": "publisher",
                    "agentType": "general-purpose",
                    "model": "claude-haiku-4-5",
                    "joinedAt": 1739284800000i64,
                    "tmuxPaneId": "%1",
                    "cwd": temp_dir.path().to_str().unwrap(),
                    "subscriptions": [],
                    "isActive": true
                }
            ]
        });

        let config_path = team_dir.join("config.json");
        fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
        team_dir
    }

    #[test]
    fn test_format_age() {
        // Test with a timestamp from 1 day ago
        let now = Utc::now();
        let one_day_ago = now - chrono::Duration::days(1);
        let timestamp = (one_day_ago.timestamp() * 1000) as u64;

        let age = format_age(timestamp);
        assert!(age.contains("day"));
    }

    #[test]
    #[serial]
    fn test_resume_no_team_returns_err() {
        let temp_dir = TempDir::new().unwrap();
        let home_env = temp_dir.path().to_str().unwrap().to_string();

        let original = std::env::var("ATM_HOME").ok();
        let identity_original = std::env::var("ATM_IDENTITY").ok();

        // SAFETY: test-only env mutation; tests using env vars must be serialized.
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
            std::env::set_var("ATM_IDENTITY", "team-lead");
        }

        let args = ResumeArgs {
            team: "nonexistent-team".to_string(),
            message: None,
            force: false,
            kill: false,
            session_id: None,
        };

        // Should return Err with a helpful message so the CLI exits non-zero.
        let result = resume(args);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("nonexistent-team"), "error should name the team: {msg}");

        // Restore env
        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
            match identity_original {
                Some(v) => std::env::set_var("ATM_IDENTITY", v),
                None => std::env::remove_var("ATM_IDENTITY"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_resume_wrong_caller_identity() {
        let temp_dir = TempDir::new().unwrap();
        let home_env = temp_dir.path().to_str().unwrap().to_string();
        create_test_team(&temp_dir, "atm-dev");

        let original_home = std::env::var("ATM_HOME").ok();
        let original_id = std::env::var("ATM_IDENTITY").ok();

        // SAFETY: test-only env mutation
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
            std::env::set_var("ATM_IDENTITY", "arch-ctm"); // wrong caller
        }

        let args = ResumeArgs {
            team: "atm-dev".to_string(),
            message: None,
            force: false,
            kill: false,
            session_id: None,
        };

        let result = resume(args);
        // Wrong identity returns Err so the CLI exits non-zero.
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("arch-ctm"), "error should name the caller: {msg}");

        // Restore env
        // SAFETY: test-only cleanup
        unsafe {
            match original_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
            match original_id {
                Some(v) => std::env::set_var("ATM_IDENTITY", v),
                None => std::env::remove_var("ATM_IDENTITY"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_cleanup_no_team_returns_err() {
        let temp_dir = TempDir::new().unwrap();
        let home_env = temp_dir.path().to_str().unwrap().to_string();

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        let args = CleanupArgs {
            team: "nonexistent".to_string(),
            agent: None,
            force: false,
        };

        let result = cleanup(args);
        // No team found is now an Err so the CLI exits non-zero.
        assert!(result.is_err());

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_cleanup_removes_member_with_no_daemon() {
        // When daemon is not running and --force is set, members are removed without
        // daemon confirmation.  Without --force, they would be skipped (see companion test).
        let temp_dir = TempDir::new().unwrap();
        let home_env = temp_dir.path().to_str().unwrap().to_string();
        let team_dir = create_test_team(&temp_dir, "atm-dev");

        // Create publisher inbox
        let inbox = team_dir.join("inboxes/publisher.json");
        fs::write(&inbox, "[]").unwrap();

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        let args = CleanupArgs {
            team: "atm-dev".to_string(),
            agent: Some("publisher".to_string()),
            force: true,
        };

        let result = cleanup(args);
        assert!(result.is_ok());

        // Publisher's inbox should be removed
        assert!(!inbox.exists(), "inbox should have been removed");

        // config.json should no longer contain publisher
        let config_path = team_dir.join("config.json");
        let config: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(!config.members.iter().any(|m| m.name == "publisher"));

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_cleanup_skips_and_errors_when_daemon_unreachable_no_force() {
        // Without --force, cleanup must not remove members when daemon liveness
        // cannot be determined; it should return an incomplete-cleanup error.
        let temp_dir = TempDir::new().unwrap();
        let home_env = temp_dir.path().to_str().unwrap().to_string();
        let team_dir = create_test_team(&temp_dir, "atm-dev");

        let inbox = team_dir.join("inboxes/publisher.json");
        fs::write(&inbox, "[]").unwrap();

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        let args = CleanupArgs {
            team: "atm-dev".to_string(),
            agent: Some("publisher".to_string()),
            force: false,
        };

        let result = cleanup(args);
        assert!(result.is_err(), "cleanup should fail when daemon is unreachable without --force");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Cleanup incomplete"),
            "error should indicate incomplete cleanup, got: {err}"
        );

        // Member should not be removed.
        assert!(inbox.exists(), "publisher inbox should remain");
        let config_path = team_dir.join("config.json");
        let config: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(config.members.iter().any(|m| m.name == "publisher"));

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_resume_no_session_id_returns_helpful_error() {
        // When no session ID is available from any source (no --session-id flag,
        // CLAUDE_SESSION_ID env absent/empty, /tmp/atm-session-id absent/empty),
        // resume() must return an Err with a helpful message.
        let temp_dir = TempDir::new().unwrap();
        let home_env = temp_dir.path().to_str().unwrap().to_string();
        create_test_team(&temp_dir, "atm-dev");

        let original_home = std::env::var("ATM_HOME").ok();
        let original_id = std::env::var("ATM_IDENTITY").ok();
        let original_session = std::env::var("CLAUDE_SESSION_ID").ok();

        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
            std::env::set_var("ATM_IDENTITY", "team-lead");
            std::env::remove_var("CLAUDE_SESSION_ID"); // no env var
        }

        // Ensure the session-id file is absent or empty for this test
        let session_file = std::env::temp_dir().join("atm-session-id");
        let session_file_existed = session_file.exists();
        let session_file_backup: Option<String> = if session_file_existed {
            std::fs::read_to_string(&session_file).ok()
        } else {
            None
        };
        // Remove the file so the fallback chain has nothing to read
        if session_file_existed {
            let _ = std::fs::remove_file(&session_file);
        }

        let args = ResumeArgs {
            team: "atm-dev".to_string(),
            message: None,
            force: false,
            kill: false,
            session_id: None, // no explicit flag
        };

        let result = resume(args);
        assert!(result.is_err(), "resume without any session ID source should fail");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("Could not determine") || msg.contains("session ID") || msg.contains("session-id"),
            "error should explain how to fix it: {msg}"
        );

        // Restore the session file if it was there before
        if let Some(ref content) = session_file_backup {
            let _ = std::fs::write(&session_file, content);
        }

        // SAFETY: test-only cleanup
        unsafe {
            match original_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
            match original_id {
                Some(v) => std::env::set_var("ATM_IDENTITY", v),
                None => std::env::remove_var("ATM_IDENTITY"),
            }
            match original_session {
                Some(v) => std::env::set_var("CLAUDE_SESSION_ID", v),
                None => std::env::remove_var("CLAUDE_SESSION_ID"),
            }
        }
    }

    // ---- backup / restore unit tests ----

    fn set_atm_home(temp_dir: &TempDir) -> String {
        temp_dir.path().to_str().unwrap().to_string()
    }

    #[test]
    #[serial]
    fn test_backup_creates_timestamped_dir() {
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        let team_dir = create_test_team(&temp_dir, "atm-dev");

        // Create an inbox file for the publisher
        let inbox = team_dir.join("inboxes/publisher.json");
        fs::write(&inbox, "[]").unwrap();

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe { std::env::set_var("ATM_HOME", &home_env); }

        let args = BackupArgs { team: "atm-dev".to_string(), json: false };
        let result = backup(args);
        assert!(result.is_ok(), "backup should succeed: {result:?}");

        // Verify backup root exists with a timestamped subdirectory
        let backups_root = temp_dir
            .path()
            .join(".claude/teams/.backups/atm-dev");
        assert!(backups_root.exists(), "backups root should exist");

        let entries: Vec<_> = fs::read_dir(&backups_root)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1, "should have exactly one timestamped backup dir");

        let backup_dir = entries[0].path();
        assert!(backup_dir.join("config.json").exists(), "config.json should be backed up");
        assert!(backup_dir.join("inboxes/publisher.json").exists(), "inbox should be backed up");

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_backup_no_team_returns_err() {
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe { std::env::set_var("ATM_HOME", &home_env); }

        let args = BackupArgs { team: "nonexistent".to_string(), json: false };
        let result = backup(args);
        assert!(result.is_err(), "backup of nonexistent team should fail");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("nonexistent"), "error should name the team: {msg}");

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_restore_from_backup() {
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        let team_dir = create_test_team(&temp_dir, "atm-dev");

        // Create publisher inbox
        let inbox = team_dir.join("inboxes/publisher.json");
        fs::write(&inbox, r#"[{"from":"team-lead","text":"hello","timestamp":"2026-01-01T00:00:00Z","read":false}]"#).unwrap();

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe { std::env::set_var("ATM_HOME", &home_env); }

        // Create a backup first
        let backup_args = BackupArgs { team: "atm-dev".to_string(), json: false };
        backup(backup_args).unwrap();

        // Remove the publisher from the team (simulate loss)
        {
            let config_path = team_dir.join("config.json");
            // Create placeholder for atomic swap
            let placeholder = config_path.with_extension("placeholder");
            fs::copy(&config_path, &placeholder).unwrap();

            let mut config: TeamConfig =
                serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
            config.members.retain(|m| m.name == "team-lead");
            write_team_config(&config_path, &config).unwrap();
            let _ = fs::remove_file(&placeholder);
        }
        // Remove inbox too
        fs::remove_file(&inbox).unwrap();

        // Now restore
        let restore_args = RestoreArgs {
            team: "atm-dev".to_string(),
            from: None,
            dry_run: false,
            skip_tasks: false,
            json: false,
        };
        let result = restore(restore_args);
        assert!(result.is_ok(), "restore should succeed: {result:?}");

        // Verify publisher is back in config
        let config_path = team_dir.join("config.json");
        let config: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(
            config.members.iter().any(|m| m.name == "publisher"),
            "publisher should be restored to config"
        );

        // Verify inbox is restored
        assert!(inbox.exists(), "publisher inbox should be restored");

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_restore_dry_run_makes_no_changes() {
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        let team_dir = create_test_team(&temp_dir, "atm-dev");

        let inbox = team_dir.join("inboxes/publisher.json");
        fs::write(&inbox, "[]").unwrap();

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe { std::env::set_var("ATM_HOME", &home_env); }

        // Create backup
        backup(BackupArgs { team: "atm-dev".to_string(), json: false }).unwrap();

        // Remove publisher
        {
            let config_path = team_dir.join("config.json");
            let mut config: TeamConfig =
                serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
            config.members.retain(|m| m.name == "team-lead");
            // Write directly (no atomic swap needed in tests)
            fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
        }
        fs::remove_file(&inbox).unwrap();

        // Dry-run restore
        let restore_args = RestoreArgs {
            team: "atm-dev".to_string(),
            from: None,
            dry_run: true,
            skip_tasks: false,
            json: false,
        };
        let result = restore(restore_args);
        assert!(result.is_ok(), "dry-run restore should succeed: {result:?}");

        // Publisher should NOT be in config after dry run
        let config_path = team_dir.join("config.json");
        let config: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(
            !config.members.iter().any(|m| m.name == "publisher"),
            "publisher should NOT be added by dry run"
        );

        // Inbox should NOT exist after dry run
        assert!(!inbox.exists(), "publisher inbox should NOT be restored by dry run");

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_restore_skips_team_lead() {
        // team-lead in a backup should never be added again as a duplicate
        // and never replace the current team-lead entry.
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        create_test_team(&temp_dir, "atm-dev");

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe { std::env::set_var("ATM_HOME", &home_env); }

        // Create a backup
        backup(BackupArgs { team: "atm-dev".to_string(), json: false }).unwrap();

        let restore_args = RestoreArgs {
            team: "atm-dev".to_string(),
            from: None,
            dry_run: false,
            skip_tasks: false,
            json: false,
        };
        restore(restore_args).unwrap();

        // team-lead should appear exactly once
        let team_dir = temp_dir.path().join(".claude/teams/atm-dev");
        let config_path = team_dir.join("config.json");
        let config: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        let lead_count = config.members.iter().filter(|m| m.name == "team-lead").count();
        assert_eq!(lead_count, 1, "team-lead should appear exactly once after restore");

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_restore_preserves_lead_session_id() {
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        let team_dir = create_test_team(&temp_dir, "atm-dev");

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe { std::env::set_var("ATM_HOME", &home_env); }

        // Backup (captures "old-session-id-1234" from create_test_team)
        backup(BackupArgs { team: "atm-dev".to_string(), json: false }).unwrap();

        // Update leadSessionId to a new value in the current config
        let new_session_id = "new-session-after-backup";
        {
            let config_path = team_dir.join("config.json");
            let mut config: TeamConfig =
                serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
            config.lead_session_id = new_session_id.to_string();
            fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
        }

        // Restore — should not overwrite leadSessionId
        let restore_args = RestoreArgs {
            team: "atm-dev".to_string(),
            from: None,
            dry_run: false,
            skip_tasks: false,
            json: false,
        };
        restore(restore_args).unwrap();

        // Verify leadSessionId is still the post-backup value
        let config_path = team_dir.join("config.json");
        let config: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            config.lead_session_id, new_session_id,
            "leadSessionId must not be overwritten by restore"
        );

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    fn test_write_team_config_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");

        // Write a minimal TeamConfig
        let cfg = TeamConfig {
            name: "test".to_string(),
            description: Some("desc".to_string()),
            created_at: 12345,
            lead_agent_id: "team-lead@test".to_string(),
            lead_session_id: "sess-abc".to_string(),
            members: vec![],
            unknown_fields: HashMap::new(),
        };

        // Create placeholder so atomic_swap has both files present (required on macOS)
        fs::write(&config_path, "{}").unwrap();

        write_team_config(&config_path, &cfg).unwrap();
        assert!(config_path.exists());

        let read_back: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(read_back.name, "test");
        assert_eq!(read_back.lead_session_id, "sess-abc");
    }

    /// Helper to create a team config with 3 dead-session members.
    fn create_test_team_multi_dead(temp_dir: &TempDir, team_name: &str) -> PathBuf {
        let team_dir = temp_dir
            .path()
            .join(".claude/teams")
            .join(team_name);
        let inboxes_dir = team_dir.join("inboxes");
        fs::create_dir_all(&inboxes_dir).unwrap();

        let config = serde_json::json!({
            "name": team_name,
            "description": "Test team",
            "createdAt": 1739284800000i64,
            "leadAgentId": format!("team-lead@{team_name}"),
            "leadSessionId": "old-session-id-1234",
            "members": [
                {
                    "agentId": format!("team-lead@{team_name}"),
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "claude-opus-4-6",
                    "joinedAt": 1739284800000i64,
                    "cwd": temp_dir.path().to_str().unwrap(),
                    "subscriptions": []
                },
                {
                    "agentId": format!("agent-a@{team_name}"),
                    "name": "agent-a",
                    "agentType": "general-purpose",
                    "model": "claude-haiku-4-5",
                    "joinedAt": 1739284800000i64,
                    "cwd": temp_dir.path().to_str().unwrap(),
                    "subscriptions": [],
                    "isActive": true
                },
                {
                    "agentId": format!("agent-b@{team_name}"),
                    "name": "agent-b",
                    "agentType": "general-purpose",
                    "model": "claude-haiku-4-5",
                    "joinedAt": 1739284800000i64,
                    "cwd": temp_dir.path().to_str().unwrap(),
                    "subscriptions": [],
                    "isActive": false
                }
            ]
        });

        let config_path = team_dir.join("config.json");
        fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        // Create inbox files for both agents
        fs::write(inboxes_dir.join("agent-a.json"), "[]").unwrap();
        fs::write(inboxes_dir.join("agent-b.json"), "[]").unwrap();

        team_dir
    }

    #[test]
    #[serial]
    fn test_resume_force_flag_bypasses_alive_check() {
        // When `--force` is passed, the resume proceeds even if the old
        // session appears alive.  Because there is no daemon running in
        // tests, query_session returns Ok(None) (treated as dead) so the
        // force path is effectively the same as the normal path — this
        // test verifies that the args struct is accepted and the function
        // completes successfully.
        let temp_dir = TempDir::new().unwrap();
        let home_env = temp_dir.path().to_str().unwrap().to_string();
        create_test_team(&temp_dir, "atm-dev");

        let original_home = std::env::var("ATM_HOME").ok();
        let original_id = std::env::var("ATM_IDENTITY").ok();
        let original_session = std::env::var("CLAUDE_SESSION_ID").ok();

        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
            std::env::set_var("ATM_IDENTITY", "team-lead");
            std::env::set_var("CLAUDE_SESSION_ID", "test-force-session-id");
        }

        let args = ResumeArgs {
            team: "atm-dev".to_string(),
            message: None,
            force: true,  // --force bypasses alive-session check
            kill: false,
            session_id: None,
        };

        let result = resume(args);
        assert!(result.is_ok(), "resume with --force should succeed: {result:?}");

        // SAFETY: test-only cleanup
        unsafe {
            match original_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
            match original_id {
                Some(v) => std::env::set_var("ATM_IDENTITY", v),
                None => std::env::remove_var("ATM_IDENTITY"),
            }
            match original_session {
                Some(v) => std::env::set_var("CLAUDE_SESSION_ID", v),
                None => std::env::remove_var("CLAUDE_SESSION_ID"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_cleanup_removes_multiple_dead_members() {
        // When no specific agent is named, cleanup removes all dead members.
        // With no daemon running and --force, non-team-lead members are removed.
        // team-lead is protected and must never be removed by cleanup.
        let temp_dir = TempDir::new().unwrap();
        let home_env = temp_dir.path().to_str().unwrap().to_string();
        let team_dir = create_test_team_multi_dead(&temp_dir, "atm-dev");

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        let args = CleanupArgs {
            team: "atm-dev".to_string(),
            agent: None, // full-team cleanup
            force: true,
        };

        let result = cleanup(args);
        assert!(result.is_ok(), "cleanup with --force should succeed: {result:?}");

        // Both agent inboxes should be removed (--force, no daemon → all treated as dead)
        assert!(!team_dir.join("inboxes/agent-a.json").exists());
        assert!(!team_dir.join("inboxes/agent-b.json").exists());

        // With --force and no daemon, non-team-lead members are removed.
        // team-lead remains present.
        let config_path = team_dir.join("config.json");
        let config: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            config.members.len(),
            1,
            "only team-lead should remain: {:?}",
            config.members.iter().map(|m| &m.name).collect::<Vec<_>>()
        );
        assert_eq!(config.members[0].name, "team-lead");

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_cleanup_force_removes_when_daemon_unreachable_single_agent() {
        // In single-agent mode (`args.agent.is_some()`), an alive member
        // should produce a warning.  Since no daemon is running, query_session
        // returns Ok(None) and with --force the member is treated as dead —
        // meaning cleanup removes it.  This test verifies the code path
        // compiles and runs without panicking.
        let temp_dir = TempDir::new().unwrap();
        let home_env = temp_dir.path().to_str().unwrap().to_string();
        let team_dir = create_test_team(&temp_dir, "atm-dev");

        let inbox = team_dir.join("inboxes/publisher.json");
        fs::write(&inbox, "[]").unwrap();

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        // Cleanup a specific agent — daemon unreachable but --force removes anyway
        let args = CleanupArgs {
            team: "atm-dev".to_string(),
            agent: Some("publisher".to_string()),
            force: true,
        };

        let result = cleanup(args);
        assert!(result.is_ok());
        assert!(!inbox.exists(), "publisher inbox should be removed");

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_resume_sets_non_team_lead_members_inactive() {
        // After a successful resume, members other than team-lead should have
        // is_active set to Some(false).
        let temp_dir = TempDir::new().unwrap();
        let home_env = temp_dir.path().to_str().unwrap().to_string();
        let team_dir = create_test_team(&temp_dir, "atm-dev");

        let original_home = std::env::var("ATM_HOME").ok();
        let original_id = std::env::var("ATM_IDENTITY").ok();
        let original_session = std::env::var("CLAUDE_SESSION_ID").ok();

        // SAFETY: test-only env mutation
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
            std::env::set_var("ATM_IDENTITY", "team-lead");
            std::env::set_var("CLAUDE_SESSION_ID", "test-inactive-session-id");
        }

        let args = ResumeArgs {
            team: "atm-dev".to_string(),
            message: None,
            force: false,
            kill: false,
            session_id: None,
        };

        let result = resume(args);
        assert!(result.is_ok(), "resume should succeed: {result:?}");

        // Read config back and verify publisher member has is_active = false
        let config_path = team_dir.join("config.json");
        let config: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();

        let publisher = config.members.iter().find(|m| m.name == "publisher")
            .expect("publisher should still be a member");
        assert_eq!(
            publisher.is_active,
            Some(false),
            "publisher.isActive should be false after resume"
        );

        // team-lead should not be touched (is_active not forced to false)
        let lead = config.members.iter().find(|m| m.name == "team-lead")
            .expect("team-lead should be a member");
        // team-lead had no is_active field in test fixture, so should still be None
        assert!(
            lead.is_active.is_none() || lead.is_active == Some(true),
            "team-lead.isActive should not be forced to false"
        );

        // SAFETY: test-only cleanup
        unsafe {
            match original_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
            match original_id {
                Some(v) => std::env::set_var("ATM_IDENTITY", v),
                None => std::env::remove_var("ATM_IDENTITY"),
            }
            match original_session {
                Some(v) => std::env::set_var("CLAUDE_SESSION_ID", v),
                None => std::env::remove_var("CLAUDE_SESSION_ID"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_resume_identity_from_atm_identity_env() {
        // resume() should read identity from ATM_IDENTITY env (via resolve_config).
        // When ATM_IDENTITY=team-lead, the resume should proceed.
        // When ATM_IDENTITY=arch-ctm, it should be rejected.
        let temp_dir = TempDir::new().unwrap();
        let home_env = temp_dir.path().to_str().unwrap().to_string();
        create_test_team(&temp_dir, "atm-dev");

        let original_home = std::env::var("ATM_HOME").ok();
        let original_id = std::env::var("ATM_IDENTITY").ok();
        let original_session = std::env::var("CLAUDE_SESSION_ID").ok();

        // SAFETY: test-only env mutation
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
            std::env::set_var("ATM_IDENTITY", "team-lead");
            std::env::set_var("CLAUDE_SESSION_ID", "test-identity-session-id");
        }

        let args = ResumeArgs {
            team: "atm-dev".to_string(),
            message: None,
            force: false,
            kill: false,
            session_id: None,
        };

        // Should succeed with team-lead identity
        let result = resume(args);
        assert!(result.is_ok(), "team-lead identity should be accepted");

        // SAFETY: test-only cleanup
        unsafe {
            match original_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
            match original_id {
                Some(v) => std::env::set_var("ATM_IDENTITY", v),
                None => std::env::remove_var("ATM_IDENTITY"),
            }
            match original_session {
                Some(v) => std::env::set_var("CLAUDE_SESSION_ID", v),
                None => std::env::remove_var("CLAUDE_SESSION_ID"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_resume_explicit_session_id() {
        // When --session-id is passed, resume uses that ID directly without
        // reading env or file.
        let temp_dir = TempDir::new().unwrap();
        let home_env = temp_dir.path().to_str().unwrap().to_string();
        let team_dir = create_test_team(&temp_dir, "atm-dev");

        let original_home = std::env::var("ATM_HOME").ok();
        let original_id = std::env::var("ATM_IDENTITY").ok();
        let original_session = std::env::var("CLAUDE_SESSION_ID").ok();

        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
            std::env::set_var("ATM_IDENTITY", "team-lead");
            std::env::remove_var("CLAUDE_SESSION_ID"); // ensure env var is absent
        }

        let explicit_id = "explicit-test-session-id-12345";
        let args = ResumeArgs {
            team: "atm-dev".to_string(),
            message: None,
            force: false,
            kill: false,
            session_id: Some(explicit_id.to_string()),
        };

        let result = resume(args);
        assert!(result.is_ok(), "resume with explicit session_id should succeed: {result:?}");

        // Verify the config was updated with the explicit session ID
        let config_path = team_dir.join("config.json");
        let config: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(config.lead_session_id, explicit_id);

        // SAFETY: test-only cleanup
        unsafe {
            match original_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
            match original_id {
                Some(v) => std::env::set_var("ATM_IDENTITY", v),
                None => std::env::remove_var("ATM_IDENTITY"),
            }
            match original_session {
                Some(v) => std::env::set_var("CLAUDE_SESSION_ID", v),
                None => std::env::remove_var("CLAUDE_SESSION_ID"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_resume_session_id_from_file_fallback() {
        // When CLAUDE_SESSION_ID env var is absent but /tmp/atm-session-id exists,
        // resume should use the file's content.
        let temp_dir = TempDir::new().unwrap();
        let home_env = temp_dir.path().to_str().unwrap().to_string();
        let team_dir = create_test_team(&temp_dir, "atm-dev");

        // Write a test session ID to the file
        let session_file = std::env::temp_dir().join("atm-session-id");
        let file_session_id = "file-session-id-67890";
        std::fs::write(&session_file, file_session_id).unwrap();

        let original_home = std::env::var("ATM_HOME").ok();
        let original_id = std::env::var("ATM_IDENTITY").ok();
        let original_session = std::env::var("CLAUDE_SESSION_ID").ok();

        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
            std::env::set_var("ATM_IDENTITY", "team-lead");
            std::env::remove_var("CLAUDE_SESSION_ID"); // ensure env var is absent
        }

        let args = ResumeArgs {
            team: "atm-dev".to_string(),
            message: None,
            force: false,
            kill: false,
            session_id: None, // no explicit flag
        };

        let result = resume(args);
        assert!(result.is_ok(), "resume with file fallback should succeed: {result:?}");

        // Verify the config was updated with the file's session ID
        let config_path = team_dir.join("config.json");
        let config: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(config.lead_session_id, file_session_id);

        // SAFETY: test-only cleanup
        unsafe {
            match original_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
            match original_id {
                Some(v) => std::env::set_var("ATM_IDENTITY", v),
                None => std::env::remove_var("ATM_IDENTITY"),
            }
            match original_session {
                Some(v) => std::env::set_var("CLAUDE_SESSION_ID", v),
                None => std::env::remove_var("CLAUDE_SESSION_ID"),
            }
        }
    }

    // ---- tasks backup / restore unit tests ----

    /// Create a tasks directory for a team with a couple of sample task files.
    fn create_test_tasks(temp_dir: &TempDir, team_name: &str) -> PathBuf {
        let tasks_dir = temp_dir
            .path()
            .join(".claude/tasks")
            .join(team_name);
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(tasks_dir.join("task-1.json"), r#"{"id":"task-1","status":"open"}"#).unwrap();
        fs::write(tasks_dir.join("task-2.json"), r#"{"id":"task-2","status":"done"}"#).unwrap();
        tasks_dir
    }

    #[test]
    #[serial]
    fn test_backup_includes_tasks() {
        // Backup should copy the tasks directory into the snapshot.
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        create_test_team(&temp_dir, "atm-dev");
        create_test_tasks(&temp_dir, "atm-dev");

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe { std::env::set_var("ATM_HOME", &home_env); }

        let args = BackupArgs { team: "atm-dev".to_string(), json: false };
        backup(args).unwrap();

        // Find the backup directory
        let backups_root = temp_dir.path().join(".claude/teams/.backups/atm-dev");
        let backup_dir = fs::read_dir(&backups_root)
            .unwrap()
            .filter_map(|e| e.ok())
            .next()
            .expect("backup dir should exist")
            .path();

        let tasks_backup = backup_dir.join("tasks");
        assert!(tasks_backup.exists(), "tasks/ should exist in backup");
        assert!(
            tasks_backup.join("task-1.json").exists(),
            "task-1.json should be in backup"
        );
        assert!(
            tasks_backup.join("task-2.json").exists(),
            "task-2.json should be in backup"
        );

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_restore_includes_tasks_by_default() {
        // Restore without --skip-tasks should copy task files back.
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        create_test_team(&temp_dir, "atm-dev");
        let tasks_dir = create_test_tasks(&temp_dir, "atm-dev");

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe { std::env::set_var("ATM_HOME", &home_env); }

        // Create backup (includes tasks)
        backup(BackupArgs { team: "atm-dev".to_string(), json: false }).unwrap();

        // Remove the tasks directory to simulate loss
        fs::remove_dir_all(&tasks_dir).unwrap();
        assert!(!tasks_dir.exists(), "tasks dir should be removed before restore");

        // Restore without --skip-tasks
        let restore_args = RestoreArgs {
            team: "atm-dev".to_string(),
            from: None,
            dry_run: false,
            skip_tasks: false,
            json: false,
        };
        restore(restore_args).unwrap();

        assert!(
            tasks_dir.join("task-1.json").exists(),
            "task-1.json should be restored"
        );
        assert!(
            tasks_dir.join("task-2.json").exists(),
            "task-2.json should be restored"
        );

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_restore_skip_tasks_flag() {
        // When --skip-tasks is set, tasks should NOT be restored even if present in backup.
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        create_test_team(&temp_dir, "atm-dev");
        let tasks_dir = create_test_tasks(&temp_dir, "atm-dev");

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe { std::env::set_var("ATM_HOME", &home_env); }

        // Create backup (includes tasks)
        backup(BackupArgs { team: "atm-dev".to_string(), json: false }).unwrap();

        // Remove the tasks directory
        fs::remove_dir_all(&tasks_dir).unwrap();

        // Restore with --skip-tasks
        let restore_args = RestoreArgs {
            team: "atm-dev".to_string(),
            from: None,
            dry_run: false,
            skip_tasks: true,
            json: false,
        };
        restore(restore_args).unwrap();

        // Tasks directory should remain absent
        assert!(!tasks_dir.exists(), "tasks dir should NOT be restored when --skip-tasks is set");

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_prune_old_backups_keeps_last_5() {
        // Create 7 backup directories, prune with keep=5, verify only last 5 remain.
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe { std::env::set_var("ATM_HOME", &home_env); }

        let backups_dir = temp_dir
            .path()
            .join(".claude/teams/.backups/my-team");
        fs::create_dir_all(&backups_dir).unwrap();

        // Create 7 fake timestamped directories (lexicographic order matters)
        let names: Vec<&str> = vec![
            "20260101T000000Z",
            "20260102T000000Z",
            "20260103T000000Z",
            "20260104T000000Z",
            "20260105T000000Z",
            "20260106T000000Z",
            "20260107T000000Z",
        ];
        for name in &names {
            fs::create_dir_all(backups_dir.join(name)).unwrap();
        }

        prune_old_backups(temp_dir.path(), "my-team", 5).unwrap();

        let remaining: Vec<_> = fs::read_dir(&backups_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(remaining.len(), 5, "should keep only 5 backups after prune");

        // The oldest 2 should have been removed
        assert!(!backups_dir.join("20260101T000000Z").exists(), "oldest backup should be pruned");
        assert!(!backups_dir.join("20260102T000000Z").exists(), "second oldest should be pruned");
        assert!(backups_dir.join("20260103T000000Z").exists(), "third oldest should remain");
        assert!(backups_dir.join("20260107T000000Z").exists(), "newest should remain");

        // SAFETY: test-only cleanup
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    fn test_prune_no_backups_is_noop() {
        // prune_old_backups on a nonexistent directory should return Ok without error.
        let temp_dir = TempDir::new().unwrap();
        // The .backups directory does not exist — this must be a clean no-op.
        let result = prune_old_backups(temp_dir.path(), "ghost-team", 5);
        assert!(result.is_ok(), "prune on nonexistent dir should be Ok: {result:?}");
    }
}
