//! Teams command implementation

use agent_team_mail_core::config::{ConfigOverrides, resolve_config, resolve_identity};
use agent_team_mail_core::daemon_client::{LaunchConfig, launch_agent, query_session_for_team};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::io::atomic::atomic_swap;
use agent_team_mail_core::io::lock::acquire_lock;
use agent_team_mail_core::model_registry::ModelId;
use agent_team_mail_core::schema::{BackendType, TeamConfig};
use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::Args;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use tracing::warn;

use crate::commands::runtime_adapter::{RuntimeKind, SpawnSpec, adapter_for_runtime};
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
    /// Spawn a team member via daemon with runtime-specific adapter behavior
    Spawn(SpawnArgs),
    /// Join an existing team and print a copy-pastable resume launch command
    Join(JoinArgs),
    /// Add a member to a team without launching an agent
    AddMember(AddMemberArgs),
    /// Update fields on an existing team member
    UpdateMember(UpdateMemberArgs),
    /// Resume as team-lead for an existing team (updates leadSessionId)
    Resume(ResumeArgs),
    /// Remove stale (dead) members from a team
    Cleanup(CleanupArgs),
    /// Back up a team's config, inboxes, and tasks to a timestamped snapshot directory
    Backup(BackupArgs),
    /// Restore a team's members, inboxes, and tasks from a backup snapshot
    Restore(RestoreArgs),
}

/// Spawn a team member (runtime-aware daemon launch)
#[derive(Args, Debug)]
pub struct SpawnArgs {
    /// Agent name
    agent: String,

    /// Team name (defaults to configured default team)
    #[arg(long)]
    team: Option<String>,

    /// Runtime selector
    #[arg(long, value_enum, default_value_t = RuntimeKind::Codex)]
    runtime: RuntimeKind,

    /// Optional model override
    #[arg(long)]
    model: Option<String>,

    /// Optional sandbox mode override (`true` or `false`)
    #[arg(long)]
    sandbox: Option<bool>,

    /// Optional approval mode override
    #[arg(long)]
    approval_mode: Option<String>,

    /// Optional system prompt path (used by Gemini as `GEMINI_SYSTEM_MD`)
    #[arg(long)]
    system_prompt: Option<PathBuf>,

    /// Resume previous runtime session for this agent
    #[arg(long)]
    resume: bool,

    /// Explicit runtime session ID for resume (otherwise resolved from daemon registry)
    #[arg(long)]
    resume_session_id: Option<String>,

    /// Readiness timeout in seconds
    #[arg(long, default_value_t = 30)]
    timeout: u32,

    /// Additional environment variables (KEY=VALUE, repeatable)
    #[arg(long = "env", value_name = "KEY=VALUE")]
    env: Vec<String>,

    /// Optional prompt to send after startup
    #[arg(long)]
    prompt: Option<String>,

    /// Canonical spawn directory (`--cwd` is accepted as an alias)
    #[arg(long, alias = "cwd")]
    folder: Vec<PathBuf>,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

/// Join an existing team and return launch guidance for the joined identity
#[derive(Args, Debug)]
pub struct JoinArgs {
    /// Agent name to add/join
    agent: String,

    /// Team name (required in self-join mode; optional verification in team-lead-initiated mode)
    #[arg(long)]
    team: Option<String>,

    /// Agent type to persist (default: codex, matching add-member behavior)
    #[arg(long)]
    agent_type: Option<String>,

    /// Model identifier validated against the ATM model registry
    #[arg(long)]
    model: Option<String>,

    /// Working directory used in roster entry and launch command
    #[arg(long)]
    folder: Option<PathBuf>,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

/// Add a member to a team (no agent spawn)
#[derive(Args, Debug, Clone)]
pub struct AddMemberArgs {
    /// Team name
    team: String,

    /// Agent name (unique within team)
    agent: String,

    /// Agent type (e.g., "codex", "human", "plugin:ci_monitor")
    #[arg(long, default_value = "codex")]
    agent_type: String,

    /// Model identifier validated against the ATM model registry.
    ///
    /// Use `"custom:<id>"` for models not in the built-in list.
    /// Defaults to `"unknown"`.
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

    /// Session or agent ID for external agents (stored for liveness tracking)
    #[arg(long)]
    session_id: Option<String>,

    /// Backend type for external agents.
    ///
    /// Valid values: `claude-code`, `codex`, `gemini`, `external`, `human:<username>`.
    /// `human:` alone (without a username) is rejected.
    #[arg(long)]
    backend_type: Option<String>,
}

/// Update fields on an existing team member
#[derive(Args, Debug)]
pub struct UpdateMemberArgs {
    /// Team name
    team: String,

    /// Agent name to update
    agent: String,

    /// Update session ID
    #[arg(long)]
    session_id: Option<String>,

    /// Update model (validated against the ATM model registry)
    #[arg(long)]
    model: Option<String>,

    /// Update tmux pane ID
    #[arg(long)]
    pane_id: Option<String>,

    /// Update backend type (claude-code, codex, gemini, external, human:<username>)
    #[arg(long)]
    backend_type: Option<String>,

    /// Set active status
    #[arg(long)]
    active: Option<bool>,
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
    /// the platform temp-dir session-id file (written by the gate hook on every tool call).
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
            TeamsCommand::Spawn(spawn_args) => spawn_member(spawn_args),
            TeamsCommand::Join(join_args) => join_member(join_args),
            TeamsCommand::AddMember(add_args) => add_member(add_args),
            TeamsCommand::UpdateMember(update_args) => update_member(update_args),
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

fn spawn_member(args: SpawnArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;
    let launch_dir = resolve_spawn_folder(&args.folder, &current_dir)?;
    let config = resolve_config(
        &ConfigOverrides {
            team: args.team.clone(),
            ..Default::default()
        },
        &current_dir,
        &home_dir,
    )?;
    let team_name = args
        .team
        .clone()
        .unwrap_or_else(|| config.core.default_team.clone());

    let parsed_env = parse_env_vars(&args.env)?;

    let resolved_resume_session_id = if args.resume_session_id.is_some() {
        args.resume_session_id.clone()
    } else if args.resume {
        query_session_for_team(&team_name, &args.agent)
            .ok()
            .flatten()
            .map(|info| info.runtime_session_id.unwrap_or(info.session_id))
    } else {
        None
    };

    let spec = SpawnSpec {
        team: team_name.clone(),
        agent: args.agent.clone(),
        cwd: launch_dir.clone(),
        model: args.model.clone(),
        sandbox: args.sandbox,
        approval_mode: args.approval_mode.clone(),
        resume: args.resume,
        resume_session_id: resolved_resume_session_id,
        system_prompt: args.system_prompt.clone(),
    };

    let adapter = adapter_for_runtime(&args.runtime);
    let command = adapter.build_command(&spec)?;
    let mut env_vars = adapter.build_env(&spec, &home_dir)?;
    for (k, v) in parsed_env {
        env_vars.insert(k, v);
    }

    let launch_config = LaunchConfig {
        agent: args.agent.clone(),
        team: team_name.clone(),
        command,
        prompt: args.prompt.clone(),
        timeout_secs: args.timeout,
        env_vars,
        runtime: Some(runtime_name(&args.runtime).to_string()),
        resume_session_id: spec.resume_session_id.clone(),
    };

    let result = match launch_agent(&launch_config) {
        Ok(Some(r)) => r,
        Ok(None) => {
            if args.json {
                let output = json!({
                    "error": "Daemon is not running. Start it with: atm-daemon",
                    "agent": args.agent,
                    "team": team_name,
                    "runtime": runtime_name(&args.runtime),
                    "folder": launch_dir.to_string_lossy(),
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                eprintln!("Error: Daemon is not running. Start it with: atm-daemon");
            }
            std::process::exit(1);
        }
        Err(e) => {
            if args.json {
                let output = json!({
                    "error": e.to_string(),
                    "agent": args.agent,
                    "team": team_name,
                    "runtime": runtime_name(&args.runtime),
                    "folder": launch_dir.to_string_lossy(),
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                eprintln!("Error: {e}");
            }
            std::process::exit(1);
        }
    };

    if args.json {
        let output = json!({
            "agent": result.agent,
            "team": team_name,
            "runtime": runtime_name(&args.runtime),
            "folder": launch_dir.to_string_lossy(),
            "pane_id": result.pane_id,
            "state": result.state,
            "warning": result.warning,
            "resume_session_id": spec.resume_session_id,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Launched agent: {}", result.agent);
        println!("  team:    {}", team_name);
        println!("  runtime: {}", runtime_name(&args.runtime));
        println!("  folder:  {}", launch_dir.display());
        println!("  pane:    {}", result.pane_id);
        println!("  state:   {}", result.state);
        if let Some(session_id) = spec.resume_session_id {
            println!("  resume:  {session_id}");
        }
        if let Some(warning) = result.warning {
            eprintln!("Warning: {warning}");
        }
    }

    Ok(())
}

fn runtime_name(runtime: &RuntimeKind) -> &'static str {
    match runtime {
        RuntimeKind::Claude => "claude",
        RuntimeKind::Codex => "codex",
        RuntimeKind::Gemini => "gemini",
        RuntimeKind::Opencode => "opencode",
    }
}

fn canonicalize_directory(path: &Path, flag: &str) -> Result<PathBuf> {
    if !path.exists() {
        anyhow::bail!(
            "{} path '{}' does not exist. Provide an existing directory.",
            flag,
            path.display()
        );
    }
    if !path.is_dir() {
        anyhow::bail!(
            "{} path '{}' is not a directory. Provide a directory path.",
            flag,
            path.display()
        );
    }
    fs::canonicalize(path)
        .map_err(|e| anyhow::anyhow!("Failed to resolve {} path '{}': {e}.", flag, path.display()))
}

fn resolve_spawn_folder(folder_args: &[PathBuf], current_dir: &Path) -> Result<PathBuf> {
    let current = fs::canonicalize(current_dir).map_err(|e| {
        anyhow::anyhow!(
            "Failed to resolve current directory '{}': {e}.",
            current_dir.display()
        )
    })?;

    if folder_args.is_empty() {
        return Ok(current);
    }

    let mut canonical_paths = Vec::with_capacity(folder_args.len());
    for raw_path in folder_args {
        // `folder` accepts values from both --folder and its --cwd alias.
        canonical_paths.push(canonicalize_directory(raw_path, "--folder/--cwd")?);
    }

    let first = canonical_paths[0].clone();
    let mismatched = canonical_paths.iter().any(|p| p != &first);
    if mismatched {
        anyhow::bail!(
            "--folder/--cwd values resolve to different directories: {}. \
             Provide a single path or matching paths.",
            canonical_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(first)
}

fn parse_env_vars(pairs: &[String]) -> Result<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    for pair in pairs {
        let eq_pos = pair.find('=').ok_or_else(|| {
            anyhow::anyhow!("Invalid --env value '{pair}': expected KEY=VALUE format")
        })?;
        let key = &pair[..eq_pos];
        let value = &pair[eq_pos + 1..];
        if key.is_empty() {
            anyhow::bail!("Invalid --env value '{pair}': key must not be empty");
        }
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoinMode {
    TeamLeadInitiated,
    SelfJoin,
}

impl JoinMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::TeamLeadInitiated => "team_lead_initiated",
            Self::SelfJoin => "self_join",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddMemberOutcome {
    Added,
    UpdatedInactive,
    UpdatedPaneId,
    AlreadyRegistered,
}

fn resolve_join_folder(folder: Option<PathBuf>) -> Result<PathBuf> {
    let selected = match folder {
        Some(path) => path,
        None => std::env::current_dir()?,
    };
    canonicalize_directory(&selected, "--folder")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'"'"'"#))
}

fn resolve_caller_team_context(home_dir: &Path, current_dir: &Path) -> Result<Option<String>> {
    let config = resolve_config(&ConfigOverrides::default(), current_dir, home_dir)?;
    let configured_team = config.core.default_team.trim().to_string();
    if configured_team.is_empty() {
        return Ok(None);
    }

    let caller_identity = resolve_identity(&config.core.identity, &config.roles, &config.aliases);
    if caller_identity.is_empty() || caller_identity == "human" {
        return Ok(None);
    }

    let config_path = home_dir
        .join(".claude/teams")
        .join(&configured_team)
        .join("config.json");
    if !config_path.exists() {
        return Ok(None);
    }

    let team_config = read_team_config(&config_path)?;
    let is_member = team_config
        .members
        .iter()
        .any(|m| m.name == caller_identity);
    if is_member {
        Ok(Some(configured_team))
    } else {
        Ok(None)
    }
}

fn join_member(args: JoinArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let caller_team = resolve_caller_team_context(&home_dir, &current_dir)?;

    let (mode, target_team) = if let Some(caller_team) = caller_team {
        if let Some(requested_team) = args.team.as_deref()
            && requested_team != caller_team
        {
            anyhow::bail!(
                "--team '{}' does not match your current team '{}'. \
                 Remove --team or pass --team '{}'.",
                requested_team,
                caller_team,
                caller_team
            );
        }
        (JoinMode::TeamLeadInitiated, caller_team)
    } else {
        let explicit_team = args.team.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "--team is required when caller has no current team context. \
                 Set ATM_TEAM/ATM_IDENTITY for team-lead-initiated mode, or pass --team <team>."
            )
        })?;
        (JoinMode::SelfJoin, explicit_team)
    };

    let team_dir = home_dir.join(".claude/teams").join(&target_team);
    if !team_dir.exists() {
        anyhow::bail!(
            "Team '{}' not found (directory {} doesn't exist)",
            target_team,
            team_dir.display()
        );
    }

    let config_path = team_dir.join("config.json");
    if !config_path.exists() {
        anyhow::bail!("Team config not found at {}", config_path.display());
    }

    let folder = resolve_join_folder(args.folder)?;
    let folder_display = folder.to_string_lossy().to_string();
    let member_outcome = add_member_internal(AddMemberArgs {
        team: target_team.clone(),
        agent: args.agent.clone(),
        agent_type: args.agent_type.unwrap_or_else(|| "codex".to_string()),
        model: args.model.unwrap_or_else(|| "unknown".to_string()),
        cwd: Some(folder.clone()),
        inactive: false,
        pane_id: None,
        session_id: None,
        backend_type: None,
    })?;

    let roster_state = match member_outcome {
        AddMemberOutcome::Added | AddMemberOutcome::UpdatedInactive => "added",
        AddMemberOutcome::UpdatedPaneId | AddMemberOutcome::AlreadyRegistered => "already_present",
    };

    let launch_command = format!(
        "cd {} && env ATM_TEAM={} ATM_IDENTITY={} claude --resume",
        shell_quote(&folder_display),
        shell_quote(&target_team),
        shell_quote(&args.agent)
    );

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "teams_join",
        team: Some(target_team.clone()),
        session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
        agent_id: std::env::var("ATM_IDENTITY").ok(),
        agent_name: std::env::var("ATM_IDENTITY").ok(),
        target: Some(args.agent.clone()),
        result: Some("ok".to_string()),
        spawn_mode: Some(mode.as_str().to_string()),
        ..Default::default()
    });

    if args.json {
        let output = json!({
            "team": target_team,
            "agent": args.agent,
            "folder": folder_display,
            "launch_command": launch_command,
            "mode": mode.as_str(),
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    println!("Mode: {}", mode.as_str());
    println!("Team: {}", target_team);
    println!("Agent: {}", args.agent);
    println!("Folder: {}", folder_display);
    println!("Roster: {}", roster_state);
    if roster_state == "already_present" {
        println!("Requested team member is already in the roster.");
    }
    println!("Launch command:");
    println!("{launch_command}");
    Ok(())
}

fn add_member(args: AddMemberArgs) -> Result<()> {
    match add_member_internal(args.clone())? {
        AddMemberOutcome::UpdatedPaneId => {
            println!(
                "Updated tmuxPaneId for '{}' in team '{}' (paneId='{}')",
                args.agent,
                args.team,
                args.pane_id.as_deref().unwrap_or_default()
            );
        }
        AddMemberOutcome::AlreadyRegistered => {
            println!("Member already registered (idempotent)");
        }
        AddMemberOutcome::UpdatedInactive => {
            println!(
                "Updated inactive member '{}' in team '{}' (agentType='{}')",
                args.agent, args.team, args.agent_type
            );
        }
        AddMemberOutcome::Added => {
            println!(
                "Added member '{}' to team '{}' (agentType='{}')",
                args.agent, args.team, args.agent_type
            );
        }
    }
    Ok(())
}

fn add_member_internal(args: AddMemberArgs) -> Result<AddMemberOutcome> {
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

    // Validate model and backend_type before acquiring the lock so we fail
    // fast on bad input without ever touching the config file.
    let parsed_model =
        ModelId::from_str(&args.model).map_err(|e| anyhow::anyhow!("Invalid --model: {e}"))?;

    let parsed_backend_type: Option<BackendType> = if let Some(ref bt_str) = args.backend_type {
        Some(
            BackendType::from_str(bt_str)
                .map_err(|e| anyhow::anyhow!("Invalid --backend-type: {e}"))?,
        )
    } else {
        None
    };

    let lock_path = config_path.with_extension("lock");
    let _lock = acquire_lock(&lock_path, 5)
        .map_err(|e| anyhow::anyhow!("Failed to acquire lock for team config: {e}"))?;

    let mut team_config: TeamConfig = serde_json::from_str(&fs::read_to_string(&config_path)?)?;

    let agent_id = format!("{}@{}", args.agent, args.team);

    // Check for MCP collision or idempotent re-add.
    if let Some(idx) = team_config
        .members
        .iter()
        .position(|m| m.name == args.agent)
    {
        let existing = &team_config.members[idx];

        // Idempotent: same agent_id already present → no-op.
        if existing.agent_id == agent_id {
            // If --pane-id provided, update it on the existing member and return.
            if let Some(ref pane_id) = args.pane_id {
                team_config.members[idx].tmux_pane_id = Some(pane_id.clone());
                write_team_config(&config_path, &team_config)?;
                return Ok(AddMemberOutcome::UpdatedPaneId);
            } else {
                return Ok(AddMemberOutcome::AlreadyRegistered);
            }
        }

        // Collision guard: active member with a different agent_id → reject.
        if existing.is_active == Some(true) {
            anyhow::bail!(
                "Member '{}' already exists and is active with a different agent_id. \
                 Use update-member to modify existing members.",
                args.agent
            );
        }

        // Inactive member with same name but different agent_id → update in place.
        let cwd = match args.cwd {
            Some(ref path) => path.to_string_lossy().to_string(),
            None => std::env::current_dir()
                .unwrap_or_else(|_| Path::new(".").to_path_buf())
                .to_string_lossy()
                .to_string(),
        };
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let m = &mut team_config.members[idx];
        m.agent_id = agent_id;
        m.agent_type = args.agent_type.clone();
        m.model = args.model.clone();
        m.cwd = cwd;
        m.is_active = Some(!args.inactive);
        m.last_active = if !args.inactive { Some(now_ms) } else { None };
        m.session_id = args.session_id.clone();
        m.external_backend_type = parsed_backend_type;
        m.external_model = Some(parsed_model);
        if let Some(ref pane_id) = args.pane_id {
            m.tmux_pane_id = Some(pane_id.clone());
        }
        write_team_config(&config_path, &team_config)?;
        return Ok(AddMemberOutcome::UpdatedInactive);
    }

    // Brand-new member.
    let cwd = match args.cwd {
        Some(path) => path,
        None => std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf()),
    };

    let now_ms = chrono::Utc::now().timestamp_millis() as u64;

    let member = agent_team_mail_core::schema::AgentMember {
        agent_id,
        name: args.agent.clone(),
        agent_type: args.agent_type.clone(),
        model: args.model.clone(),
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
        session_id: args.session_id,
        external_backend_type: parsed_backend_type,
        external_model: Some(parsed_model),
        unknown_fields: std::collections::HashMap::new(),
    };

    team_config.members.push(member);
    write_team_config(&config_path, &team_config)?;
    Ok(AddMemberOutcome::Added)
}

/// Implement `atm teams update-member <team> <agent> [flags]`
///
/// Updates selected fields on an existing team member.  Only fields whose
/// corresponding flag is `Some` are modified; omitted flags leave the current
/// value unchanged.
fn update_member(args: UpdateMemberArgs) -> Result<()> {
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

    // Validate inputs before locking.
    let parsed_model: Option<ModelId> = if let Some(ref m) = args.model {
        Some(ModelId::from_str(m).map_err(|e| anyhow::anyhow!("Invalid --model: {e}"))?)
    } else {
        None
    };

    let parsed_backend_type: Option<BackendType> = if let Some(ref bt) = args.backend_type {
        Some(
            BackendType::from_str(bt)
                .map_err(|e| anyhow::anyhow!("Invalid --backend-type: {e}"))?,
        )
    } else {
        None
    };

    let lock_path = config_path.with_extension("lock");
    let _lock = acquire_lock(&lock_path, 5)
        .map_err(|e| anyhow::anyhow!("Failed to acquire lock for team config: {e}"))?;

    let mut team_config: TeamConfig = serde_json::from_str(&fs::read_to_string(&config_path)?)?;

    let member = team_config
        .members
        .iter_mut()
        .find(|m| m.name == args.agent)
        .ok_or_else(|| {
            anyhow::anyhow!("Member '{}' not found in team '{}'", args.agent, args.team)
        })?;

    let mut updated_fields: Vec<&str> = Vec::new();

    if let Some(session_id) = args.session_id {
        member.session_id = Some(session_id);
        updated_fields.push("session_id");
    }

    if let Some(model) = parsed_model {
        member.model = model.to_string();
        member.external_model = Some(model);
        updated_fields.push("model");
    }

    if let Some(pane_id) = args.pane_id {
        member.tmux_pane_id = Some(pane_id);
        updated_fields.push("tmux_pane_id");
    }

    if let Some(bt) = parsed_backend_type {
        member.external_backend_type = Some(bt);
        updated_fields.push("backend_type");
    }

    if let Some(active) = args.active {
        member.is_active = Some(active);
        if active {
            let now_ms = chrono::Utc::now().timestamp_millis() as u64;
            member.last_active = Some(now_ms);
        }
        updated_fields.push("is_active");
    }

    if updated_fields.is_empty() {
        println!(
            "No fields to update. Specify at least one flag (--session-id, --model, --pane-id, --backend-type, --active)."
        );
        return Ok(());
    }

    write_team_config(&config_path, &team_config)?;

    println!(
        "Updated member '{}' in team '{}': {}",
        args.agent,
        args.team,
        updated_fields.join(", ")
    );

    Ok(())
}

/// Implement `atm teams resume <team> [message]`
///
/// Performs R.1 handoff semantics:
/// - Refuses takeover when team-lead is actively running for the team.
/// - For inactive/no session: creates a flat backup snapshot and removes the
///   active team directory, prompting the caller to re-establish via TeamCreate.
fn resume(args: ResumeArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let team_dir = home_dir.join(".claude/teams").join(&args.team);

    // Check team exists
    let config_path = team_dir.join("config.json");
    if !config_path.exists() {
        return Err(anyhow::anyhow!(
            "No team '{}' found. Call TeamCreate(team_name=\"{}\") to create it.",
            args.team,
            args.team
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

    use agent_team_mail_core::daemon_client::query_session_for_team;

    let requested_session_id = args
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);

    match query_session_for_team(&args.team, "team-lead") {
        Ok(Some(info)) => {
            // --session-id must match daemon's tracked lead session when provided.
            if let Some(ref sid) = requested_session_id {
                if &info.session_id != sid {
                    return Err(anyhow::anyhow!(
                        "--session-id '{}' does not match daemon active record '{}'",
                        sid,
                        info.session_id
                    ));
                }
            }

            if info.alive {
                return Err(anyhow::anyhow!(
                    "team-lead already active in PID {}, cannot steal identity",
                    info.process_id
                ));
            }

            // Stale/dead record path: require explicit force to proceed.
            if args.kill {
                kill_process(info.process_id);
            }
            if !args.force {
                return Err(anyhow::anyhow!(
                    "stale team-lead session detected (PID {}). Use --force to proceed.",
                    info.process_id
                ));
            }
        }
        Ok(None) => {
            // No daemon record for this team scope — proceed.
        }
        Err(e) => {
            warn!("daemon session-query-team failed: {e}");
        }
    }

    // Snapshot first; abort if backup fails.
    let backup_path = do_backup(&home_dir, &args.team, false)?;
    prune_old_backups(&home_dir, &args.team, BACKUP_RETENTION_COUNT)?;

    // Remove active team directory so TeamCreate can re-establish the team lead context.
    fs::remove_dir_all(&team_dir)?;

    let maybe_msg = args
        .message
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("team archived for re-establishment");
    println!(
        "{}.\nBackup: {}\nCall TeamCreate({}) to re-establish as team-lead",
        maybe_msg,
        backup_path.display(),
        args.team
    );

    Ok(())
}

/// Send SIGTERM to a process by PID on Unix. No-op on Windows.
#[cfg_attr(
    not(unix),
    expect(unused_variables, reason = "pid unused on non-Unix platforms")
)]
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

    let mut team_config: TeamConfig = serde_json::from_str(&fs::read_to_string(&config_path)?)?;

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
        //
        // External agents (those with `external_backend_type` set) require
        // additional conservatism: they do not use the Claude Code hook system,
        // so the daemon may have no record for them even when they are alive.
        // We only remove an external agent when:
        //   1. It has a `session_id` set, AND
        //   2. The daemon explicitly confirms that session is dead (`alive == false`).
        // Any other outcome (no session_id, daemon unreachable, no daemon record)
        // is treated as "unknown liveness" → the external agent is kept.
        let is_external = member.external_backend_type.is_some();

        let is_dead = if is_external {
            // External agent: use session_id for the liveness query if available.
            let query_key = match &member.session_id {
                Some(sid) => sid.clone(),
                None => {
                    // No session_id → cannot confirm liveness; keep the member.
                    warn!(
                        "Warning: external agent '{}' has no session_id, skipping (unknown liveness)",
                        member.name
                    );
                    skipped_names.push(member.name.clone());
                    continue;
                }
            };

            match agent_team_mail_core::daemon_client::query_session(&query_key) {
                Ok(Some(ref info)) if !info.alive => {
                    // Daemon explicitly reports the session as dead → safe to remove.
                    true
                }
                Ok(_) => {
                    // Daemon unreachable, session alive, or no record → keep the agent.
                    warn!(
                        "Warning: external agent '{}' liveness unknown, skipping — use --force to override",
                        member.name
                    );
                    skipped_names.push(member.name.clone());
                    continue;
                }
                Err(_) => {
                    // I/O error → keep the agent to be safe.
                    warn!(
                        "Warning: daemon query error for external agent '{}', skipping",
                        member.name
                    );
                    skipped_names.push(member.name.clone());
                    continue;
                }
            }
        } else {
            // Standard Claude Code member: existing liveness logic unchanged.
            match agent_team_mail_core::daemon_client::query_session_for_team(
                &args.team,
                &member.name,
            ) {
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

/// Entry point for `atm cleanup --agent` compatibility.
pub(crate) fn cleanup_single_agent(team: String, agent: String, force: bool) -> Result<()> {
    cleanup(CleanupArgs {
        team,
        agent: Some(agent),
        force,
    })
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

    // Build timestamped backup directory with nanosecond precision to avoid
    // collisions when multiple backups are created within the same second.
    let now = chrono::Utc::now();
    let timestamp = format!(
        "{}{:09}Z",
        now.format("%Y%m%dT%H%M%S"),
        now.timestamp_subsec_nanos()
    );
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
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("json") {
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
        let backups_root = home_dir.join(".claude/teams/.backups").join(&args.team);

        if !backups_root.exists() {
            anyhow::bail!("No backup found for team '{}'", args.team);
        }

        let mut entries: Vec<PathBuf> = fs::read_dir(&backups_root)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();

        entries.sort();
        entries
            .into_iter()
            .last()
            .ok_or_else(|| anyhow::anyhow!("No backup found for team '{}'", args.team))?
    };

    if !backup_dir.exists() {
        anyhow::bail!("Backup directory not found: {}", backup_dir.display());
    }

    let backup_config_path = backup_dir.join("config.json");
    if !backup_config_path.exists() {
        anyhow::bail!(
            "Backup config not found at {}",
            backup_config_path.display()
        );
    }

    // Load backup config
    let backup_config: TeamConfig =
        serde_json::from_str(&fs::read_to_string(&backup_config_path)?)?;

    // Load current config (to preserve leadSessionId and existing members)
    let current_config: TeamConfig = serde_json::from_str(&fs::read_to_string(&config_path)?)?;

    let backup_inboxes_dir = backup_dir.join("inboxes");
    let team_inboxes_dir = team_dir.join("inboxes");

    // Determine which members to restore (skip team-lead, skip already-present)
    let restore_members: Vec<&agent_team_mail_core::schema::AgentMember> = backup_config
        .members
        .iter()
        .filter(|m| m.name != "team-lead")
        .filter(|m| !current_config.members.iter().any(|cm| cm.name == m.name))
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

        let mut config: TeamConfig = serde_json::from_str(&fs::read_to_string(&config_path)?)?;

        // NEVER overwrite leadSessionId
        for member in restore_members {
            if !config.members.iter().any(|m| m.name == member.name) {
                let mut restored = member.clone();
                // Restored members have no active runtime session until they
                // explicitly re-register with the daemon.
                restored.is_active = Some(false);
                restored.last_active = None;
                restored.session_id = None;
                config.members.push(restored);
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
        println!("Restored from: {}", backup_dir.display());
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
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn create_test_team(temp_dir: &TempDir, team_name: &str) -> PathBuf {
        let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
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
        assert!(
            msg.contains("nonexistent-team"),
            "error should name the team: {msg}"
        );

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
        assert!(
            msg.contains("arch-ctm"),
            "error should name the caller: {msg}"
        );

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
        assert!(
            result.is_err(),
            "cleanup should fail when daemon is unreachable without --force"
        );
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
    fn test_resume_no_session_id_archives_team_when_daemon_unreachable() {
        // R.1 contract: if no active team-lead record is found, resume archives
        // the team for re-establishment, independent of CLAUDE_SESSION_ID.
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
            std::env::remove_var("CLAUDE_SESSION_ID");
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
        assert!(
            !team_dir.exists(),
            "team directory should be removed after archive handoff"
        );
        let backups_root = temp_dir.path().join(".claude/teams/.backups/atm-dev");
        assert!(backups_root.exists(), "backup root should exist");

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
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        let args = BackupArgs {
            team: "atm-dev".to_string(),
            json: false,
        };
        let result = backup(args);
        assert!(result.is_ok(), "backup should succeed: {result:?}");

        // Verify backup root exists with a timestamped subdirectory
        let backups_root = temp_dir.path().join(".claude/teams/.backups/atm-dev");
        assert!(backups_root.exists(), "backups root should exist");

        let entries: Vec<_> = fs::read_dir(&backups_root)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "should have exactly one timestamped backup dir"
        );

        let backup_dir = entries[0].path();
        assert!(
            backup_dir.join("config.json").exists(),
            "config.json should be backed up"
        );
        assert!(
            backup_dir.join("inboxes/publisher.json").exists(),
            "inbox should be backed up"
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
    fn test_backup_twice_creates_distinct_dirs() {
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        create_test_team(&temp_dir, "atm-dev");

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        backup(BackupArgs {
            team: "atm-dev".to_string(),
            json: false,
        })
        .unwrap();
        backup(BackupArgs {
            team: "atm-dev".to_string(),
            json: false,
        })
        .unwrap();

        let backups_root = temp_dir.path().join(".claude/teams/.backups/atm-dev");
        let entries: Vec<_> = fs::read_dir(&backups_root)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(
            entries.len(),
            2,
            "two immediate backups should create two distinct snapshot dirs"
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
    fn test_backup_no_team_returns_err() {
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        let args = BackupArgs {
            team: "nonexistent".to_string(),
            json: false,
        };
        let result = backup(args);
        assert!(result.is_err(), "backup of nonexistent team should fail");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("nonexistent"),
            "error should name the team: {msg}"
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
    fn test_restore_from_backup() {
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        let team_dir = create_test_team(&temp_dir, "atm-dev");

        // Create publisher inbox
        let inbox = team_dir.join("inboxes/publisher.json");
        fs::write(&inbox, r#"[{"from":"team-lead","text":"hello","timestamp":"2026-01-01T00:00:00Z","read":false}]"#).unwrap();

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        // Create a backup first
        let backup_args = BackupArgs {
            team: "atm-dev".to_string(),
            json: false,
        };
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
        let publisher = config
            .members
            .iter()
            .find(|m| m.name == "publisher")
            .expect("publisher restored");
        assert_eq!(
            publisher.is_active,
            Some(false),
            "restored members should not remain active without a live session"
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
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        // Create backup
        backup(BackupArgs {
            team: "atm-dev".to_string(),
            json: false,
        })
        .unwrap();

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
        assert!(
            !inbox.exists(),
            "publisher inbox should NOT be restored by dry run"
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
    fn test_restore_skips_team_lead() {
        // team-lead in a backup should never be added again as a duplicate
        // and never replace the current team-lead entry.
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        create_test_team(&temp_dir, "atm-dev");

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        // Create a backup
        backup(BackupArgs {
            team: "atm-dev".to_string(),
            json: false,
        })
        .unwrap();

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
        let lead_count = config
            .members
            .iter()
            .filter(|m| m.name == "team-lead")
            .count();
        assert_eq!(
            lead_count, 1,
            "team-lead should appear exactly once after restore"
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
    fn test_restore_preserves_lead_session_id() {
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        let team_dir = create_test_team(&temp_dir, "atm-dev");

        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        // Backup (captures "old-session-id-1234" from create_test_team)
        backup(BackupArgs {
            team: "atm-dev".to_string(),
            json: false,
        })
        .unwrap();

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
        let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
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
            force: true, // --force bypasses alive-session check
            kill: false,
            session_id: None,
        };

        let result = resume(args);
        assert!(
            result.is_ok(),
            "resume with --force should succeed: {result:?}"
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
        assert!(
            result.is_ok(),
            "cleanup with --force should succeed: {result:?}"
        );

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
    fn test_resume_removes_team_directory_instead_of_mutating_members() {
        // R.1 changed resume semantics: config is no longer mutated in-place.
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
        assert!(
            !team_dir.exists(),
            "team directory should be removed after resume archive handoff"
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
    fn test_resume_explicit_session_id_succeeds_without_config_mutation() {
        // R.1 keeps --session-id for daemon consistency checks, but successful
        // path archives/removes team directory.
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
        assert!(
            result.is_ok(),
            "resume with explicit session_id should succeed: {result:?}"
        );
        assert!(
            !team_dir.exists(),
            "team directory should be removed after resume archive handoff"
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
    fn test_resume_without_explicit_session_id_succeeds_when_daemon_unreachable() {
        // With no daemon session record available, resume should still archive.
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

        let args = ResumeArgs {
            team: "atm-dev".to_string(),
            message: None,
            force: false,
            kill: false,
            session_id: None, // no explicit flag
        };

        let result = resume(args);
        assert!(
            result.is_ok(),
            "resume without explicit session_id should succeed: {result:?}"
        );
        assert!(
            !team_dir.exists(),
            "team directory should be removed after resume archive handoff"
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

    // ---- tasks backup / restore unit tests ----

    /// Create a tasks directory for a team with a couple of sample task files.
    fn create_test_tasks(temp_dir: &TempDir, team_name: &str) -> PathBuf {
        let tasks_dir = temp_dir.path().join(".claude/tasks").join(team_name);
        fs::create_dir_all(&tasks_dir).unwrap();
        fs::write(
            tasks_dir.join("task-1.json"),
            r#"{"id":"task-1","status":"open"}"#,
        )
        .unwrap();
        fs::write(
            tasks_dir.join("task-2.json"),
            r#"{"id":"task-2","status":"done"}"#,
        )
        .unwrap();
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
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        let args = BackupArgs {
            team: "atm-dev".to_string(),
            json: false,
        };
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
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        // Create backup (includes tasks)
        backup(BackupArgs {
            team: "atm-dev".to_string(),
            json: false,
        })
        .unwrap();

        // Remove the tasks directory to simulate loss
        fs::remove_dir_all(&tasks_dir).unwrap();
        assert!(
            !tasks_dir.exists(),
            "tasks dir should be removed before restore"
        );

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
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        // Create backup (includes tasks)
        backup(BackupArgs {
            team: "atm-dev".to_string(),
            json: false,
        })
        .unwrap();

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
        assert!(
            !tasks_dir.exists(),
            "tasks dir should NOT be restored when --skip-tasks is set"
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
    fn test_prune_old_backups_keeps_last_5() {
        // Create 7 backup directories, prune with keep=5, verify only last 5 remain.
        let temp_dir = TempDir::new().unwrap();
        let home_env = set_atm_home(&temp_dir);
        let original = std::env::var("ATM_HOME").ok();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        let backups_dir = temp_dir.path().join(".claude/teams/.backups/my-team");
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
        assert!(
            !backups_dir.join("20260101T000000Z").exists(),
            "oldest backup should be pruned"
        );
        assert!(
            !backups_dir.join("20260102T000000Z").exists(),
            "second oldest should be pruned"
        );
        assert!(
            backups_dir.join("20260103T000000Z").exists(),
            "third oldest should remain"
        );
        assert!(
            backups_dir.join("20260107T000000Z").exists(),
            "newest should remain"
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
    fn test_prune_no_backups_is_noop() {
        // prune_old_backups on a nonexistent directory should return Ok without error.
        let temp_dir = TempDir::new().unwrap();
        // The .backups directory does not exist — this must be a clean no-op.
        let result = prune_old_backups(temp_dir.path(), "ghost-team", 5);
        assert!(
            result.is_ok(),
            "prune on nonexistent dir should be Ok: {result:?}"
        );
    }

    #[test]
    fn test_parse_env_vars_valid_pair() {
        let parsed = parse_env_vars(&["FOO=bar".to_string()]).expect("valid env should parse");
        assert_eq!(parsed.get("FOO").map(String::as_str), Some("bar"));
    }

    #[test]
    fn test_parse_env_vars_value_with_equals() {
        let parsed = parse_env_vars(&["URL=https://x.test?a=1&b=2".to_string()])
            .expect("value with equals should parse");
        assert_eq!(
            parsed.get("URL").map(String::as_str),
            Some("https://x.test?a=1&b=2")
        );
    }

    #[test]
    fn test_parse_env_vars_missing_delimiter_errors() {
        let err = parse_env_vars(&["NO_DELIM".to_string()]);
        assert!(err.is_err());
    }

    #[test]
    fn test_parse_env_vars_empty_key_errors() {
        let err = parse_env_vars(&["=value".to_string()]);
        assert!(err.is_err());
    }

    #[test]
    fn test_resolve_spawn_folder_defaults_to_current_dir() {
        let temp_dir = TempDir::new().unwrap();
        let current = temp_dir.path().join("repo");
        fs::create_dir_all(&current).unwrap();
        let resolved = resolve_spawn_folder(&[], &current).unwrap();
        assert_eq!(resolved, fs::canonicalize(&current).unwrap());
    }

    #[test]
    fn test_resolve_spawn_folder_rejects_mismatched_folder_and_cwd() {
        let temp_dir = TempDir::new().unwrap();
        let folder_a = temp_dir.path().join("a");
        let folder_b = temp_dir.path().join("b");
        fs::create_dir_all(&folder_a).unwrap();
        fs::create_dir_all(&folder_b).unwrap();
        let err = resolve_spawn_folder(&[folder_a, folder_b], temp_dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("resolve to different directories"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_resolve_spawn_folder_accepts_matching_folder_and_cwd() {
        let temp_dir = TempDir::new().unwrap();
        let folder = temp_dir.path().join("same");
        fs::create_dir_all(&folder).unwrap();
        let via_folder = temp_dir.path().join("same");
        let via_cwd = temp_dir.path().join("same/.");
        let resolved = resolve_spawn_folder(&[via_folder, via_cwd], temp_dir.path()).unwrap();
        assert_eq!(resolved, fs::canonicalize(&folder).unwrap());
    }

    #[test]
    fn test_resolve_spawn_folder_rejects_nonexistent_folder() {
        let temp_dir = TempDir::new().unwrap();
        let missing = temp_dir.path().join("does-not-exist");
        let err = resolve_spawn_folder(&[missing], temp_dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("does not exist"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_canonicalize_directory_rejects_file_path() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("not-a-dir.txt");
        fs::write(&file_path, "data").unwrap();
        let err = canonicalize_directory(&file_path, "--folder").unwrap_err();
        assert!(
            err.to_string().contains("is not a directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_runtime_name_all_variants() {
        assert_eq!(runtime_name(&RuntimeKind::Claude), "claude");
        assert_eq!(runtime_name(&RuntimeKind::Codex), "codex");
        assert_eq!(runtime_name(&RuntimeKind::Gemini), "gemini");
        assert_eq!(runtime_name(&RuntimeKind::Opencode), "opencode");
    }
}
