//! Staged polling helpers extracted during AM.4.
//!
//! These helpers remain module-local until AM.6 finishes thinning the remaining
//! socket/plugin call sites onto the routed CI-monitor surface.
#![allow(dead_code)]

use super::alerts::{emit_ci_monitor_message, emit_merge_conflict_alert, resolve_ci_alert_routing};
use super::gh_cli::{
    fetch_failed_log_excerpt, fetch_pull_request, fetch_run, is_pr_merge_state_dirty,
};
use super::helpers::upsert_gh_monitor_status;
use super::provider::ErasedCiProvider;
use super::types::{CiJob, CiRun, CiRunStatus, CiStep};
use crate::plugin::PluginError;
use agent_team_mail_core::daemon_client::{GhMonitorRequest, GhMonitorStatus, GhMonitorTargetKind};
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GhRunTerminalState {
    Success,
    Failure,
    TimedOut,
    Cancelled,
    ActionRequired,
    Other,
}

pub(crate) async fn monitor_run(
    provider: &dyn ErasedCiProvider,
    home: &std::path::Path,
    status_seed: &GhMonitorStatus,
    gh_request: &GhMonitorRequest,
    expected_repo_slug: Option<&str>,
    run_id: u64,
) -> Result<(), PluginError> {
    let mut seen_completed: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut pending_completed: Vec<CiJob> = Vec::new();
    let mut last_progress_emit: Option<std::time::Instant> = None;
    let mut first_poll = true;

    loop {
        let run = fetch_run(provider, run_id).await?;
        let expected_repo = expected_repo_slug
            .map(str::to_string)
            .or_else(|| extract_repo_slug_from_url(&run.url));
        let (from_agent, targets) = resolve_ci_alert_routing(
            home,
            &status_seed.team,
            gh_request.config_cwd.as_deref(),
            expected_repo.as_deref(),
        );

        let completed_jobs: Vec<CiJob> = run
            .jobs
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(is_job_completed)
            .collect();
        for job in completed_jobs {
            if seen_completed.insert(job.id) {
                pending_completed.push(job);
            }
        }

        let terminal = classify_terminal_state(&run);
        if terminal.is_none() {
            let now = std::time::Instant::now();
            if should_emit_progress(last_progress_emit, now) && !pending_completed.is_empty() {
                let message = format_progress_message(&run, &pending_completed);
                let summary = format!(
                    "ci progress: run {} ({}/{})",
                    run.id,
                    count_completed_jobs(&run),
                    run.jobs.as_ref().map_or(0, Vec::len)
                );
                emit_ci_monitor_message(
                    home,
                    &from_agent,
                    &targets,
                    &summary,
                    &message,
                    Some(format!("ci-progress-{}-{}", run.id, uuid::Uuid::new_v4())),
                );
                pending_completed.clear();
                last_progress_emit = Some(now);
            }

            let mut state = status_seed.clone();
            state.run_id = Some(run.id);
            state.state = "monitoring".to_string();
            state.updated_at = chrono::Utc::now().to_rfc3339();
            state.message = Some(format!(
                "Run {} still in progress ({}/{})",
                run.id,
                count_completed_jobs(&run),
                run.jobs.as_ref().map_or(0, Vec::len)
            ));
            upsert_gh_monitor_status(home, state).map_err(to_provider_error)?;

            let sleep_secs = if first_poll { 5 } else { 15 };
            first_poll = false;
            tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
            continue;
        }

        let terminal = terminal.unwrap_or(GhRunTerminalState::Other);
        let summary_table = format_summary_table(&run);
        let mut message = format!(
            "CI monitor terminal update\nRun: {}\nWorkflow: {}\nState: {}\nURL: {}\n\n{}\n",
            run.id,
            run.name,
            terminal_state_label(terminal),
            run.url,
            summary_table
        );

        if terminal != GhRunTerminalState::Success {
            let correlation_id = format!("ci-failure-{}-{}", run.id, uuid::Uuid::new_v4());
            let failure_payload =
                build_failure_payload(provider, &run, status_seed, gh_request, &correlation_id)
                    .await;
            message.push_str("\nFailure details:\n");
            message.push_str(&failure_payload);
        }

        emit_ci_monitor_message(
            home,
            &from_agent,
            &targets,
            &format!(
                "ci terminal: run {} {}",
                run.id,
                terminal_state_label(terminal)
            ),
            &message,
            Some(format!("ci-terminal-{}-{}", run.id, uuid::Uuid::new_v4())),
        );

        let mut state = status_seed.clone();
        state.run_id = Some(run.id);
        state.state = terminal_state_label(terminal)
            .to_lowercase()
            .replace(' ', "_");
        state.updated_at = chrono::Utc::now().to_rfc3339();
        state.message = Some(format!(
            "Terminal: {} ({}/{})",
            terminal_state_label(terminal),
            count_completed_jobs(&run),
            run.jobs.as_ref().map_or(0, Vec::len)
        ));
        upsert_gh_monitor_status(home, state).map_err(to_provider_error)?;

        if matches!(gh_request.target_kind, GhMonitorTargetKind::Pr)
            && let Ok(pr_number) = status_seed.target.trim().parse::<u64>()
        {
            match fetch_pull_request(provider, pr_number).await {
                Ok(Some(pr_view)) => {
                    if let Some(merge_state_status) = pr_view.merge_state_status.as_deref()
                        && is_pr_merge_state_dirty(merge_state_status)
                    {
                        emit_merge_conflict_alert(
                            home,
                            status_seed,
                            pr_view.url.as_deref(),
                            merge_state_status,
                            run.conclusion.map(ci_conclusion_label),
                            gh_request.config_cwd.as_deref(),
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        team = %status_seed.team,
                        pr = %status_seed.target,
                        "gh-monitor post-terminal mergeStateStatus lookup failed: {e}"
                    );
                }
            }
        }
        return Ok(());
    }
}

fn to_provider_error(e: impl std::fmt::Display) -> PluginError {
    PluginError::Runtime {
        message: e.to_string(),
        source: None,
    }
}

pub(crate) async fn build_failure_payload(
    provider: &dyn ErasedCiProvider,
    run: &CiRun,
    status_seed: &GhMonitorStatus,
    gh_request: &GhMonitorRequest,
    correlation_id: &str,
) -> String {
    let failed_jobs: Vec<&CiJob> = run
        .jobs
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|job| matches!(job_status_label(job), "failure" | "timed_out"))
        .collect();
    let failed_job_names = failed_jobs
        .iter()
        .map(|job| job.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let failed_job_urls = failed_jobs
        .iter()
        .map(|job| {
            job.url
                .clone()
                .unwrap_or_else(|| format!("{}/job/{}", run.url.trim_end_matches('/'), job.id))
        })
        .collect::<Vec<_>>();
    let first_failing_step = failed_jobs
        .iter()
        .flat_map(|job| job.steps.as_deref().unwrap_or(&[]).iter())
        .find(|step| step_concluded_failed(step))
        .map(|step| step.name.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let failed_log_excerpt = if let Some(first_job) = failed_jobs.first() {
        fetch_failed_log_excerpt(provider, first_job.id)
            .await
            .unwrap_or_else(|_| "(log excerpt unavailable)".to_string())
    } else {
        "(no failed jobs captured)".to_string()
    };

    let pr_url = derive_pr_url(run, status_seed, gh_request);
    format!(
        "run_url: {run_url}\nfailed_job_urls: {failed_job_urls}\npr_url: {pr_url}\nworkflow: {workflow}\njob_names: {job_names}\nrun_id: {run_id}\nrun_attempt: {attempt}\nbranch: {branch}\ncommit_short: {sha_short}\ncommit_full: {sha_full}\nclassification: {classification}\nfirst_failing_step: {first_failing_step}\nlog_excerpt: {log_excerpt}\ncorrelation_id: {correlation_id}\nnext_action_hint: {next_action}\nrepo_base: {repo_base}",
        run_url = run.url,
        failed_job_urls = if failed_job_urls.is_empty() {
            "(none)".to_string()
        } else {
            failed_job_urls.join(", ")
        },
        pr_url = pr_url.unwrap_or_else(|| "(unknown)".to_string()),
        workflow = run.name,
        job_names = if failed_job_names.is_empty() {
            "(none)".to_string()
        } else {
            failed_job_names
        },
        run_id = run.id,
        attempt = run.attempt.unwrap_or(1),
        branch = run.head_branch,
        sha_short = short_sha(&run.head_sha),
        sha_full = run.head_sha,
        classification = classify_failure(run),
        first_failing_step = first_failing_step,
        log_excerpt = failed_log_excerpt
            .replace('\n', " ")
            .chars()
            .take(240)
            .collect::<String>(),
        correlation_id = correlation_id,
        next_action = if failed_jobs.is_empty() {
            format!("atm gh status run {}", run.id)
        } else {
            format!("gh run view {} --log-failed", run.id)
        },
        repo_base = derive_repo_base_from_run_url(&run.url).unwrap_or_default(),
    )
}

pub(crate) fn should_emit_progress(
    last_progress_emit: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    match last_progress_emit {
        None => true,
        Some(prev) => now.duration_since(prev) >= std::time::Duration::from_secs(60),
    }
}

pub(crate) fn is_job_completed(job: &CiJob) -> bool {
    job.status == CiRunStatus::Completed
}

pub(crate) fn count_completed_jobs(run: &CiRun) -> usize {
    run.jobs
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|job| is_job_completed(job))
        .count()
}

pub(crate) fn format_progress_message(run: &CiRun, pending_completed: &[CiJob]) -> String {
    let new_jobs = pending_completed
        .iter()
        .map(|job| format!("{}({})", job.name, job_status_label(job)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "CI monitor progress\nRun: {}\nWorkflow: {}\nCompleted: {}/{}\nNewly completed: {}\nRun URL: {}",
        run.id,
        run.name,
        count_completed_jobs(run),
        run.jobs.as_ref().map_or(0, Vec::len),
        if new_jobs.is_empty() {
            "(none)"
        } else {
            &new_jobs
        },
        run.url
    )
}

pub(crate) fn job_status_label(job: &CiJob) -> &'static str {
    match job.conclusion {
        Some(super::types::CiRunConclusion::Success) => "success",
        Some(super::types::CiRunConclusion::Failure) => "failure",
        Some(super::types::CiRunConclusion::TimedOut) => "timed_out",
        Some(super::types::CiRunConclusion::Cancelled) => "cancelled",
        Some(super::types::CiRunConclusion::ActionRequired) => "action_required",
        _ if is_job_completed(job) => "completed",
        _ => "in_progress",
    }
}

pub(crate) fn classify_terminal_state(run: &CiRun) -> Option<GhRunTerminalState> {
    if run.status != CiRunStatus::Completed && run.conclusion.is_none() {
        return None;
    }
    Some(match run.conclusion {
        Some(super::types::CiRunConclusion::Success) => GhRunTerminalState::Success,
        Some(super::types::CiRunConclusion::Failure) => GhRunTerminalState::Failure,
        Some(super::types::CiRunConclusion::TimedOut) => GhRunTerminalState::TimedOut,
        Some(super::types::CiRunConclusion::Cancelled) => GhRunTerminalState::Cancelled,
        Some(super::types::CiRunConclusion::ActionRequired) => GhRunTerminalState::ActionRequired,
        _ => GhRunTerminalState::Other,
    })
}

pub(crate) fn terminal_state_label(state: GhRunTerminalState) -> &'static str {
    match state {
        GhRunTerminalState::Success => "SUCCESS",
        GhRunTerminalState::Failure => "FAILURE",
        GhRunTerminalState::TimedOut => "TIMED_OUT",
        GhRunTerminalState::Cancelled => "CANCELLED",
        GhRunTerminalState::ActionRequired => "ACTION_REQUIRED",
        GhRunTerminalState::Other => "UNKNOWN",
    }
}

pub(crate) fn format_summary_table(run: &CiRun) -> String {
    let mut lines = vec![
        "| Job/Test | Status | Runtime |".to_string(),
        "|---|---|---|".to_string(),
    ];
    for job in run.jobs.as_deref().unwrap_or(&[]) {
        lines.push(format!(
            "| {} | {} | {} |",
            job.name,
            job_status_label(job),
            format_job_runtime(job)
        ));
    }
    lines.join("\n")
}

pub(crate) fn format_job_runtime(job: &CiJob) -> String {
    let Some(started) = job.started_at.as_deref() else {
        return "-".to_string();
    };
    let Some(completed) = job.completed_at.as_deref() else {
        return "-".to_string();
    };
    let Ok(started_dt) = chrono::DateTime::parse_from_rfc3339(started) else {
        return "-".to_string();
    };
    let Ok(completed_dt) = chrono::DateTime::parse_from_rfc3339(completed) else {
        return "-".to_string();
    };
    let secs = completed_dt
        .signed_duration_since(started_dt)
        .num_seconds()
        .max(0);
    format!("{}m {}s", secs / 60, secs % 60)
}

pub(crate) fn classify_failure(run: &CiRun) -> &'static str {
    match run.conclusion {
        Some(super::types::CiRunConclusion::TimedOut) => "timeout",
        Some(super::types::CiRunConclusion::Cancelled) => "cancelled",
        Some(super::types::CiRunConclusion::ActionRequired) => "action_required",
        Some(super::types::CiRunConclusion::Failure) => {
            if is_infra_failure(run) {
                "infra"
            } else {
                "test_fail"
            }
        }
        _ => "unknown",
    }
}

pub(crate) fn is_infra_failure(run: &CiRun) -> bool {
    const INFRA_HINTS: &[&str] = &[
        "runner",
        "infrastructure",
        "resource exhausted",
        "no space",
        "disk",
        "network",
        "connection",
        "service unavailable",
        "timed out waiting",
        "oom",
        "out of memory",
    ];
    let contains_hint = |value: &str| {
        let lowered = value.to_lowercase();
        INFRA_HINTS.iter().any(|hint| lowered.contains(hint))
    };

    run.jobs.as_deref().unwrap_or(&[]).iter().any(|job| {
        matches!(job_status_label(job), "failure" | "timed_out")
            && (contains_hint(&job.name)
                || job
                    .steps
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .any(|step| contains_hint(&step.name)))
    })
}

pub(crate) fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

pub(crate) fn derive_repo_base_from_run_url(run_url: &str) -> Option<String> {
    let parts = run_url.split('/').collect::<Vec<_>>();
    if parts.len() < 5 {
        return None;
    }
    Some(format!(
        "{}//{}/{}/{}",
        parts[0], parts[2], parts[3], parts[4]
    ))
}

pub(crate) fn extract_repo_slug_from_url(url: &str) -> Option<String> {
    let parts = url.split('/').collect::<Vec<_>>();
    if parts.len() < 5 {
        return None;
    }
    let owner = parts[3].trim();
    let repo = parts[4].trim();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{}/{}", owner.to_lowercase(), repo.to_lowercase()))
}

pub(crate) fn derive_pr_url(
    run: &CiRun,
    status_seed: &GhMonitorStatus,
    gh_request: &GhMonitorRequest,
) -> Option<String> {
    if let Some(url) = run
        .pull_requests
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .find_map(|pr| pr.url.clone())
    {
        return Some(url);
    }
    if matches!(gh_request.target_kind, GhMonitorTargetKind::Pr)
        && let Some(repo_base) = derive_repo_base_from_run_url(&run.url)
    {
        return Some(format!("{repo_base}/pull/{}", status_seed.target.trim()));
    }
    None
}

fn step_concluded_failed(step: &CiStep) -> bool {
    matches!(
        step.conclusion,
        Some(super::types::CiRunConclusion::Failure | super::types::CiRunConclusion::TimedOut)
    )
}

fn ci_conclusion_label(conclusion: super::types::CiRunConclusion) -> &'static str {
    match conclusion {
        super::types::CiRunConclusion::Success => "success",
        super::types::CiRunConclusion::Failure => "failure",
        super::types::CiRunConclusion::Cancelled => "cancelled",
        super::types::CiRunConclusion::Skipped => "skipped",
        super::types::CiRunConclusion::TimedOut => "timed_out",
        super::types::CiRunConclusion::ActionRequired => "action_required",
        super::types::CiRunConclusion::Neutral => "neutral",
        super::types::CiRunConclusion::Stale => "stale",
    }
}
