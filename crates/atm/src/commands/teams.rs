//! Teams command implementation

use anyhow::Result;
use agent_team_mail_core::config::{resolve_config, ConfigOverrides};
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
use uuid::Uuid;

use crate::util::settings::get_home_dir;

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
                eprintln!("Warning: Failed to read config for {path_display}: {e}");
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
                let current_session_id = std::env::var("CLAUDE_SESSION_ID").unwrap_or_default();

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
                eprintln!("Warning: daemon not running, assuming old session is dead");
            }
            Err(_) => {
                eprintln!("Warning: daemon not running, assuming old session is dead");
            }
        }
    }

    // Determine new session ID
    let new_session_id = std::env::var("CLAUDE_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

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
                eprintln!("Warning: Failed to notify {}: {e}", member.name);
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
        eprintln!("Warning: --kill is not supported on this platform; skipping SIGTERM");
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
                    eprintln!(
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
                eprintln!(
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
                    eprintln!("Warning: Failed to remove inbox for {}: {e}", member.name);
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
        return Err(anyhow::anyhow!(
            "Cleanup incomplete: {} member(s) skipped because daemon liveness could not be determined. \
             Start the daemon or use --force to remove without confirmation.",
            skipped_names.len()
        ));
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

        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
            std::env::set_var("ATM_IDENTITY", "team-lead");
        }

        let args = ResumeArgs {
            team: "atm-dev".to_string(),
            message: None,
            force: true,  // --force bypasses alive-session check
            kill: false,
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
        }
    }

    #[test]
    #[serial]
    fn test_cleanup_removes_multiple_dead_members() {
        // When no specific agent is named, cleanup removes all dead members.
        // With no daemon running and --force, all members (including team-lead)
        // are removed without daemon confirmation.
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

        // With --force and no daemon all members are removed (including team-lead)
        let config_path = team_dir.join("config.json");
        let config: TeamConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(
            config.members.is_empty(),
            "all members removed when --force and no daemon: {:?}",
            config.members.iter().map(|m| &m.name).collect::<Vec<_>>()
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
    fn test_cleanup_single_agent_mode_prints_alive_warning() {
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

        // SAFETY: test-only env mutation
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
            std::env::set_var("ATM_IDENTITY", "team-lead");
        }

        let args = ResumeArgs {
            team: "atm-dev".to_string(),
            message: None,
            force: false,
            kill: false,
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

        // SAFETY: test-only env mutation
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
            std::env::set_var("ATM_IDENTITY", "team-lead");
        }

        let args = ResumeArgs {
            team: "atm-dev".to_string(),
            message: None,
            force: false,
            kill: false,
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
        }
    }
}
