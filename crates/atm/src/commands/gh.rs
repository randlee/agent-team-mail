//! GitHub CI monitor command surface (`atm gh ...`).

use agent_team_mail_core::config::{
    Config, ConfigOverrides, resolve_config, resolve_plugin_config_location,
};
use agent_team_mail_core::consts::GH_MONITOR_DEFAULT_DRAIN_TIMEOUT_SECS;
use agent_team_mail_core::context::GitProvider;
use agent_team_mail_core::daemon_client::{
    GhMonitorControlRequest, GhMonitorHealth, GhMonitorLifecycleAction, GhMonitorRequest,
    GhMonitorStatus, GhMonitorTargetKind, GhStatusRequest, gh_monitor, gh_monitor_control,
    gh_monitor_health_with_context, gh_status,
};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::gh_monitor_observability::{
    GhCliObserverContext, run_attributed_gh_command,
};
use agent_team_mail_core::io::inbox::inbox_append;
use agent_team_mail_core::schema::InboxMessage;
use agent_team_mail_core::team_config_store::TeamConfigStore;
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use minijinja::Environment;
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;
use tempfile::NamedTempFile;

use crate::util::settings::{get_home_dir, teams_root_dir_for};

/// GitHub CI monitor commands.
#[derive(Args, Debug)]
pub struct GhArgs {
    /// Team override (defaults to configured default team)
    #[arg(long)]
    team: Option<String>,

    /// Repository override (`owner/repo` or GitHub URL)
    #[arg(long, global = true, value_name = "OWNER/REPO|URL")]
    repo: Option<String>,

    /// Additional ATM recipients for copied monitor notifications
    #[arg(long = "cc", global = true, value_name = "AGENT[@TEAM]")]
    cc: Vec<String>,

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
    /// One-shot PR query/report commands (no daemon monitor lifecycle control)
    Pr(PrArgs),
    /// Query CI monitor status
    Status(StatusArgs),
}

#[derive(Args, Debug)]
struct InitArgs {
    /// Do not write files; print planned config changes only
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args, Debug)]
struct MonitorArgs {
    #[command(subcommand)]
    target: MonitorTarget,
}

#[derive(Args, Debug)]
struct PrArgs {
    #[command(subcommand)]
    target: PrTarget,
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

#[derive(Subcommand, Debug)]
enum PrTarget {
    /// List open PRs with CI/merge/review rollups (one-shot; no daemon required)
    List(PrListArgs),
    /// Show detailed check/review/merge report for a single PR (one-shot; no daemon required)
    Report(PrReportArgs),
    /// Scaffold a starter template for `atm gh pr report --template`
    InitReport(PrInitReportArgs),
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
    /// Graceful drain timeout in seconds before force-stop (default 10s)
    #[arg(
        long = "drain-timeout",
        default_value_t = GH_MONITOR_DEFAULT_DRAIN_TIMEOUT_SECS
    )]
    drain_timeout_secs: u64,

    /// Hidden operator confirmation for cross-team shutdown
    #[arg(long, hide = true)]
    user_authorized: bool,

    /// Human reason recorded for hidden cross-team shutdown
    #[arg(long, hide = true, requires = "user_authorized")]
    reason: Option<String>,
}

#[derive(Args, Debug)]
struct MonitorRestartArgs {
    /// Graceful drain timeout in seconds before restart (default 10s)
    #[arg(
        long = "drain-timeout",
        default_value_t = GH_MONITOR_DEFAULT_DRAIN_TIMEOUT_SECS
    )]
    drain_timeout_secs: u64,

    /// Hidden operator confirmation for cross-team restart
    #[arg(long, hide = true)]
    user_authorized: bool,

    /// Human reason recorded for hidden cross-team restart
    #[arg(long, hide = true, requires = "user_authorized")]
    reason: Option<String>,
}

#[derive(Args, Debug)]
struct MonitorHealthArgs {}

#[derive(Args, Debug)]
struct PrListArgs {
    /// Maximum number of open PRs to display (default 20)
    #[arg(long, default_value_t = 20)]
    limit: u32,
}

#[derive(Args, Debug)]
struct PrReportArgs {
    /// Pull request number to report
    pr_number: u64,

    /// Render output using a user template file (Jinja-compatible syntax)
    #[arg(long, value_name = "PATH")]
    template: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct PrInitReportArgs {
    /// Output path for starter template (default: ./gh-monitor-report-template.j2)
    #[arg(long, value_name = "PATH")]
    output: Option<PathBuf>,
}

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
    repo: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_state_updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_limit_per_hour: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_used_in_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate_limit_remaining: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate_limit_limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    poll_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_runtime_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_binary_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_atm_home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_poll_interval_secs: Option<u64>,
    actions: Vec<&'static str>,
}

#[derive(Debug, Deserialize)]
struct GhPrListRow {
    number: u64,
    title: String,
    url: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "reviewDecision", default)]
    review_decision: Option<String>,
    #[serde(rename = "mergeStateStatus", default)]
    merge_state_status: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct GhPrReportRow {
    number: u64,
    title: String,
    url: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "reviewDecision", default)]
    review_decision: Option<String>,
    #[serde(rename = "mergeStateStatus", default)]
    merge_state_status: Option<String>,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Vec<serde_json::Value>,
    #[serde(default)]
    reviews: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
struct GhPrListSummary {
    team: String,
    repo: String,
    generated_at: String,
    total_open_prs: usize,
    items: Vec<GhMonitorListItem>,
}

#[derive(Debug, Clone, Serialize)]
struct GhMonitorListItem {
    number: u64,
    title: String,
    url: String,
    draft: bool,
    ci: GhCiRollup,
    merge: String,
    review: String,
}

#[derive(Debug, Clone, Serialize)]
struct GhCiRollup {
    state: String,
    total: u64,
    pass: u64,
    fail: u64,
    pending: u64,
    skip: u64,
    neutral: u64,
}

#[derive(Debug, Clone, Serialize)]
struct GhPrReportSummary {
    schema_version: String,
    team: String,
    repo: String,
    generated_at: String,
    pr: GhMonitorReportPr,
}

#[derive(Debug, Clone, Serialize)]
struct GhMonitorReportPr {
    number: u64,
    title: String,
    url: String,
    draft: bool,
    ci: GhCiRollup,
    review_decision: String,
    merge: GhMergeReport,
    checks: Vec<GhMonitorCheckReport>,
    reviews: Vec<GhMonitorReviewReport>,
}

#[derive(Debug, Clone, Serialize)]
struct GhMergeReport {
    mergeable: String,
    merge_state_status: String,
    status: String,
    blocking_reasons: Vec<String>,
    advisory_reasons: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GhPrMergeProbe {
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(rename = "mergeStateStatus", default)]
    merge_state_status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct GhMonitorCheckReport {
    name: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    conclusion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct GhMonitorReviewReport {
    reviewer: String,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    submitted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct GhPrInitReportSummary {
    output_path: String,
    created: bool,
    schema_version: String,
}

const GH_MONITOR_REPORT_SCHEMA_VERSION: &str = "1.0.0";
const GH_MONITOR_DEFAULT_TEMPLATE_FILENAME: &str = "gh-monitor-report-template.j2";
const GH_MONITOR_MERGE_RETRY_ATTEMPTS: u8 = 3;
const GH_MONITOR_MERGE_RETRY_DELAY_MS: u64 = 250;

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
    let namespace_repo_scope = resolve_daemon_repo_scope(args.repo.as_deref(), &current_dir).ok();

    match args.command {
        None => {
            let health = resolve_namespace_health(
                team,
                &current_dir,
                namespace_repo_scope.as_deref(),
                &plugin_state,
            )?;
            print_namespace_status(&health, args.json)
        }
        Some(GhCommand::Init(init_args)) => execute_init(
            team,
            &current_dir,
            &home_dir,
            init_args,
            args.repo.as_deref(),
            args.json,
        ),
        Some(GhCommand::Status(status_args)) => {
            validate_status_args(&status_args)?;
            if status_args.target_kind.is_none() {
                let health = resolve_namespace_health(
                    team,
                    &current_dir,
                    namespace_repo_scope.as_deref(),
                    &plugin_state,
                )?;
                return print_namespace_status(&health, args.json);
            }

            enforce_plugin_ready(&plugin_state, args.json)?;
            agent_team_mail_core::daemon_client::ensure_daemon_running()
                .context("failed to auto-start daemon for atm gh status command")?;

            let request = GhStatusRequest {
                team: team.to_string(),
                target_kind: status_kind_to_wire(status_args.target_kind.expect("validated")),
                target: status_args.target.expect("validated"),
                repo: Some(resolve_daemon_repo_scope(
                    args.repo.as_deref(),
                    &current_dir,
                )?),
                reference: status_args.reference,
                config_cwd: Some(current_dir.to_string_lossy().to_string()),
            };
            let status = gh_status(&request)?.ok_or_else(|| {
                anyhow::anyhow!("daemon is not reachable for atm gh status command")
            })?;
            print_target_status(&status, args.json)
        }
        Some(GhCommand::Pr(pr)) => {
            enforce_plugin_ready(&plugin_state, args.json)?;
            match pr.target {
                PrTarget::List(list_args) => execute_pr_list(
                    team,
                    &config,
                    &current_dir,
                    &home_dir,
                    list_args.limit,
                    args.json,
                ),
                PrTarget::Report(report_args) => execute_pr_report(
                    team,
                    &config,
                    &current_dir,
                    &home_dir,
                    report_args.pr_number,
                    report_args.template.as_deref(),
                    args.json,
                ),
                PrTarget::InitReport(init_report_args) => execute_pr_init_report(
                    &current_dir,
                    init_report_args.output.as_deref(),
                    args.json,
                ),
            }
        }
        Some(GhCommand::Monitor(monitor)) => {
            if let MonitorTarget::Status(_status) = &monitor.target {
                let health = resolve_namespace_health(
                    team,
                    &current_dir,
                    namespace_repo_scope.as_deref(),
                    &plugin_state,
                )?;
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
                        repo: Some(resolve_daemon_repo_scope(
                            args.repo.as_deref(),
                            &current_dir,
                        )?),
                        reference: None,
                        start_timeout_secs: Some(pr.start_timeout_secs),
                        config_cwd: Some(current_dir.to_string_lossy().to_string()),
                        caller_agent: Some(resolve_monitor_caller_identity(&config)),
                        cc: args.cc.clone(),
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
                        repo: Some(resolve_daemon_repo_scope(
                            args.repo.as_deref(),
                            &current_dir,
                        )?),
                        reference: Some(workflow.reference),
                        start_timeout_secs: Some(workflow.start_timeout_secs),
                        config_cwd: Some(current_dir.to_string_lossy().to_string()),
                        caller_agent: Some(resolve_monitor_caller_identity(&config)),
                        cc: args.cc.clone(),
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
                        repo: Some(resolve_daemon_repo_scope(
                            args.repo.as_deref(),
                            &current_dir,
                        )?),
                        reference: None,
                        start_timeout_secs: None,
                        config_cwd: Some(current_dir.to_string_lossy().to_string()),
                        caller_agent: Some(resolve_monitor_caller_identity(&config)),
                        cc: args.cc.clone(),
                    };
                    GhOutput::MonitorStatus(gh_monitor(&request)?.ok_or_else(|| {
                        anyhow::anyhow!("daemon is not reachable for atm gh monitor command")
                    })?)
                }
                MonitorTarget::Start(_start) => {
                    let request = GhMonitorControlRequest {
                        team: team.to_string(),
                        action: GhMonitorLifecycleAction::Start,
                        repo: Some(resolve_daemon_repo_scope(
                            args.repo.as_deref(),
                            &current_dir,
                        )?),
                        drain_timeout_secs: None,
                        config_cwd: Some(current_dir.to_string_lossy().to_string()),
                        actor: Some(resolve_monitor_caller_identity(&config)),
                        actor_team: Some(config.core.default_team.clone()),
                        user_authorized: false,
                        operator_reason: None,
                    };
                    GhOutput::MonitorHealth(gh_monitor_control(&request)?.ok_or_else(|| {
                        anyhow::anyhow!("daemon is not reachable for atm gh monitor start command")
                    })?)
                }
                MonitorTarget::Stop(stop) => {
                    let actor = resolve_monitor_caller_identity(&config);
                    let actor_team = config.core.default_team.clone();
                    validate_cross_team_monitor_control(
                        &actor_team,
                        team,
                        stop.user_authorized,
                        stop.reason.as_deref(),
                    )?;
                    let request = GhMonitorControlRequest {
                        team: team.to_string(),
                        action: GhMonitorLifecycleAction::Stop,
                        repo: Some(resolve_daemon_repo_scope(
                            args.repo.as_deref(),
                            &current_dir,
                        )?),
                        drain_timeout_secs: Some(stop.drain_timeout_secs),
                        config_cwd: Some(current_dir.to_string_lossy().to_string()),
                        actor: Some(actor.clone()),
                        actor_team: Some(actor_team.clone()),
                        user_authorized: stop.user_authorized,
                        operator_reason: stop.reason.clone(),
                    };
                    let health = gh_monitor_control(&request)?.ok_or_else(|| {
                        anyhow::anyhow!("daemon is not reachable for atm gh monitor stop command")
                    })?;
                    audit_monitor_control_action(
                        "stop",
                        &actor,
                        &actor_team,
                        team,
                        request.repo.as_deref(),
                        stop.reason.as_deref(),
                        stop.user_authorized,
                    );
                    if stop.user_authorized && actor_team != team {
                        notify_team_lead_of_monitor_control(
                            &home_dir,
                            &actor,
                            &actor_team,
                            team,
                            "disabled",
                            stop.reason
                                .as_deref()
                                .unwrap_or("operator-authorized cross-team stop"),
                        )?;
                    }
                    GhOutput::MonitorHealth(health)
                }
                MonitorTarget::Restart(restart) => {
                    let actor = resolve_monitor_caller_identity(&config);
                    let actor_team = config.core.default_team.clone();
                    validate_cross_team_monitor_control(
                        &actor_team,
                        team,
                        restart.user_authorized,
                        restart.reason.as_deref(),
                    )?;
                    let request = GhMonitorControlRequest {
                        team: team.to_string(),
                        action: GhMonitorLifecycleAction::Restart,
                        repo: Some(resolve_daemon_repo_scope(
                            args.repo.as_deref(),
                            &current_dir,
                        )?),
                        drain_timeout_secs: Some(restart.drain_timeout_secs),
                        config_cwd: Some(current_dir.to_string_lossy().to_string()),
                        actor: Some(actor.clone()),
                        actor_team: Some(actor_team.clone()),
                        user_authorized: restart.user_authorized,
                        operator_reason: restart.reason.clone(),
                    };
                    let health = gh_monitor_control(&request)?.ok_or_else(|| {
                        anyhow::anyhow!(
                            "daemon is not reachable for atm gh monitor restart command"
                        )
                    })?;
                    audit_monitor_control_action(
                        "restart",
                        &actor,
                        &actor_team,
                        team,
                        request.repo.as_deref(),
                        restart.reason.as_deref(),
                        restart.user_authorized,
                    );
                    if restart.user_authorized && actor_team != team {
                        notify_team_lead_of_monitor_control(
                            &home_dir,
                            &actor,
                            &actor_team,
                            team,
                            "restarted",
                            restart
                                .reason
                                .as_deref()
                                .unwrap_or("operator-authorized cross-team restart"),
                        )?;
                    }
                    GhOutput::MonitorHealth(health)
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

fn execute_pr_list(
    team: &str,
    config: &Config,
    current_dir: &Path,
    home_dir: &Path,
    limit: u32,
    json: bool,
) -> Result<()> {
    let repo = resolve_monitor_repo_scope(config, current_dir, home_dir, team)?;
    let request_limit = limit.clamp(1, 200);
    let gh_json_fields =
        "number,title,url,isDraft,reviewDecision,mergeStateStatus,statusCheckRollup";
    let limit_arg = request_limit.to_string();
    let args = vec![
        "-R".to_string(),
        repo.clone(),
        "pr".to_string(),
        "list".to_string(),
        "--state".to_string(),
        "open".to_string(),
        "--limit".to_string(),
        limit_arg,
        "--json".to_string(),
        gh_json_fields.to_string(),
    ];
    let output = run_repo_scoped_gh_command(team, home_dir, &repo, "gh_pr_list", &args, None)
        .with_context(|| format!("failed to invoke `gh pr list` for repository {repo}"))?;

    let rows: Vec<GhPrListRow> = serde_json::from_str(&output)
        .with_context(|| "failed to parse `gh pr list` JSON output")?;

    let mut items: Vec<GhMonitorListItem> = rows
        .iter()
        .map(|row| GhMonitorListItem {
            number: row.number,
            title: row.title.clone(),
            url: row.url.clone(),
            draft: row.is_draft,
            ci: summarize_ci_rollup(&row.status_check_rollup),
            merge: normalize_merge_status(row.merge_state_status.as_deref()),
            review: normalize_review_status(row.review_decision.as_deref()),
        })
        .collect();
    items.sort_by(|a, b| a.number.cmp(&b.number));

    let summary = GhPrListSummary {
        team: team.to_string(),
        repo,
        generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        total_open_prs: items.len(),
        items,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    print_pr_list_summary(&summary);
    Ok(())
}

fn execute_pr_report(
    team: &str,
    config: &Config,
    current_dir: &Path,
    home_dir: &Path,
    pr_number: u64,
    template_path: Option<&Path>,
    json: bool,
) -> Result<()> {
    if json && template_path.is_some() {
        bail!("`--template` cannot be combined with `--json`");
    }

    let repo = resolve_monitor_repo_scope(config, current_dir, home_dir, team)?;
    let gh_json_fields = "number,title,url,isDraft,reviewDecision,mergeStateStatus,mergeable,statusCheckRollup,reviews";
    let pr_number_arg = pr_number.to_string();
    let args = vec![
        "-R".to_string(),
        repo.clone(),
        "pr".to_string(),
        "view".to_string(),
        pr_number_arg.clone(),
        "--json".to_string(),
        gh_json_fields.to_string(),
    ];
    let output = run_repo_scoped_gh_command(
        team,
        home_dir,
        &repo,
        "gh_pr_view",
        &args,
        Some(pr_number_arg.as_str()),
    )
    .with_context(|| format!("failed to invoke `gh pr view` for repository {repo}"))?;

    let row: GhPrReportRow = serde_json::from_str(&output)
        .with_context(|| "failed to parse `gh pr view` JSON output")?;
    let checks = extract_check_reports(&row.status_check_rollup);
    let reviews = extract_review_reports(&row.reviews);
    let ci = summarize_ci_rollup(&row.status_check_rollup);
    let review_decision =
        normalize_report_review_decision(row.review_decision.as_deref(), &reviews);
    let (mergeable, merge_state_status) = resolve_merge_snapshot_with_retry(
        team,
        home_dir,
        &repo,
        pr_number,
        row.mergeable.clone(),
        row.merge_state_status.clone(),
    );
    let merge = build_merge_report(
        mergeable.as_deref(),
        merge_state_status.as_deref(),
        row.is_draft,
        &ci,
        &review_decision,
    );

    let report = GhPrReportSummary {
        schema_version: GH_MONITOR_REPORT_SCHEMA_VERSION.to_string(),
        team: team.to_string(),
        repo,
        generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        pr: GhMonitorReportPr {
            number: row.number,
            title: row.title,
            url: row.url,
            draft: row.is_draft,
            ci,
            review_decision,
            merge,
            checks,
            reviews,
        },
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if let Some(path) = template_path {
        let rendered = render_pr_report_template(path, &report)?;
        print!("{rendered}");
        if !rendered.ends_with('\n') {
            println!();
        }
        return Ok(());
    }

    print_pr_report_summary(&report);
    Ok(())
}

fn resolve_merge_snapshot_with_retry(
    team: &str,
    home_dir: &Path,
    repo: &str,
    pr_number: u64,
    initial_mergeable: Option<String>,
    initial_merge_state_status: Option<String>,
) -> (Option<String>, Option<String>) {
    let mut mergeable = initial_mergeable;
    let mut merge_state_status = initial_merge_state_status;

    if !should_retry_mergeability(mergeable.as_deref(), merge_state_status.as_deref()) {
        return (mergeable, merge_state_status);
    }

    for _ in 0..GH_MONITOR_MERGE_RETRY_ATTEMPTS {
        thread::sleep(Duration::from_millis(GH_MONITOR_MERGE_RETRY_DELAY_MS));
        let Ok(snapshot) = query_merge_snapshot(team, home_dir, repo, pr_number) else {
            break;
        };

        mergeable = snapshot.mergeable;
        merge_state_status = snapshot.merge_state_status;

        if !should_retry_mergeability(mergeable.as_deref(), merge_state_status.as_deref()) {
            break;
        }
    }

    (mergeable, merge_state_status)
}

fn should_retry_mergeability(mergeable: Option<&str>, merge_state_status: Option<&str>) -> bool {
    let mergeable_normalized = normalize_mergeable(mergeable);
    let merge_state_status_normalized = normalize_merge_status(merge_state_status);
    mergeable_normalized == "unknown"
        || matches!(
            merge_state_status_normalized.as_str(),
            "unknown" | "pending"
        )
}

fn query_merge_snapshot(
    team: &str,
    home_dir: &Path,
    repo: &str,
    pr_number: u64,
) -> Result<GhPrMergeProbe> {
    let pr_number_arg = pr_number.to_string();
    let args = vec![
        "-R".to_string(),
        repo.to_string(),
        "pr".to_string(),
        "view".to_string(),
        pr_number_arg.clone(),
        "--json".to_string(),
        "mergeStateStatus,mergeable".to_string(),
    ];
    let output = run_repo_scoped_gh_command(
        team,
        home_dir,
        repo,
        "gh_pr_view_merge_probe",
        &args,
        Some(pr_number_arg.as_str()),
    )
    .with_context(|| format!("failed to invoke `gh pr view` merge probe for {repo}"))?;

    serde_json::from_str(&output).with_context(|| "failed to parse merge probe JSON output")
}

fn run_repo_scoped_gh_command(
    team: &str,
    home_dir: &Path,
    repo: &str,
    action: &str,
    args: &[String],
    reference: Option<&str>,
) -> Result<String> {
    let observer_ctx = GhCliObserverContext {
        home: home_dir.to_path_buf(),
        team: team.to_string(),
        repo: repo.to_string(),
        runtime: "atm".to_string(),
    };
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_attributed_gh_command(&observer_ctx, action, &arg_refs, None, reference)
}

fn execute_pr_init_report(
    current_dir: &Path,
    output_override: Option<&Path>,
    json: bool,
) -> Result<()> {
    let output_path = match output_override {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => current_dir.join(path),
        None => current_dir.join(GH_MONITOR_DEFAULT_TEMPLATE_FILENAME),
    };

    if output_path.exists() {
        bail!(
            "report template already exists at {} (choose another path or remove existing file)",
            output_path.display()
        );
    }

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create parent directories for {}",
                output_path.display()
            )
        })?;
    }

    std::fs::write(&output_path, default_monitor_report_template()).with_context(|| {
        format!(
            "failed to write starter report template to {}",
            output_path.display()
        )
    })?;

    let summary = GhPrInitReportSummary {
        output_path: output_path.display().to_string(),
        created: true,
        schema_version: GH_MONITOR_REPORT_SCHEMA_VERSION.to_string(),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    println!("atm gh pr init-report complete");
    println!("Template:          {}", summary.output_path);
    println!("Schema Version:    {}", summary.schema_version);
    println!();
    println!("Use with:");
    println!(
        "  atm gh pr report <pr-number> --template {}",
        summary.output_path
    );
    Ok(())
}

fn render_pr_report_template(template_path: &Path, report: &GhPrReportSummary) -> Result<String> {
    let template = std::fs::read_to_string(template_path)
        .with_context(|| format!("failed to read template file {}", template_path.display()))?;
    let env = Environment::new();
    env.render_str(&template, report).map_err(|err| {
        anyhow::anyhow!(
            "failed to render template {}: {}",
            template_path.display(),
            err
        )
    })
}

fn default_monitor_report_template() -> &'static str {
    r#"GitHub PR Report (schema {{ schema_version }})
Team: {{ team }}
Repository: {{ repo }}
Generated: {{ generated_at }}
PR #{{ pr.number }}: {{ pr.title }}
URL: {{ pr.url }}
Draft: {{ "yes" if pr.draft else "no" }}
CI: {{ pr.ci.state }} (pass={{ pr.ci.pass }}{% if pr.ci.fail > 0 %} fail={{ pr.ci.fail }}{% endif %}{% if pr.ci.pending > 0 %} pending={{ pr.ci.pending }}{% endif %}{% if pr.ci.skip > 0 %} skip={{ pr.ci.skip }}{% endif %}{% if pr.ci.neutral > 0 %} neutral={{ pr.ci.neutral }}{% endif %} total={{ pr.ci.total }})
Review Decision: {{ pr.review_decision }}
Merge: status={{ pr.merge.status }} mergeable={{ pr.merge.mergeable }} mergeStateStatus={{ pr.merge.merge_state_status }}

Blocking Reasons:
{% if pr.merge.blocking_reasons|length == 0 -%}
- none
{% else -%}
{% for reason in pr.merge.blocking_reasons -%}
- {{ reason }}
{% endfor -%}
{% endif %}

Advisory Reasons:
{% if pr.merge.advisory_reasons|length == 0 -%}
- none
{% else -%}
{% for reason in pr.merge.advisory_reasons -%}
- {{ reason }}
{% endfor -%}
{% endif %}

Reviews ({{ pr.reviews|length }}):
{% if pr.reviews|length == 0 -%}
- none
{% else -%}
{% for review in pr.reviews -%}
- {{ review.reviewer }} [{{ review.state }}] submitted_at={{ review.submitted_at or "-" }}
{% endfor -%}
{% endif %}

Checks ({{ pr.checks|length }}):
{% if pr.checks|length == 0 -%}
- none
{% else -%}
{% for check in pr.checks -%}
- {{ check.name }} | status={{ check.status }} | conclusion={{ check.conclusion or "-" }} | started_at={{ check.started_at or "-" }} | completed_at={{ check.completed_at or "-" }} | run_url={{ check.run_url or "-" }}
{% endfor -%}
{% endif %}
"#
}

fn resolve_monitor_repo_scope(
    config: &Config,
    current_dir: &Path,
    home_dir: &Path,
    team: &str,
) -> Result<String> {
    let location = resolve_plugin_config_location("gh_monitor", current_dir, home_dir);
    let Some(table) = config.plugin_config("gh_monitor") else {
        bail!("gh_monitor plugin is not configured (run `atm gh init`)");
    };

    let cfg_team = table
        .get("team")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default();
    if cfg_team.is_empty() {
        bail!("gh_monitor configuration missing required field: team");
    }
    if cfg_team != team {
        bail!(
            "gh_monitor configured for team '{}' but command is using team '{}'",
            cfg_team,
            team
        );
    }

    let repo = table
        .get("repo")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default();
    if repo.is_empty() {
        bail!("gh_monitor configuration missing required field: repo");
    }

    if repo.contains('/') {
        return Ok(repo.to_string());
    }

    if let Some(owner) = table
        .get("owner")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|owner| !owner.is_empty())
    {
        return Ok(format!("{owner}/{repo}"));
    }

    if let Some((owner, _)) = detect_github_remote(current_dir) {
        return Ok(format!("{owner}/{repo}"));
    }

    let cfg_path = location
        .as_ref()
        .map(|l| l.path.display().to_string())
        .unwrap_or_else(|| "<unknown config>".to_string());
    bail!(
        "gh_monitor repo is '{}' but owner is missing and could not be inferred from git remote (config: {}). Set [plugins.gh_monitor].owner or [plugins.gh_monitor].repo as owner/repo.",
        repo,
        cfg_path
    );
}

fn print_pr_list_summary(summary: &GhPrListSummary) {
    println!("GitHub PR List: atm gh pr list");
    println!("Team:              {}", summary.team);
    println!("Repository:        {}", summary.repo);
    println!("Open PRs:          {}", summary.total_open_prs);
    println!("Generated At:      {}", summary.generated_at);
    println!();
    if summary.items.is_empty() {
        println!("No open pull requests found.");
        return;
    }

    for item in &summary.items {
        let draft = if item.draft { "draft" } else { "ready" };
        let ci_label = render_pr_list_ci_label(&item.ci, &item.merge);
        let merge_label = render_pr_list_merge_label(&item.merge);
        println!(
            "#{} [{}] [ci:{}] [merge:{}] [review:{}] {}",
            item.number, draft, ci_label, merge_label, item.review, item.title
        );
    }
}

fn print_pr_report_summary(report: &GhPrReportSummary) {
    println!("GitHub PR Report: atm gh pr report");
    println!("Schema Version:    {}", report.schema_version);
    println!("Team:              {}", report.team);
    println!("Repository:        {}", report.repo);
    println!("Generated At:      {}", report.generated_at);
    println!("PR:                #{}", report.pr.number);
    println!("Title:             {}", report.pr.title);
    println!("URL:               {}", report.pr.url);
    println!(
        "Draft:             {}",
        if report.pr.draft { "yes" } else { "no" }
    );
    println!();
    println!("CI:                {}", render_ci_summary(&report.pr.ci));
    println!("Review Decision:   {}", report.pr.review_decision);
    println!(
        "Merge:             status={} mergeable={} mergeStateStatus={}",
        report.pr.merge.status, report.pr.merge.mergeable, report.pr.merge.merge_state_status
    );
    println!("Blocking Reasons:");
    if report.pr.merge.blocking_reasons.is_empty() {
        println!("  - none");
    } else {
        for reason in &report.pr.merge.blocking_reasons {
            println!("  - {reason}");
        }
    }
    println!("Advisory Reasons:");
    if report.pr.merge.advisory_reasons.is_empty() {
        println!("  - none");
    } else {
        for reason in &report.pr.merge.advisory_reasons {
            println!("  - {reason}");
        }
    }

    println!();
    println!("Reviews ({}):", report.pr.reviews.len());
    if report.pr.reviews.is_empty() {
        println!("  - none");
    } else {
        for review in &report.pr.reviews {
            let submitted = review.submitted_at.as_deref().unwrap_or("-");
            println!(
                "  - {} [{}] submitted_at={}",
                review.reviewer, review.state, submitted
            );
        }
    }

    println!();
    println!("Checks ({}):", report.pr.checks.len());
    if report.pr.checks.is_empty() {
        println!("  - none");
    } else {
        for check in &report.pr.checks {
            println!(
                "  - {} | status={} | conclusion={} | started_at={} | completed_at={} | run_url={}",
                check.name,
                check.status,
                check.conclusion.as_deref().unwrap_or("-"),
                check.started_at.as_deref().unwrap_or("-"),
                check.completed_at.as_deref().unwrap_or("-"),
                check.run_url.as_deref().unwrap_or("-")
            );
        }
    }
}

fn ci_effective_total(ci: &GhCiRollup) -> u64 {
    ci.total.saturating_sub(ci.skip)
}

fn is_merge_conflict_status(merge: &str) -> bool {
    matches!(
        merge.trim().to_ascii_lowercase().as_str(),
        "dirty" | "conflicting" | "conflict"
    )
}

fn render_pr_list_merge_label(merge: &str) -> String {
    if is_merge_conflict_status(merge) {
        "CONFLICT ⚠".to_string()
    } else {
        merge.to_string()
    }
}

fn render_pr_list_ci_label(ci: &GhCiRollup, merge: &str) -> String {
    if ci.state == "fail" && is_merge_conflict_status(merge) && ci.fail > 0 && ci.pass == 0 {
        return "BLOCKED — merge conflict".to_string();
    }
    format!(
        "{} {}/{}",
        ci.state.to_uppercase(),
        ci.pass,
        ci_effective_total(ci)
    )
}

fn render_ci_summary(ci: &GhCiRollup) -> String {
    let mut parts = vec![format!("pass={}", ci.pass)];
    if ci.fail > 0 {
        parts.push(format!("fail={}", ci.fail));
    }
    if ci.pending > 0 {
        parts.push(format!("pending={}", ci.pending));
    }
    if ci.skip > 0 {
        parts.push(format!("skip={}", ci.skip));
    }
    if ci.neutral > 0 {
        parts.push(format!("neutral={}", ci.neutral));
    }
    parts.push(format!("total={}", ci.total));
    let details = parts.join(" ");
    format!(
        "{} {}/{} ({})",
        ci.state,
        ci.pass,
        ci_effective_total(ci),
        details
    )
}

fn extract_check_reports(entries: &[serde_json::Value]) -> Vec<GhMonitorCheckReport> {
    let mut checks: Vec<GhMonitorCheckReport> = entries
        .iter()
        .map(|entry| GhMonitorCheckReport {
            name: extract_check_name(entry),
            status: extract_check_status(entry),
            conclusion: extract_string_field(entry, &["conclusion"]),
            started_at: extract_string_field(entry, &["startedAt", "started_at"]),
            completed_at: extract_string_field(entry, &["completedAt", "completed_at"]),
            run_url: extract_string_field(entry, &["detailsUrl", "targetUrl", "url", "htmlUrl"]),
        })
        .collect();
    checks.sort_by(|a, b| a.name.cmp(&b.name));
    checks
}

fn extract_review_reports(entries: &[serde_json::Value]) -> Vec<GhMonitorReviewReport> {
    let mut reviews: Vec<GhMonitorReviewReport> = entries
        .iter()
        .map(|entry| {
            let reviewer = entry
                .get("author")
                .and_then(|author| author.get("login"))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    entry
                        .get("authorLogin")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                })
                .unwrap_or("unknown-reviewer")
                .to_string();
            let state = extract_string_field(entry, &["state"])
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_else(|| "unknown".to_string());
            GhMonitorReviewReport {
                reviewer,
                state,
                submitted_at: extract_string_field(entry, &["submittedAt", "submitted_at"]),
            }
        })
        .collect();
    reviews.sort_by(|a, b| a.reviewer.cmp(&b.reviewer));
    reviews
}

fn extract_check_name(entry: &serde_json::Value) -> String {
    extract_string_field(entry, &["name", "context"])
        .or_else(|| extract_string_field(entry, &["displayTitle"]))
        .unwrap_or_else(|| "unknown-check".to_string())
}

fn extract_check_status(entry: &serde_json::Value) -> String {
    extract_string_field(entry, &["status", "state"])
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| {
            if extract_string_field(entry, &["conclusion"]).is_some() {
                "completed".to_string()
            } else {
                "unknown".to_string()
            }
        })
}

fn extract_string_field(entry: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        entry
            .get(*key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn build_merge_report(
    mergeable: Option<&str>,
    merge_state_status: Option<&str>,
    draft: bool,
    ci: &GhCiRollup,
    review_decision: &str,
) -> GhMergeReport {
    let mergeable_normalized = normalize_mergeable(mergeable);
    let merge_state_status_normalized = normalize_merge_status(merge_state_status);
    let mut blocking_reasons: Vec<String> = Vec::new();
    let mut advisory_reasons: Vec<String> = Vec::new();

    if draft {
        blocking_reasons.push("PR is draft".to_string());
    }
    if mergeable_normalized == "unknown" {
        advisory_reasons.push("mergeability is UNKNOWN (transient)".to_string());
    } else if mergeable_normalized == "conflicting" {
        blocking_reasons.push("mergeability is CONFLICTING".to_string());
    }
    if matches!(
        merge_state_status_normalized.as_str(),
        "dirty" | "blocked" | "behind"
    ) {
        blocking_reasons.push(format!(
            "mergeStateStatus={}",
            merge_state_status_normalized.to_ascii_uppercase()
        ));
    } else if matches!(
        merge_state_status_normalized.as_str(),
        "pending" | "unknown"
    ) {
        advisory_reasons.push(format!(
            "mergeStateStatus={}",
            merge_state_status_normalized.to_ascii_uppercase()
        ));
    }
    if ci.fail > 0 {
        blocking_reasons.push("CI has failing checks".to_string());
    } else if ci.pending > 0 {
        blocking_reasons.push("CI checks still pending".to_string());
    }
    if review_decision == "changes_requested" {
        blocking_reasons.push("review decision is CHANGES_REQUESTED".to_string());
    } else if review_decision == "review_required" {
        advisory_reasons.push("review approval still required".to_string());
    } else if review_decision == "unknown" {
        advisory_reasons.push("review decision unavailable".to_string());
    } else if review_decision == "none" {
        advisory_reasons.push("no explicit review decision".to_string());
    }

    let status = if !blocking_reasons.is_empty() {
        "blocked"
    } else if mergeable_normalized == "unknown" {
        "indeterminate"
    } else {
        "ready"
    };

    GhMergeReport {
        mergeable: mergeable_normalized,
        merge_state_status: merge_state_status_normalized,
        status: status.to_string(),
        blocking_reasons,
        advisory_reasons,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GhCheckOutcome {
    Pass,
    Fail,
    Pending,
    Skip,
    Neutral,
}

fn summarize_ci_rollup(entries: &[serde_json::Value]) -> GhCiRollup {
    let mut total = 0_u64;
    let mut pass = 0_u64;
    let mut fail = 0_u64;
    let mut pending = 0_u64;
    let mut skip = 0_u64;
    let mut neutral = 0_u64;

    for entry in entries {
        if let Some(outcome) = classify_check_outcome(entry) {
            total += 1;
            match outcome {
                GhCheckOutcome::Pass => pass += 1,
                GhCheckOutcome::Fail => fail += 1,
                GhCheckOutcome::Pending => pending += 1,
                GhCheckOutcome::Skip => skip += 1,
                GhCheckOutcome::Neutral => neutral += 1,
            }
        }
    }

    let state = if total == 0 {
        "none"
    } else if fail > 0 {
        "fail"
    } else if pending > 0 {
        "pending"
    } else if pass + skip + neutral == total {
        "pass"
    } else {
        "mixed"
    };

    GhCiRollup {
        state: state.to_string(),
        total,
        pass,
        fail,
        pending,
        skip,
        neutral,
    }
}

fn classify_check_outcome(entry: &serde_json::Value) -> Option<GhCheckOutcome> {
    let conclusion = entry
        .get("conclusion")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    if let Some(conclusion) = conclusion.as_deref() {
        return Some(match conclusion {
            "success" => GhCheckOutcome::Pass,
            "failure" | "timed_out" | "startup_failure" | "action_required" => GhCheckOutcome::Fail,
            "skipped" => GhCheckOutcome::Skip,
            "cancelled" | "neutral" => GhCheckOutcome::Neutral,
            _ => GhCheckOutcome::Neutral,
        });
    }

    let status = entry
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            entry
                .get("state")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
        });
    status.as_deref().map(|status| match status {
        "success" => GhCheckOutcome::Pass,
        "failure" | "error" | "timed_out" | "startup_failure" | "action_required" => {
            GhCheckOutcome::Fail
        }
        "queued" | "in_progress" | "pending" | "requested" | "waiting" => GhCheckOutcome::Pending,
        "skipped" => GhCheckOutcome::Skip,
        "completed" => GhCheckOutcome::Neutral,
        "cancelled" | "neutral" => GhCheckOutcome::Neutral,
        _ => GhCheckOutcome::Neutral,
    })
}

fn normalize_review_status(value: Option<&str>) -> String {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(raw) => match raw.to_ascii_uppercase().as_str() {
            "APPROVED" => "approved".to_string(),
            "CHANGES_REQUESTED" => "changes_requested".to_string(),
            "REVIEW_REQUIRED" => "review_required".to_string(),
            _ => raw.to_ascii_lowercase(),
        },
        None => "unknown".to_string(),
    }
}

fn normalize_report_review_decision(
    value: Option<&str>,
    reviews: &[GhMonitorReviewReport],
) -> String {
    let normalized = normalize_review_status(value);
    if normalized == "unknown" && reviews.is_empty() {
        return "none".to_string();
    }
    normalized
}

fn normalize_mergeable(value: Option<&str>) -> String {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(raw) => match raw.to_ascii_uppercase().as_str() {
            "MERGEABLE" => "mergeable".to_string(),
            "CONFLICTING" => "conflicting".to_string(),
            "UNKNOWN" => "unknown".to_string(),
            _ => raw.to_ascii_lowercase(),
        },
        None => "unknown".to_string(),
    }
}

fn normalize_merge_status(value: Option<&str>) -> String {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(raw) if raw.eq_ignore_ascii_case("unknown") => "pending".to_string(),
        Some(raw) => raw.to_ascii_lowercase(),
        None => "unknown".to_string(),
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
        repo: None,
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
        state.message = Some(
            "gh_monitor configuration missing required field: repo (run `atm gh init`)".to_string(),
        );
        return state;
    }

    state.repo = Some(cfg_repo.to_string());
    state
}

fn resolve_namespace_health(
    team: &str,
    current_dir: &Path,
    repo_scope: Option<&str>,
    plugin_state: &GhPluginState,
) -> Result<GhMonitorHealth> {
    if !plugin_state.is_usable() {
        return Ok(GhMonitorHealth {
            team: team.to_string(),
            configured: plugin_state.configured,
            enabled: plugin_state.enabled,
            config_source: plugin_state.config_source.clone(),
            config_path: plugin_state.config_path.clone(),
            lifecycle_state: "unknown".to_string(),
            availability_state: "disabled_config_error".to_string(),
            in_flight: 0,
            updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            message: Some(
                plugin_state
                    .message
                    .clone()
                    .unwrap_or_else(|| "gh_monitor plugin is not configured".to_string()),
            ),
            repo_state_updated_at: None,
            budget_limit_per_hour: None,
            budget_used_in_window: None,
            rate_limit_remaining: None,
            rate_limit_limit: None,
            poll_owner: None,
            owner_runtime_kind: None,
            owner_pid: None,
            owner_binary_path: None,
            owner_atm_home: None,
            owner_repo: None,
            owner_poll_interval_secs: None,
        });
    }

    let effective_repo = repo_scope
        .map(str::to_string)
        .or_else(|| plugin_state.repo.clone());

    let daemon_error = match agent_team_mail_core::daemon_client::ensure_daemon_running() {
        Ok(()) => {
            if let Some(mut health) = gh_monitor_health_with_context(
                team,
                Some(current_dir.to_string_lossy().to_string()),
                effective_repo.clone(),
            )? {
                if health.availability_state == "disabled_config_error" {
                    if let Some(reason) = plugin_state.message.as_deref() {
                        health.message = Some(reason.to_string());
                    } else if health
                        .message
                        .as_deref()
                        .is_none_or(|m| m.trim().is_empty())
                    {
                        health.message = Some(
                            "gh_monitor configuration error: run `atm gh init` to repair setup"
                                .to_string(),
                        );
                    }
                }
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
        repo_state_updated_at: None,
        budget_limit_per_hour: None,
        budget_used_in_window: None,
        rate_limit_remaining: None,
        rate_limit_limit: None,
        poll_owner: None,
        owner_runtime_kind: None,
        owner_pid: None,
        owner_binary_path: None,
        owner_atm_home: None,
        owner_repo: None,
        owner_poll_interval_secs: None,
    })
}

fn enforce_plugin_ready(plugin_state: &GhPluginState, json: bool) -> Result<()> {
    if plugin_state.is_usable() {
        return Ok(());
    }

    let reason = if !plugin_state.configured {
        "gh_monitor plugin is not configured"
    } else if !plugin_state.enabled {
        "gh_monitor plugin is disabled in configuration"
    } else {
        plugin_state
            .message
            .as_deref()
            .unwrap_or("gh_monitor plugin is not available for this team")
    };

    if json {
        let payload = serde_json::json!({
            "error_code": "PLUGIN_UNAVAILABLE",
            "message": reason,
            "hint": "Run `atm gh init` to configure and enable GitHub monitor for this team.",
        });
        bail!("{}", serde_json::to_string_pretty(&payload)?);
    }

    bail!("{reason}\nRemediation: run `atm gh init` and retry.")
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
    if let Some(repo_state_updated_at) = status.repo_state_updated_at.as_deref() {
        println!("Repo State:  {repo_state_updated_at}");
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
    if let Some(repo_state_updated_at) = status.repo_state_updated_at.as_deref() {
        println!("Repo State:        {repo_state_updated_at}");
    }
    if let (Some(used), Some(limit)) = (status.budget_used_in_window, status.budget_limit_per_hour)
    {
        println!("Budget:            {used}/{limit} calls per hour");
    }
    if let (Some(remaining), Some(limit)) = (status.rate_limit_remaining, status.rate_limit_limit) {
        println!("Rate Limit:        {remaining}/{limit} remaining");
    }
    if let Some(owner) = status.poll_owner.as_deref() {
        println!("Poll Owner:        {owner}");
    }
    if let Some(runtime_kind) = status.owner_runtime_kind.as_deref() {
        println!("Owner Runtime:     {runtime_kind}");
    }
    if let Some(pid) = status.owner_pid {
        println!("Owner PID:         {pid}");
    }
    if let Some(binary_path) = status.owner_binary_path.as_deref() {
        println!("Owner Binary:      {binary_path}");
    }
    if let Some(atm_home) = status.owner_atm_home.as_deref() {
        println!("Owner ATM_HOME:    {atm_home}");
    }
    if let Some(repo) = status.owner_repo.as_deref() {
        println!("Owner Repo:        {repo}");
    }
    if let Some(poll_interval_secs) = status.owner_poll_interval_secs {
        println!("Poll Interval:     {}s", poll_interval_secs);
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
        repo_state_updated_at: health.repo_state_updated_at.clone(),
        budget_limit_per_hour: health.budget_limit_per_hour,
        budget_used_in_window: health.budget_used_in_window,
        rate_limit_remaining: health.rate_limit_remaining,
        rate_limit_limit: health.rate_limit_limit,
        poll_owner: health.poll_owner.clone(),
        owner_runtime_kind: health.owner_runtime_kind.clone(),
        owner_pid: health.owner_pid,
        owner_binary_path: health.owner_binary_path.clone(),
        owner_atm_home: health.owner_atm_home.clone(),
        owner_repo: health.owner_repo.clone(),
        owner_poll_interval_secs: health.owner_poll_interval_secs,
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
            "atm gh pr list",
            "atm gh pr report <pr-number>",
            "atm gh pr init-report [--output <path>]",
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
    repo_override: Option<&str>,
    json: bool,
) -> Result<()> {
    validate_gh_cli_prerequisites()?;

    let detected = detect_github_remote(current_dir);
    let (owner, repo) = resolve_repo_coordinates(repo_override, detected.as_ref())?;
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
    } else {
        gh.remove("owner");
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
            "atm gh pr list".to_string(),
            "atm gh pr report <pr-number>".to_string(),
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
        println!("Repository:   {}", preview.repo);
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

fn resolve_repo_override(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("--repo cannot be empty");
    }

    if let GitProvider::GitHub { owner, repo } = GitProvider::detect_from_url(trimmed) {
        return Ok(format!("{owner}/{repo}"));
    }

    if let Some((owner, repo)) = trimmed.split_once('/') {
        let owner = owner.trim();
        let repo = repo.trim().trim_end_matches(".git");
        if owner.is_empty() || repo.is_empty() {
            bail!("--repo must be `owner/repo` or a full GitHub URL");
        }
        return Ok(format!("{owner}/{repo}"));
    }

    bail!("--repo must be `owner/repo` or a full GitHub URL")
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

        if let GitProvider::GitHub { owner, repo } = GitProvider::detect_from_url(trimmed) {
            return Ok((Some(owner.clone()), format!("{owner}/{repo}")));
        }

        if let Some((owner, repo)) = trimmed.split_once('/') {
            let owner = owner.trim();
            let repo = repo.trim().trim_end_matches(".git");
            if owner.is_empty() || repo.is_empty() {
                bail!("--repo must be `owner/repo`, `repo`, or a full GitHub URL");
            }
            return Ok((Some(owner.to_string()), format!("{owner}/{repo}")));
        }

        let owner = detected.map(|(owner, _)| owner.clone());
        let repo = owner
            .as_ref()
            .map(|owner| format!("{owner}/{trimmed}"))
            .unwrap_or_else(|| trimmed.to_string());
        return Ok((owner, repo));
    }

    if let Some((owner, repo)) = detected {
        return Ok((Some(owner.clone()), format!("{owner}/{repo}")));
    }

    bail!(
        "Could not determine GitHub repository from git remote. Use `atm gh init --repo <owner/repo>`"
    )
}

fn resolve_daemon_repo_scope(repo_arg: Option<&str>, current_dir: &Path) -> Result<String> {
    if let Some(raw) = repo_arg {
        return resolve_repo_override(raw);
    }

    if let Some((owner, repo)) = detect_github_remote(current_dir) {
        return Ok(format!("{owner}/{repo}"));
    }

    bail!(
        "Could not determine GitHub repository from current directory. Run from a git checkout with a GitHub remote or pass `--repo <owner/repo>`."
    )
}

fn resolve_monitor_caller_identity(config: &Config) -> String {
    std::env::var("ATM_IDENTITY")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| config.core.identity.clone())
}

fn validate_cross_team_monitor_control(
    actor_team: &str,
    target_team: &str,
    user_authorized: bool,
    reason: Option<&str>,
) -> Result<()> {
    if actor_team == target_team {
        return Ok(());
    }

    if !user_authorized {
        bail!(
            "cross-team gh monitor control from '{}' to '{}' requires --user-authorized",
            actor_team,
            target_team
        );
    }

    if reason.map(str::trim).is_none_or(str::is_empty) {
        bail!(
            "cross-team gh monitor control from '{}' to '{}' requires --reason",
            actor_team,
            target_team
        );
    }

    Ok(())
}

fn audit_monitor_control_action(
    action: &str,
    actor: &str,
    actor_team: &str,
    target_team: &str,
    repo: Option<&str>,
    reason: Option<&str>,
    user_authorized: bool,
) {
    emit_event_best_effort(build_monitor_control_audit_fields(
        action,
        actor,
        actor_team,
        target_team,
        repo,
        reason,
        user_authorized,
    ));
}

fn build_monitor_control_audit_fields(
    action: &str,
    actor: &str,
    actor_team: &str,
    target_team: &str,
    repo: Option<&str>,
    reason: Option<&str>,
    user_authorized: bool,
) -> EventFields {
    let mut extra = serde_json::Map::new();
    extra.insert("actor".to_string(), serde_json::json!(actor));
    extra.insert("actor_team".to_string(), serde_json::json!(actor_team));
    extra.insert("target_team".to_string(), serde_json::json!(target_team));
    extra.insert(
        "user_authorized".to_string(),
        serde_json::json!(user_authorized),
    );
    if let Some(repo) = repo {
        extra.insert("repo".to_string(), serde_json::json!(repo));
    }
    if let Some(reason) = reason.filter(|value| !value.trim().is_empty()) {
        extra.insert("reason".to_string(), serde_json::json!(reason.trim()));
    }
    EventFields {
        level: "info",
        source: "atm",
        action: "gh_monitor_control",
        team: Some(target_team.to_string()),
        target: repo.map(str::to_string),
        runtime: Some(actor_team.to_string()),
        result: Some(action.to_string()),
        agent_name: Some(actor.to_string()),
        extra_fields: extra,
        ..Default::default()
    }
}

fn notify_team_lead_of_monitor_control(
    home_dir: &Path,
    actor: &str,
    actor_team: &str,
    target_team: &str,
    action_word: &str,
    reason: &str,
) -> Result<()> {
    let teams_root = teams_root_dir_for(home_dir);
    let team_dir = teams_root.join(target_team);
    let lead_agent = TeamConfigStore::open(&team_dir)
        .read()
        .ok()
        .and_then(|cfg| {
            cfg.lead_agent_id
                .split('@')
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "team-lead".to_string());
    let inbox_path = teams_root
        .join(target_team)
        .join("inboxes")
        .join(format!("{lead_agent}.json"));
    let now = chrono::Utc::now().to_rfc3339();
    let message = InboxMessage {
        from: actor.to_string(),
        text: format!(
            "your gh monitor was {} by {}@{} for {}",
            action_word,
            actor,
            actor_team,
            reason.trim()
        ),
        timestamp: now.clone(),
        read: false,
        summary: Some(format!(
            "gh monitor {} by {}@{}",
            action_word, actor, actor_team
        )),
        message_id: Some(format!(
            "gh-monitor-{}-{}-{}",
            action_word,
            target_team,
            chrono::Utc::now().timestamp_millis()
        )),
        unknown_fields: std::collections::HashMap::new(),
    };
    let _ = inbox_append(&inbox_path, &message, target_team, &lead_agent)?;
    Ok(())
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
    use tempfile::TempDir;

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
        assert_eq!(coords.1, "acme/repo");
    }

    #[test]
    fn resolve_repo_coordinates_uses_detected_owner_for_repo_only() {
        let detected = ("acme".to_string(), "agent-team-mail".to_string());
        let coords = resolve_repo_coordinates(Some("agent-team-mail"), Some(&detected)).unwrap();
        assert_eq!(coords.0.as_deref(), Some("acme"));
        assert_eq!(coords.1, "acme/agent-team-mail");
    }

    #[test]
    fn resolve_repo_coordinates_defaults_to_detected_owner_repo() {
        let detected = ("acme".to_string(), "agent-team-mail".to_string());
        let coords = resolve_repo_coordinates(None, Some(&detected)).unwrap();
        assert_eq!(coords.0.as_deref(), Some("acme"));
        assert_eq!(coords.1, "acme/agent-team-mail");
    }

    #[test]
    fn summarize_ci_rollup_marks_fail_when_any_check_fails() {
        let entries = vec![
            serde_json::json!({"conclusion":"SUCCESS"}),
            serde_json::json!({"conclusion":"FAILURE"}),
            serde_json::json!({"status":"queued"}),
        ];
        let rollup = summarize_ci_rollup(&entries);
        assert_eq!(rollup.state, "fail");
        assert_eq!(rollup.total, 3);
        assert_eq!(rollup.pass, 1);
        assert_eq!(rollup.fail, 1);
        assert_eq!(rollup.pending, 1);
    }

    #[test]
    fn summarize_ci_rollup_marks_pending_without_failures() {
        let entries = vec![
            serde_json::json!({"conclusion":"SUCCESS"}),
            serde_json::json!({"status":"in_progress"}),
        ];
        let rollup = summarize_ci_rollup(&entries);
        assert_eq!(rollup.state, "pending");
        assert_eq!(rollup.total, 2);
        assert_eq!(rollup.pass, 1);
        assert_eq!(rollup.pending, 1);
    }

    #[test]
    fn summarize_ci_rollup_marks_pass_when_all_success() {
        let entries = vec![
            serde_json::json!({"conclusion":"SUCCESS"}),
            serde_json::json!({"state":"success"}),
        ];
        let rollup = summarize_ci_rollup(&entries);
        assert_eq!(rollup.state, "pass");
        assert_eq!(rollup.total, 2);
        assert_eq!(rollup.pass, 2);
        assert_eq!(rollup.fail, 0);
        assert_eq!(rollup.pending, 0);
    }

    #[test]
    fn summarize_ci_rollup_marks_pass_when_neutral_skipped_checks_present() {
        // 15 pass + 1 skipped + 1 neutral should be "pass", not "mixed"
        let mut entries: Vec<serde_json::Value> = (0..15)
            .map(|_| serde_json::json!({"conclusion":"SUCCESS"}))
            .collect();
        entries.push(serde_json::json!({"conclusion":"SKIPPED"}));
        entries.push(serde_json::json!({"conclusion":"CANCELLED"}));
        let rollup = summarize_ci_rollup(&entries);
        assert_eq!(rollup.state, "pass");
        assert_eq!(rollup.total, 17);
        assert_eq!(rollup.pass, 15);
        assert_eq!(rollup.skip, 1);
        assert_eq!(rollup.neutral, 1);
        assert_eq!(rollup.fail, 0);
        assert_eq!(rollup.pending, 0);
    }

    #[test]
    fn build_merge_report_unknown_mergeability_is_indeterminate_not_blocking() {
        let ci = GhCiRollup {
            state: "pass".to_string(),
            total: 2,
            pass: 2,
            fail: 0,
            pending: 0,
            skip: 0,
            neutral: 0,
        };
        let merge = build_merge_report(Some("UNKNOWN"), Some("CLEAN"), false, &ci, "approved");
        assert_eq!(merge.status, "indeterminate");
        assert_eq!(merge.mergeable, "unknown");
        assert!(merge.blocking_reasons.is_empty());
        assert!(
            merge
                .advisory_reasons
                .iter()
                .any(|reason| reason.contains("UNKNOWN"))
        );
    }

    #[test]
    fn render_pr_list_labels_highlight_merge_conflicts() {
        let ci = GhCiRollup {
            state: "fail".to_string(),
            total: 1,
            pass: 0,
            fail: 1,
            pending: 0,
            skip: 0,
            neutral: 0,
        };
        assert_eq!(render_pr_list_merge_label("dirty"), "CONFLICT ⚠");
        assert_eq!(
            render_pr_list_ci_label(&ci, "dirty"),
            "BLOCKED — merge conflict"
        );
    }

    #[test]
    fn render_pr_list_labels_preserve_non_conflict_ci_summary() {
        let ci = GhCiRollup {
            state: "pending".to_string(),
            total: 2,
            pass: 1,
            fail: 0,
            pending: 1,
            skip: 0,
            neutral: 0,
        };
        assert_eq!(render_pr_list_merge_label("clean"), "clean");
        assert_eq!(render_pr_list_ci_label(&ci, "clean"), "PENDING 1/2");
    }

    #[test]
    fn normalize_report_review_decision_maps_empty_to_none_when_no_reviews() {
        let reviews: Vec<GhMonitorReviewReport> = vec![];
        assert_eq!(normalize_report_review_decision(None, &reviews), "none");
        assert_eq!(normalize_report_review_decision(Some(""), &reviews), "none");
    }

    #[test]
    fn extract_check_reports_maps_check_run_and_context_fields() {
        let entries = vec![
            serde_json::json!({
                "name":"clippy",
                "status":"COMPLETED",
                "conclusion":"SUCCESS",
                "startedAt":"2026-03-09T01:00:00Z",
                "completedAt":"2026-03-09T01:02:00Z",
                "detailsUrl":"https://example.test/run/1"
            }),
            serde_json::json!({
                "context":"required-review",
                "state":"PENDING",
                "targetUrl":"https://example.test/check/2"
            }),
        ];
        let checks = extract_check_reports(&entries);
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "clippy");
        assert_eq!(checks[0].status, "completed");
        assert_eq!(checks[0].conclusion.as_deref(), Some("SUCCESS"));
        assert_eq!(checks[1].name, "required-review");
        assert_eq!(checks[1].status, "pending");
        assert_eq!(
            checks[1].run_url.as_deref(),
            Some("https://example.test/check/2")
        );
    }

    #[test]
    fn extract_review_reports_maps_reviewer_state_and_timestamp() {
        let entries = vec![
            serde_json::json!({
                "author":{"login":"alice"},
                "state":"APPROVED",
                "submittedAt":"2026-03-09T01:00:00Z"
            }),
            serde_json::json!({
                "author":{"login":"bob"},
                "state":"CHANGES_REQUESTED"
            }),
        ];
        let reviews = extract_review_reports(&entries);
        assert_eq!(reviews.len(), 2);
        assert_eq!(reviews[0].reviewer, "alice");
        assert_eq!(reviews[0].state, "approved");
        assert_eq!(
            reviews[0].submitted_at.as_deref(),
            Some("2026-03-09T01:00:00Z")
        );
        assert_eq!(reviews[1].reviewer, "bob");
        assert_eq!(reviews[1].state, "changes_requested");
    }

    #[test]
    fn normalize_merge_status_maps_unknown_to_pending() {
        assert_eq!(normalize_merge_status(Some("UNKNOWN")), "pending");
        assert_eq!(normalize_merge_status(Some("unknown")), "pending");
        assert_eq!(normalize_merge_status(Some("CLEAN")), "clean");
    }

    #[test]
    fn execute_pr_init_report_writes_default_template_file() {
        let tmp = TempDir::new().expect("tempdir");
        execute_pr_init_report(tmp.path(), None, false).expect("init report");
        let template_path = tmp.path().join(GH_MONITOR_DEFAULT_TEMPLATE_FILENAME);
        assert!(template_path.exists());
        let content = std::fs::read_to_string(template_path).expect("read template");
        assert!(content.contains("schema {{ schema_version }}"));
        assert!(content.contains("{{ pr.number }}"));
    }

    #[test]
    fn render_pr_report_template_renders_report_payload() {
        let tmp = TempDir::new().expect("tempdir");
        let template_path = tmp.path().join("custom-template.j2");
        std::fs::write(
            &template_path,
            "team={{ team }} pr={{ pr.number }} schema={{ schema_version }}",
        )
        .expect("write template");

        let report = GhPrReportSummary {
            schema_version: GH_MONITOR_REPORT_SCHEMA_VERSION.to_string(),
            team: "atm-dev".to_string(),
            repo: "acme/repo".to_string(),
            generated_at: "2026-03-09T00:00:00Z".to_string(),
            pr: GhMonitorReportPr {
                number: 42,
                title: "Title".to_string(),
                url: "https://example.test/pr/42".to_string(),
                draft: false,
                ci: GhCiRollup {
                    state: "pass".to_string(),
                    total: 1,
                    pass: 1,
                    fail: 0,
                    pending: 0,
                    skip: 0,
                    neutral: 0,
                },
                review_decision: "approved".to_string(),
                merge: GhMergeReport {
                    mergeable: "mergeable".to_string(),
                    merge_state_status: "clean".to_string(),
                    status: "ready".to_string(),
                    blocking_reasons: vec![],
                    advisory_reasons: vec![],
                },
                checks: vec![],
                reviews: vec![],
            },
        };

        let rendered = render_pr_report_template(&template_path, &report).expect("render template");
        assert_eq!(rendered, "team=atm-dev pr=42 schema=1.0.0");
    }

    #[test]
    fn build_monitor_control_audit_fields_captures_authorized_cross_team_stop() {
        let fields = build_monitor_control_audit_fields(
            "stop",
            "team-lead",
            "atm-dev",
            "ops-team",
            Some("owner/repo"),
            Some("runaway polling"),
            true,
        );

        assert_eq!(fields.action, "gh_monitor_control");
        assert_eq!(fields.team.as_deref(), Some("ops-team"));
        assert_eq!(fields.target.as_deref(), Some("owner/repo"));
        assert_eq!(fields.runtime.as_deref(), Some("atm-dev"));
        assert_eq!(fields.result.as_deref(), Some("stop"));
        assert_eq!(fields.agent_name.as_deref(), Some("team-lead"));
        assert_eq!(
            fields
                .extra_fields
                .get("user_authorized")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            fields
                .extra_fields
                .get("target_team")
                .and_then(|value| value.as_str()),
            Some("ops-team")
        );
    }
}
