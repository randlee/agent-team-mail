//! GitHub CI monitor command surface (`atm gh ...`).

use agent_team_mail_core::config::{
    Config, ConfigOverrides, resolve_config, resolve_plugin_config_location,
};
use agent_team_mail_core::context::GitProvider;
use agent_team_mail_core::daemon_client::{
    GhMonitorControlRequest, GhMonitorHealth, GhMonitorLifecycleAction, GhMonitorRequest,
    GhMonitorStatus, GhMonitorTargetKind, GhStatusRequest, gh_monitor, gh_monitor_control,
    gh_monitor_health_with_context, gh_status,
};
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::io::ErrorKind;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::NamedTempFile;

use crate::util::settings::get_home_dir;

/// GitHub CI monitor commands.
#[derive(Args, Debug)]
pub struct GhArgs {
    /// Team override (defaults to configured default team)
    #[arg(long)]
    team: Option<String>,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Option<GhCommand>,
}

#[derive(Subcommand, Debug)]
enum GhCommand {
    /// Configure and enable GitHub monitor plugin for this team
    Init(InitArgs),
    /// Start CI monitoring
    Monitor(MonitorArgs),
    /// Query CI monitor status
    Status(StatusArgs),
}

#[derive(Args, Debug)]
struct InitArgs {
    /// Do not write files; print planned config changes only
    #[arg(long)]
    dry_run: bool,

    /// Repository override (`owner/repo` or `repo`)
    #[arg(long, value_name = "OWNER/REPO|REPO")]
    repo: Option<String>,
}

#[derive(Args, Debug)]
struct MonitorArgs {
    #[command(subcommand)]
    target: MonitorTarget,
}

#[derive(Subcommand, Debug)]
enum MonitorTarget {
    /// Monitor a pull request for CI start + tracking
    Pr(MonitorPrArgs),
    /// Monitor a workflow on a specific ref
    Workflow(MonitorWorkflowArgs),
    /// Monitor a specific workflow run id
    Run(MonitorRunArgs),
    /// Start gh_monitor plugin polling lifecycle
    Start(MonitorStartArgs),
    /// Stop gh_monitor plugin polling lifecycle (with in-flight drain window)
    Stop(MonitorStopArgs),
    /// Restart gh_monitor plugin polling lifecycle
    Restart(MonitorRestartArgs),
    /// Query gh_monitor plugin lifecycle/availability health
    Status(MonitorHealthArgs),
}

#[derive(Args, Debug)]
struct MonitorPrArgs {
    /// Pull request number
    number: u64,

    /// Start-timeout in seconds (default 120 = 2m)
    #[arg(long = "start-timeout", default_value_t = 120)]
    start_timeout_secs: u64,
}

#[derive(Args, Debug)]
struct MonitorWorkflowArgs {
    /// Workflow name
    name: String,

    /// Workflow reference (branch, SHA, or PR marker)
    #[arg(long = "ref")]
    reference: String,

    /// Start-timeout in seconds (default 120 = 2m)
    #[arg(long = "start-timeout", default_value_t = 120)]
    start_timeout_secs: u64,
}

#[derive(Args, Debug)]
struct MonitorRunArgs {
    /// Workflow run id
    run_id: u64,
}

#[derive(Args, Debug)]
struct MonitorStartArgs {}

#[derive(Args, Debug)]
struct MonitorStopArgs {
    /// Graceful drain timeout in seconds before force-stop (default 30s)
    #[arg(long = "drain-timeout", default_value_t = 30)]
    drain_timeout_secs: u64,
}

#[derive(Args, Debug)]
struct MonitorRestartArgs {
    /// Graceful drain timeout in seconds before restart (default 30s)
    #[arg(long = "drain-timeout", default_value_t = 30)]
    drain_timeout_secs: u64,
}

#[derive(Args, Debug)]
struct MonitorHealthArgs {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum StatusTargetKind {
    Pr,
    Workflow,
    Run,
}

#[derive(Args, Debug)]
struct StatusArgs {
    /// Monitor target kind (`pr`, `workflow`, `run`)
    target_kind: Option<StatusTargetKind>,

    /// Monitor target value (PR number, workflow name, or run id)
    target: Option<String>,

    /// Optional workflow ref to disambiguate parallel branch monitors
    #[arg(long = "ref")]
    reference: Option<String>,
}

#[derive(Debug, Clone)]
struct GhPluginState {
    configured: bool,
    enabled: bool,
    config_source: Option<String>,
    config_path: Option<String>,
    message: Option<String>,
}

impl GhPluginState {
    fn is_usable(&self) -> bool {
        self.configured && self.enabled && self.message.is_none()
    }
}

#[derive(Debug, Serialize)]
struct GhInitPreview {
    team: String,
    config_path: String,
    dry_run: bool,
    created: bool,
    gh_installed: bool,
    gh_authenticated: bool,
    owner: Option<String>,
    repo: String,
    notify_target: String,
    next_steps: Vec<String>,
}

#[derive(Debug, Serialize)]
struct GhNamespaceStatus {
    team: String,
    configured: bool,
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_path: Option<String>,
    lifecycle_state: String,
    availability_state: String,
    in_flight: u64,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    actions: Vec<&'static str>,
}

pub fn execute(args: GhArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;
    let config = resolve_config(
        &ConfigOverrides {
            team: args.team.clone(),
            ..Default::default()
        },
        &current_dir,
        &home_dir,
    )?;
    let team = args.team.as_deref().unwrap_or(&config.core.default_team);

    let plugin_state = evaluate_plugin_state(&config, team, &current_dir, &home_dir);

    match args.command {
        None => {
            let health = resolve_namespace_health(team, &current_dir, &plugin_state)?;
            print_namespace_status(&health, args.json)
        }
        Some(GhCommand::Init(init_args)) => {
            execute_init(team, &current_dir, &home_dir, init_args, args.json)
        }
        Some(GhCommand::Status(status_args)) => {
            validate_status_args(&status_args)?;
            if status_args.target_kind.is_none() {
                let health = resolve_namespace_health(team, &current_dir, &plugin_state)?;
                return print_namespace_status(&health, args.json);
            }

            enforce_plugin_ready(&plugin_state, args.json)?;
            agent_team_mail_core::daemon_client::ensure_daemon_running()
                .context("failed to auto-start daemon for atm gh status command")?;

            let request = GhStatusRequest {
                team: team.to_string(),
                target_kind: status_kind_to_wire(status_args.target_kind.expect("validated")),
                target: status_args.target.expect("validated"),
                reference: status_args.reference,
                config_cwd: Some(current_dir.to_string_lossy().to_string()),
            };
            let status = gh_status(&request)?.ok_or_else(|| {
                anyhow::anyhow!("daemon is not reachable for atm gh status command")
            })?;
            print_target_status(&status, args.json)
        }
        Some(GhCommand::Monitor(monitor)) => {
            if let MonitorTarget::Status(_status) = &monitor.target {
                let health = resolve_namespace_health(team, &current_dir, &plugin_state)?;
                return print_namespace_status(&health, args.json);
            }

            enforce_plugin_ready(&plugin_state, args.json)?;
            agent_team_mail_core::daemon_client::ensure_daemon_running()
                .context("failed to auto-start daemon for atm gh monitor command")?;

            enum GhOutput {
                MonitorStatus(GhMonitorStatus),
                MonitorHealth(GhMonitorHealth),
            }

            let output = match monitor.target {
                MonitorTarget::Pr(pr) => {
                    let request = GhMonitorRequest {
                        team: team.to_string(),
                        target_kind: GhMonitorTargetKind::Pr,
                        target: pr.number.to_string(),
                        reference: None,
                        start_timeout_secs: Some(pr.start_timeout_secs),
                        config_cwd: Some(current_dir.to_string_lossy().to_string()),
                    };
                    GhOutput::MonitorStatus(gh_monitor(&request)?.ok_or_else(|| {
                        anyhow::anyhow!("daemon is not reachable for atm gh monitor command")
                    })?)
                }
                MonitorTarget::Workflow(workflow) => {
                    let request = GhMonitorRequest {
                        team: team.to_string(),
                        target_kind: GhMonitorTargetKind::Workflow,
                        target: workflow.name,
                        reference: Some(workflow.reference),
                        start_timeout_secs: Some(workflow.start_timeout_secs),
                        config_cwd: Some(current_dir.to_string_lossy().to_string()),
                    };
                    GhOutput::MonitorStatus(gh_monitor(&request)?.ok_or_else(|| {
                        anyhow::anyhow!("daemon is not reachable for atm gh monitor command")
                    })?)
                }
                MonitorTarget::Run(run) => {
                    let request = GhMonitorRequest {
                        team: team.to_string(),
                        target_kind: GhMonitorTargetKind::Run,
                        target: run.run_id.to_string(),
                        reference: None,
                        start_timeout_secs: None,
                        config_cwd: Some(current_dir.to_string_lossy().to_string()),
                    };
                    GhOutput::MonitorStatus(gh_monitor(&request)?.ok_or_else(|| {
                        anyhow::anyhow!("daemon is not reachable for atm gh monitor command")
                    })?)
                }
                MonitorTarget::Start(_start) => {
                    let request = GhMonitorControlRequest {
                        team: team.to_string(),
                        action: GhMonitorLifecycleAction::Start,
                        drain_timeout_secs: None,
                        config_cwd: Some(current_dir.to_string_lossy().to_string()),
                    };
                    GhOutput::MonitorHealth(gh_monitor_control(&request)?.ok_or_else(|| {
                        anyhow::anyhow!("daemon is not reachable for atm gh monitor start command")
                    })?)
                }
                MonitorTarget::Stop(stop) => {
                    let request = GhMonitorControlRequest {
                        team: team.to_string(),
                        action: GhMonitorLifecycleAction::Stop,
                        drain_timeout_secs: Some(stop.drain_timeout_secs),
                        config_cwd: Some(current_dir.to_string_lossy().to_string()),
                    };
                    GhOutput::MonitorHealth(gh_monitor_control(&request)?.ok_or_else(|| {
                        anyhow::anyhow!("daemon is not reachable for atm gh monitor stop command")
                    })?)
                }
                MonitorTarget::Restart(restart) => {
                    let request = GhMonitorControlRequest {
                        team: team.to_string(),
                        action: GhMonitorLifecycleAction::Restart,
                        drain_timeout_secs: Some(restart.drain_timeout_secs),
                        config_cwd: Some(current_dir.to_string_lossy().to_string()),
                    };
                    GhOutput::MonitorHealth(gh_monitor_control(&request)?.ok_or_else(|| {
                        anyhow::anyhow!(
                            "daemon is not reachable for atm gh monitor restart command"
                        )
                    })?)
                }
                MonitorTarget::Status(_status) => unreachable!("handled above"),
            };

            match output {
                GhOutput::MonitorStatus(status) => print_target_status(&status, args.json),
                GhOutput::MonitorHealth(health) => print_namespace_status(&health, args.json),
            }
        }
    }
}

fn validate_status_args(args: &StatusArgs) -> Result<()> {
    match (&args.target_kind, &args.target) {
        (None, None) => {}
        (Some(_), Some(_)) => {}
        (Some(_), None) => bail!("`atm gh status <kind> <target>` requires <target> value"),
        (None, Some(_)) => bail!("`atm gh status` requires both <kind> and <target> together"),
    }

    if args.reference.is_some() && args.target_kind != Some(StatusTargetKind::Workflow) {
        bail!("`--ref` is only valid for `atm gh status workflow <name>`");
    }

    Ok(())
}

fn evaluate_plugin_state(
    config: &Config,
    team: &str,
    current_dir: &Path,
    home_dir: &Path,
) -> GhPluginState {
    let location = resolve_plugin_config_location("gh_monitor", current_dir, home_dir);

    let mut state = GhPluginState {
        configured: false,
        enabled: false,
        config_source: location.as_ref().map(|loc| loc.source.clone()),
        config_path: location
            .as_ref()
            .map(|loc| loc.path.to_string_lossy().to_string()),
        message: None,
    };

    let Some(table) = config.plugin_config("gh_monitor") else {
        state.message = Some("missing [plugins.gh_monitor] configuration".to_string());
        return state;
    };

    state.configured = true;
    state.enabled = table
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if !state.enabled {
        state.message = Some("gh_monitor plugin disabled in configuration".to_string());
        return state;
    }

    let cfg_team = table
        .get("team")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if cfg_team.is_empty() {
        state.message = Some("gh_monitor configuration missing required field: team".to_string());
        return state;
    }
    if cfg_team != team {
        state.message = Some(format!(
            "gh_monitor configured for team '{}' but command is using team '{}'.",
            cfg_team, team
        ));
        return state;
    }

    let cfg_repo = table
        .get("repo")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default();
    if cfg_repo.is_empty() {
        state.message = Some("gh_monitor configuration missing required field: repo".to_string());
        return state;
    }

    state
}

fn resolve_namespace_health(
    team: &str,
    current_dir: &Path,
    plugin_state: &GhPluginState,
) -> Result<GhMonitorHealth> {
    let daemon_error = match agent_team_mail_core::daemon_client::ensure_daemon_running() {
        Ok(()) => {
            if let Some(health) = gh_monitor_health_with_context(
                team,
                Some(current_dir.to_string_lossy().to_string()),
            )? {
                return Ok(health);
            }
            Some("daemon is not reachable".to_string())
        }
        Err(e) => Some(format!("daemon unavailable: {e}")),
    };

    let availability = if plugin_state.message.is_some() {
        "disabled_config_error"
    } else if plugin_state.enabled {
        "disabled_init_error"
    } else {
        "disabled_config_error"
    };

    Ok(GhMonitorHealth {
        team: team.to_string(),
        configured: plugin_state.configured,
        enabled: plugin_state.enabled,
        config_source: plugin_state.config_source.clone(),
        config_path: plugin_state.config_path.clone(),
        lifecycle_state: "unknown".to_string(),
        availability_state: availability.to_string(),
        in_flight: 0,
        updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        message: plugin_state.message.clone().or(daemon_error),
    })
}

fn enforce_plugin_ready(plugin_state: &GhPluginState, json: bool) -> Result<()> {
    if plugin_state.is_usable() {
        return Ok(());
    }

    let reason = plugin_state
        .message
        .as_deref()
        .unwrap_or("gh_monitor plugin is not configured/enabled for this team");

    if json {
        let payload = serde_json::json!({
            "error_code": "PLUGIN_UNAVAILABLE",
            "message": reason,
            "hint": "Run `atm gh init` to configure and enable GitHub monitor for this team.",
        });
        bail!("{}", serde_json::to_string_pretty(&payload)?);
    }

    bail!("{reason}\nRun `atm gh init` to configure and enable GitHub monitor for this team.")
}

fn status_kind_to_wire(kind: StatusTargetKind) -> GhMonitorTargetKind {
    match kind {
        StatusTargetKind::Pr => GhMonitorTargetKind::Pr,
        StatusTargetKind::Workflow => GhMonitorTargetKind::Workflow,
        StatusTargetKind::Run => GhMonitorTargetKind::Run,
    }
}

fn print_target_status(status: &GhMonitorStatus, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(status)?);
        return Ok(());
    }

    let target_label = match status.target_kind {
        GhMonitorTargetKind::Pr => format!("pr:{}", status.target),
        GhMonitorTargetKind::Workflow => {
            if let Some(reference) = status.reference.as_deref() {
                format!("workflow:{} ref:{}", status.target, reference)
            } else {
                format!("workflow:{}", status.target)
            }
        }
        GhMonitorTargetKind::Run => format!("run:{}", status.target),
    };

    println!("Team:        {}", status.team);
    println!("Target:      {target_label}");
    println!("State:       {}", status.state);
    println!("Configured:  {}", yes_no(status.configured));
    println!("Enabled:     {}", yes_no(status.enabled));
    if let Some(source) = status.config_source.as_deref() {
        println!("Cfg Source:  {source}");
    }
    if let Some(path) = status.config_path.as_deref() {
        println!("Cfg Path:    {path}");
    }
    if let Some(run_id) = status.run_id {
        println!("Run ID:      {run_id}");
    }
    if let Some(message) = status.message.as_deref() {
        println!("Message:     {message}");
    }
    println!("Updated At:  {}", status.updated_at);
    Ok(())
}

fn print_namespace_status(health: &GhMonitorHealth, json: bool) -> Result<()> {
    let status = namespace_status_view(health);
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }

    println!("GitHub Monitor Namespace: atm gh");
    println!("Team:              {}", status.team);
    println!("Configured:        {}", yes_no(status.configured));
    println!("Enabled:           {}", yes_no(status.enabled));
    if let Some(source) = status.config_source.as_deref() {
        println!("Config Source:     {source}");
    }
    if let Some(path) = status.config_path.as_deref() {
        println!("Config Path:       {path}");
    }
    println!("Lifecycle:         {}", status.lifecycle_state);
    println!("Availability:      {}", status.availability_state);
    println!("In Flight:         {}", status.in_flight);
    if let Some(message) = status.message.as_deref() {
        println!("Message:           {message}");
    }
    println!("Updated At:        {}", status.updated_at);
    println!();
    println!("Available actions:");
    for action in status.actions {
        println!("  - {action}");
    }

    Ok(())
}

fn namespace_status_view(health: &GhMonitorHealth) -> GhNamespaceStatus {
    GhNamespaceStatus {
        team: health.team.clone(),
        configured: health.configured,
        enabled: health.enabled,
        config_source: health.config_source.clone(),
        config_path: health.config_path.clone(),
        lifecycle_state: health.lifecycle_state.clone(),
        availability_state: health.availability_state.clone(),
        in_flight: health.in_flight,
        updated_at: health.updated_at.clone(),
        message: health.message.clone(),
        actions: namespace_actions(health.enabled && health.configured),
    }
}

fn namespace_actions(enabled: bool) -> Vec<&'static str> {
    if enabled {
        vec![
            "atm gh",
            "atm gh status",
            "atm gh status <pr|workflow|run> <target>",
            "atm gh monitor pr <number>",
            "atm gh monitor workflow <name> --ref <ref>",
            "atm gh monitor run <run-id>",
            "atm gh monitor start|stop|restart|status",
            "atm gh init",
        ]
    } else {
        vec!["atm gh", "atm gh init"]
    }
}

fn yes_no(v: bool) -> &'static str {
    if v { "yes" } else { "no" }
}

fn execute_init(
    team: &str,
    current_dir: &Path,
    home_dir: &Path,
    args: InitArgs,
    json: bool,
) -> Result<()> {
    validate_gh_cli_prerequisites()?;

    let detected = detect_github_remote(current_dir);
    let (owner, repo) = resolve_repo_coordinates(args.repo.as_deref(), detected.as_ref())?;
    let config_path = choose_init_config_path(current_dir, home_dir);

    let mut document = if config_path.exists() {
        let contents = std::fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read {}", config_path.display()))?;
        toml::from_str::<toml::Value>(&contents)
            .with_context(|| format!("failed to parse {}", config_path.display()))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };

    let root = document
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("top-level config must be a TOML table"))?;
    let plugins = root
        .entry("plugins")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("`plugins` must be a TOML table"))?;
    let gh = plugins
        .entry("gh_monitor")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("`plugins.gh_monitor` must be a TOML table"))?;

    gh.insert("enabled".to_string(), toml::Value::Boolean(true));
    gh.insert(
        "provider".to_string(),
        toml::Value::String("github".to_string()),
    );
    gh.insert("team".to_string(), toml::Value::String(team.to_string()));
    gh.insert(
        "agent".to_string(),
        toml::Value::String("gh-monitor".to_string()),
    );
    gh.insert("repo".to_string(), toml::Value::String(repo.clone()));
    if let Some(owner) = owner.as_ref() {
        gh.insert("owner".to_string(), toml::Value::String(owner.clone()));
    }
    gh.entry("poll_interval_secs".to_string())
        .or_insert_with(|| toml::Value::Integer(60));
    gh.entry("notify_target".to_string())
        .or_insert_with(|| toml::Value::String("team-lead".to_string()));
    let notify_target = gh
        .get("notify_target")
        .and_then(toml::Value::as_str)
        .unwrap_or("team-lead")
        .to_string();

    let preview = GhInitPreview {
        team: team.to_string(),
        config_path: config_path.display().to_string(),
        dry_run: args.dry_run,
        created: !config_path.exists(),
        gh_installed: true,
        gh_authenticated: true,
        owner,
        repo,
        notify_target,
        next_steps: vec![
            "atm gh".to_string(),
            "atm gh status".to_string(),
            "atm gh monitor pr <number>".to_string(),
        ],
    };

    if !args.dry_run {
        let serialized = format!("{}\n", toml::to_string_pretty(&document)?);
        write_text_atomic(&config_path, &serialized)?;
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&preview)?);
    } else {
        if args.dry_run {
            println!("Dry run - atm gh init");
        } else {
            println!("atm gh init complete");
        }
        println!("Team:         {}", preview.team);
        println!("Config file:  {}", preview.config_path);
        println!(
            "Result:       {}",
            if preview.created {
                if args.dry_run {
                    "would create"
                } else {
                    "created/updated"
                }
            } else if args.dry_run {
                "would update"
            } else {
                "updated"
            }
        );
        if let Some(owner) = preview.owner.as_deref() {
            println!("Repository:   {owner}/{}", preview.repo);
        } else {
            println!("Repository:   {}", preview.repo);
        }
        println!("Notify:       {}", preview.notify_target);
        println!("Enabled:      yes");
        println!();
        println!("Next steps:");
        for step in preview.next_steps {
            println!("  - {step}");
        }
    }

    Ok(())
}

fn validate_gh_cli_prerequisites() -> Result<()> {
    let version = Command::new("gh")
        .arg("--version")
        .output()
        .context("failed to invoke `gh --version`")?;
    if !version.status.success() {
        bail!(
            "GitHub CLI (`gh`) not found or not executable. Install from https://cli.github.com/"
        );
    }

    let auth = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .context("failed to invoke `gh auth status`")?;
    if !auth.status.success() {
        let stderr = String::from_utf8_lossy(&auth.stderr);
        bail!(
            "GitHub CLI is not authenticated. Run `gh auth login` first.\n{}",
            stderr.trim()
        );
    }

    Ok(())
}

fn detect_github_remote(current_dir: &Path) -> Option<(String, String)> {
    let output = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(current_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if remote.is_empty() {
        return None;
    }

    match GitProvider::detect_from_url(&remote) {
        GitProvider::GitHub { owner, repo } => Some((owner, repo)),
        _ => None,
    }
}

fn resolve_repo_coordinates(
    repo_arg: Option<&str>,
    detected: Option<&(String, String)>,
) -> Result<(Option<String>, String)> {
    if let Some(raw) = repo_arg {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            bail!("--repo cannot be empty");
        }

        if let Some((owner, repo)) = trimmed.split_once('/') {
            let owner = owner.trim();
            let repo = repo.trim();
            if owner.is_empty() || repo.is_empty() {
                bail!("--repo must be `owner/repo` or `repo`");
            }
            return Ok((Some(owner.to_string()), repo.to_string()));
        }

        let owner = detected.map(|(owner, _)| owner.clone());
        return Ok((owner, trimmed.to_string()));
    }

    if let Some((owner, repo)) = detected {
        return Ok((Some(owner.clone()), repo.clone()));
    }

    bail!(
        "Could not determine GitHub repository from git remote. Use `atm gh init --repo <owner/repo>`"
    )
}

fn choose_init_config_path(current_dir: &Path, home_dir: &Path) -> PathBuf {
    if let Some(location) = resolve_plugin_config_location("gh_monitor", current_dir, home_dir) {
        return location.path;
    }

    if let Some(repo_root) = find_git_root(current_dir) {
        return repo_root.join(".atm.toml");
    }

    let global = home_dir.join(".config/atm/config.toml");
    if global.exists() {
        return global;
    }

    current_dir.join(".atm.toml")
}

fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

fn write_text_atomic(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let tmp_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = NamedTempFile::new_in(tmp_dir)
        .with_context(|| format!("failed to create temp file in {}", tmp_dir.display()))?;
    tmp.write_all(content.as_bytes())
        .with_context(|| format!("failed to write {}", tmp.path().display()))?;
    tmp.flush()
        .with_context(|| format!("failed to flush {}", tmp.path().display()))?;

    match tmp.persist(path) {
        Ok(_) => {}
        Err(err) if err.error.kind() == ErrorKind::AlreadyExists => {
            std::fs::remove_file(path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
            err.file
                .persist(path)
                .map_err(|persist_err| persist_err.error)
                .with_context(|| format!("failed to replace {}", path.display()))?;
        }
        Err(err) => {
            return Err(err.error).with_context(|| format!("failed to replace {}", path.display()));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_status_args_accepts_no_target_form() {
        let args = StatusArgs {
            target_kind: None,
            target: None,
            reference: None,
        };
        assert!(validate_status_args(&args).is_ok());
    }

    #[test]
    fn validate_status_args_rejects_partial_target_form() {
        let args = StatusArgs {
            target_kind: Some(StatusTargetKind::Pr),
            target: None,
            reference: None,
        };
        assert!(validate_status_args(&args).is_err());
    }

    #[test]
    fn resolve_repo_coordinates_accepts_owner_repo() {
        let coords = resolve_repo_coordinates(Some("acme/repo"), None).unwrap();
        assert_eq!(coords.0.as_deref(), Some("acme"));
        assert_eq!(coords.1, "repo");
    }

    #[test]
    fn resolve_repo_coordinates_uses_detected_owner_for_repo_only() {
        let detected = ("acme".to_string(), "agent-team-mail".to_string());
        let coords = resolve_repo_coordinates(Some("agent-team-mail"), Some(&detected)).unwrap();
        assert_eq!(coords.0.as_deref(), Some("acme"));
        assert_eq!(coords.1, "agent-team-mail");
    }
}
