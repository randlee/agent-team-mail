//! GitHub CI monitor command surface (`atm gh ...`).

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::daemon_client::{
    GhMonitorRequest, GhMonitorStatus, GhMonitorTargetKind, GhStatusRequest, gh_monitor, gh_status,
};
use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};

use crate::util::settings::get_home_dir;

/// GitHub CI monitor commands.
#[derive(Args, Debug)]
pub struct GhArgs {
    /// Team override (defaults to configured default team)
    #[arg(long)]
    team: Option<String>,

    /// Output as JSON
    #[arg(long)]
    json: bool,

    #[command(subcommand)]
    command: GhCommand,
}

#[derive(Subcommand, Debug)]
enum GhCommand {
    /// Start CI monitoring
    Monitor(MonitorArgs),
    /// Query CI monitor status
    Status(StatusArgs),
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum StatusTargetKind {
    Pr,
    Workflow,
    Run,
}

#[derive(Args, Debug)]
struct StatusArgs {
    /// Monitor target kind
    target_kind: StatusTargetKind,

    /// Monitor target value (PR number, workflow name, or run id)
    target: String,
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

    // Keep CLI behavior deterministic: surface explicit daemon-unavailable errors
    // and preserve plugin-owned routing through daemon socket commands.
    agent_team_mail_core::daemon_client::ensure_daemon_running()
        .context("failed to auto-start daemon for atm gh command")?;

    let status = match args.command {
        GhCommand::Monitor(monitor) => {
            let request = build_monitor_request(team, monitor);
            gh_monitor(&request)?.ok_or_else(|| {
                anyhow::anyhow!("daemon is not reachable for atm gh monitor command")
            })?
        }
        GhCommand::Status(status) => {
            let request = GhStatusRequest {
                team: team.to_string(),
                target_kind: status_kind_to_wire(status.target_kind),
                target: status.target,
            };
            gh_status(&request)?.ok_or_else(|| {
                anyhow::anyhow!("daemon is not reachable for atm gh status command")
            })?
        }
    };

    print_status(&status, args.json)
}

fn build_monitor_request(team: &str, monitor: MonitorArgs) -> GhMonitorRequest {
    match monitor.target {
        MonitorTarget::Pr(pr) => GhMonitorRequest {
            team: team.to_string(),
            target_kind: GhMonitorTargetKind::Pr,
            target: pr.number.to_string(),
            reference: None,
            start_timeout_secs: Some(pr.start_timeout_secs),
        },
        MonitorTarget::Workflow(workflow) => GhMonitorRequest {
            team: team.to_string(),
            target_kind: GhMonitorTargetKind::Workflow,
            target: workflow.name,
            reference: Some(workflow.reference),
            start_timeout_secs: Some(workflow.start_timeout_secs),
        },
        MonitorTarget::Run(run) => GhMonitorRequest {
            team: team.to_string(),
            target_kind: GhMonitorTargetKind::Run,
            target: run.run_id.to_string(),
            reference: None,
            start_timeout_secs: None,
        },
    }
}

fn status_kind_to_wire(kind: StatusTargetKind) -> GhMonitorTargetKind {
    match kind {
        StatusTargetKind::Pr => GhMonitorTargetKind::Pr,
        StatusTargetKind::Workflow => GhMonitorTargetKind::Workflow,
        StatusTargetKind::Run => GhMonitorTargetKind::Run,
    }
}

fn print_status(status: &GhMonitorStatus, json: bool) -> Result<()> {
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
    if let Some(run_id) = status.run_id {
        println!("Run ID:      {run_id}");
    }
    if let Some(message) = status.message.as_deref() {
        println!("Message:     {message}");
    }
    println!("Updated At:  {}", status.updated_at);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_monitor_request_pr_maps_fields() {
        let req = build_monitor_request(
            "atm-dev",
            MonitorArgs {
                target: MonitorTarget::Pr(MonitorPrArgs {
                    number: 123,
                    start_timeout_secs: 15,
                }),
            },
        );
        assert_eq!(req.team, "atm-dev");
        assert_eq!(req.target_kind, GhMonitorTargetKind::Pr);
        assert_eq!(req.target, "123");
        assert_eq!(req.start_timeout_secs, Some(15));
    }

    #[test]
    fn build_monitor_request_workflow_maps_fields() {
        let req = build_monitor_request(
            "atm-dev",
            MonitorArgs {
                target: MonitorTarget::Workflow(MonitorWorkflowArgs {
                    name: "ci".to_string(),
                    reference: "develop".to_string(),
                    start_timeout_secs: 20,
                }),
            },
        );
        assert_eq!(req.target_kind, GhMonitorTargetKind::Workflow);
        assert_eq!(req.target, "ci");
        assert_eq!(req.reference.as_deref(), Some("develop"));
        assert_eq!(req.start_timeout_secs, Some(20));
    }

    #[test]
    fn build_monitor_request_run_maps_fields() {
        let req = build_monitor_request(
            "atm-dev",
            MonitorArgs {
                target: MonitorTarget::Run(MonitorRunArgs { run_id: 987654 }),
            },
        );
        assert_eq!(req.target_kind, GhMonitorTargetKind::Run);
        assert_eq!(req.target, "987654");
        assert_eq!(req.reference, None);
        assert_eq!(req.start_timeout_secs, None);
    }
}
