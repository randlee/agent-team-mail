//! GitHub-specific gh_monitor provider logic.

use super::CiMonitorConfig;
use super::helpers::{normalize_repo_scope, upsert_gh_monitor_status};
use agent_team_mail_core::daemon_client::{GhMonitorRequest, GhMonitorStatus, GhMonitorTargetKind};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::schema::InboxMessage;
use anyhow::Result;
use tracing::warn;

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhRunView {
    pub(crate) database_id: u64,
    pub(crate) name: String,
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) conclusion: Option<String>,
    pub(crate) head_branch: String,
    pub(crate) head_sha: String,
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) jobs: Vec<GhRunJob>,
    #[serde(default)]
    pub(crate) attempt: Option<u64>,
    #[serde(default)]
    pub(crate) pull_requests: Vec<GhPullRequest>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhRunJob {
    pub(crate) database_id: u64,
    pub(crate) name: String,
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) conclusion: Option<String>,
    #[serde(default)]
    pub(crate) started_at: Option<String>,
    #[serde(default)]
    pub(crate) completed_at: Option<String>,
    #[serde(default)]
    pub(crate) steps: Vec<GhRunStep>,
    #[serde(default)]
    pub(crate) url: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhRunStep {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) status: Option<String>,
    #[serde(default)]
    pub(crate) conclusion: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhPullRequest {
    #[serde(default)]
    pub(crate) url: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhPrLookupView {
    #[serde(default)]
    pub(crate) head_ref_name: Option<String>,
    #[serde(default)]
    pub(crate) head_ref_oid: Option<String>,
    #[serde(default)]
    pub(crate) created_at: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhPrView {
    #[serde(default)]
    pub(crate) merge_state_status: Option<String>,
    #[serde(default)]
    pub(crate) url: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhRunListEntry {
    #[serde(default)]
    pub(crate) database_id: Option<u64>,
    #[serde(default)]
    pub(crate) head_sha: Option<String>,
    #[serde(default)]
    pub(crate) created_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GhRunTerminalState {
    Success,
    Failure,
    TimedOut,
    Cancelled,
    ActionRequired,
    Other,
}

#[cfg(unix)]
pub(crate) async fn wait_for_pr_run_start(
    owner_repo: &str,
    pr_number: u64,
    timeout_secs: u64,
) -> Result<Option<u64>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        if let Some(run_id) = try_find_pr_run_id(owner_repo, pr_number).await? {
            return Ok(Some(run_id));
        }

        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        let remaining = deadline.saturating_duration_since(now);
        let sleep_for = remaining.min(std::time::Duration::from_secs(5));
        tokio::time::sleep(sleep_for).await;
    }
}

#[cfg(unix)]
pub(crate) async fn try_find_pr_run_id(owner_repo: &str, pr_number: u64) -> Result<Option<u64>> {
    let output = run_gh_command_for_repo(
        owner_repo,
        &[
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "headRefName,headRefOid,createdAt",
        ],
    )
    .await?;
    let pr_view = serde_json::from_str::<GhPrLookupView>(&output)?;
    let branch = pr_view
        .head_ref_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let pr_head_sha = pr_view
        .head_ref_oid
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let pr_created_at = pr_view
        .created_at
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let Some(branch) = branch else {
        return Ok(None);
    };

    let output = run_gh_command_for_repo(
        owner_repo,
        &[
            "run",
            "list",
            "--branch",
            &branch,
            "--limit",
            "20",
            "--json",
            "databaseId,headSha,createdAt",
        ],
    )
    .await?;
    let runs = serde_json::from_str::<Vec<GhRunListEntry>>(&output)?;
    for run in runs {
        let Some(run_id) = run.database_id else {
            continue;
        };

        if let Some(expected_head_sha) = pr_head_sha.as_deref()
            && run
                .head_sha
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                != Some(expected_head_sha)
        {
            continue;
        }

        if !run_passes_pr_recency_gate(run.created_at.as_deref(), pr_created_at.as_deref()) {
            continue;
        }

        return Ok(Some(run_id));
    }

    Ok(None)
}

#[cfg(unix)]
pub(crate) fn run_passes_pr_recency_gate(
    run_created_at: Option<&str>,
    pr_created_at: Option<&str>,
) -> bool {
    let Some(pr_created_at) = pr_created_at else {
        return true;
    };
    let Some(run_created_at) = run_created_at else {
        return true;
    };

    let parse_ts = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok();
    let Some(pr_ts) = parse_ts(pr_created_at) else {
        return true;
    };
    let Some(run_ts) = parse_ts(run_created_at) else {
        return true;
    };

    run_ts >= pr_ts
}

#[cfg(unix)]
pub(crate) async fn fetch_pr_merge_state(
    owner_repo: &str,
    pr_number: u64,
) -> Result<Option<GhPrView>> {
    let output = run_gh_command_for_repo(
        owner_repo,
        &[
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "mergeStateStatus,url",
        ],
    )
    .await?;
    let pr = serde_json::from_str::<GhPrView>(&output)?;
    if pr
        .merge_state_status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some(pr))
}

#[cfg(unix)]
pub(crate) fn is_pr_merge_state_dirty(merge_state_status: &str) -> bool {
    merge_state_status.trim().eq_ignore_ascii_case("dirty")
}

#[cfg(unix)]
pub(crate) async fn try_find_workflow_run_id(
    owner_repo: &str,
    workflow: &str,
    reference: &str,
) -> Result<Option<u64>> {
    let output = run_gh_command_for_repo(
        owner_repo,
        &[
            "run",
            "list",
            "--workflow",
            workflow,
            "--limit",
            "20",
            "--json",
            "databaseId,headBranch,headSha",
        ],
    )
    .await?;
    let runs = serde_json::from_str::<Vec<serde_json::Value>>(&output)?;

    for run in runs {
        let branch = run.get("headBranch").and_then(|v| v.as_str());
        let sha = run.get("headSha").and_then(|v| v.as_str());
        let matches_ref =
            branch == Some(reference) || sha.is_some_and(|s| s.starts_with(reference));
        if matches_ref && let Some(run_id) = run.get("databaseId").and_then(|v| v.as_u64()) {
            return Ok(Some(run_id));
        }
    }

    Ok(None)
}

#[cfg(unix)]
pub(crate) async fn run_gh_command(args: &[&str]) -> Result<String> {
    let output = tokio::process::Command::new("gh")
        .args(args)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gh {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(unix)]
pub(crate) async fn run_gh_command_for_repo(owner_repo: &str, args: &[&str]) -> Result<String> {
    let owner_repo = owner_repo.trim();
    if owner_repo.is_empty() {
        anyhow::bail!("missing owner/repo scope for gh command");
    }

    let mut command_args: Vec<&str> = Vec::with_capacity(args.len() + 2);
    command_args.push("-R");
    command_args.push(owner_repo);
    command_args.extend_from_slice(args);
    run_gh_command(&command_args).await
}

#[cfg(unix)]
pub(crate) async fn monitor_gh_run(
    home: &std::path::Path,
    status_seed: &GhMonitorStatus,
    gh_request: &GhMonitorRequest,
    owner_repo: &str,
    run_id: u64,
) -> Result<()> {
    let mut seen_completed: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut pending_completed: Vec<GhRunJob> = Vec::new();
    let mut last_progress_emit: Option<std::time::Instant> = None;
    let mut first_poll = true;

    loop {
        let run = fetch_run_view(owner_repo, run_id).await?;
        let expected_repo = extract_repo_slug_from_url(&run.url);
        let (from_agent, targets) = resolve_ci_alert_routing(
            home,
            &status_seed.team,
            gh_request.config_cwd.as_deref(),
            expected_repo.as_deref(),
        );
        let completed_jobs: Vec<GhRunJob> = run
            .jobs
            .iter()
            .filter(|job| is_job_completed(job))
            .cloned()
            .collect();
        for job in completed_jobs {
            if seen_completed.insert(job.database_id) {
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
                    run.database_id,
                    count_completed_jobs(&run),
                    run.jobs.len()
                );
                emit_ci_monitor_message(
                    home,
                    &from_agent,
                    &targets,
                    &summary,
                    &message,
                    Some(format!(
                        "ci-progress-{}-{}",
                        run.database_id,
                        uuid::Uuid::new_v4()
                    )),
                );
                pending_completed.clear();
                last_progress_emit = Some(now);
            }

            let mut state = status_seed.clone();
            state.run_id = Some(run.database_id);
            state.state = "monitoring".to_string();
            state.updated_at = chrono::Utc::now().to_rfc3339();
            state.message = Some(format!(
                "Run {} still in progress ({}/{})",
                run.database_id,
                count_completed_jobs(&run),
                run.jobs.len()
            ));
            upsert_gh_monitor_status(home, state)?;

            let sleep_secs = if first_poll { 5 } else { 15 };
            first_poll = false;
            tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
            continue;
        }

        let terminal = terminal.unwrap_or(GhRunTerminalState::Other);
        let summary_table = format_summary_table(&run);
        let mut message = format!(
            "CI monitor terminal update\nRun: {}\nWorkflow: {}\nState: {}\nURL: {}\n\n{}\n",
            run.database_id,
            run.name,
            terminal_state_label(terminal),
            run.url,
            summary_table
        );

        if terminal != GhRunTerminalState::Success {
            let correlation_id = format!("ci-failure-{}-{}", run.database_id, uuid::Uuid::new_v4());
            let failure_payload =
                build_failure_payload(&run, status_seed, gh_request, owner_repo, &correlation_id)
                    .await;
            message.push_str("\nFailure details:\n");
            message.push_str(&failure_payload);
        }

        let summary = format!(
            "ci terminal: run {} {}",
            run.database_id,
            terminal_state_label(terminal)
        );
        emit_ci_monitor_message(
            home,
            &from_agent,
            &targets,
            &summary,
            &message,
            Some(format!(
                "ci-terminal-{}-{}",
                run.database_id,
                uuid::Uuid::new_v4()
            )),
        );

        let mut state = status_seed.clone();
        state.run_id = Some(run.database_id);
        state.state = terminal_state_label(terminal)
            .to_lowercase()
            .replace(' ', "_");
        state.updated_at = chrono::Utc::now().to_rfc3339();
        state.message = Some(format!(
            "Terminal: {} ({}/{})",
            terminal_state_label(terminal),
            count_completed_jobs(&run),
            run.jobs.len()
        ));
        upsert_gh_monitor_status(home, state)?;

        if matches!(gh_request.target_kind, GhMonitorTargetKind::Pr)
            && let Ok(pr_number) = status_seed.target.trim().parse::<u64>()
        {
            match fetch_pr_merge_state(owner_repo, pr_number).await {
                Ok(Some(pr_view)) => {
                    if let Some(merge_state_status) = pr_view.merge_state_status.as_deref()
                        && is_pr_merge_state_dirty(merge_state_status)
                    {
                        emit_merge_conflict_alert(
                            home,
                            status_seed,
                            pr_view.url.as_deref(),
                            merge_state_status,
                            run.conclusion.as_deref(),
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

#[cfg(unix)]
pub(crate) async fn fetch_run_view(owner_repo: &str, run_id: u64) -> Result<GhRunView> {
    let output = run_gh_command_for_repo(
        owner_repo,
        &[
            "run",
            "view",
            &run_id.to_string(),
            "--json",
            "databaseId,name,status,conclusion,headBranch,headSha,url,jobs,attempt,pullRequests",
        ],
    )
    .await?;
    Ok(serde_json::from_str::<GhRunView>(&output)?)
}

#[cfg(unix)]
pub(crate) fn should_emit_progress(
    last_progress_emit: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    match last_progress_emit {
        None => true,
        Some(prev) => now.duration_since(prev) >= std::time::Duration::from_secs(60),
    }
}

#[cfg(unix)]
pub(crate) fn is_job_completed(job: &GhRunJob) -> bool {
    job.status.eq_ignore_ascii_case("completed")
}

#[cfg(unix)]
pub(crate) fn count_completed_jobs(run: &GhRunView) -> usize {
    run.jobs.iter().filter(|job| is_job_completed(job)).count()
}

#[cfg(unix)]
pub(crate) fn format_progress_message(run: &GhRunView, pending_completed: &[GhRunJob]) -> String {
    let new_jobs = pending_completed
        .iter()
        .map(|job| format!("{}({})", job.name, job_status_label(job)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "CI monitor progress\nRun: {}\nWorkflow: {}\nCompleted: {}/{}\nNewly completed: {}\nRun URL: {}",
        run.database_id,
        run.name,
        count_completed_jobs(run),
        run.jobs.len(),
        if new_jobs.is_empty() {
            "(none)"
        } else {
            &new_jobs
        },
        run.url
    )
}

#[cfg(unix)]
pub(crate) fn job_status_label(job: &GhRunJob) -> &'static str {
    match job
        .conclusion
        .as_deref()
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "success" => "success",
        "failure" => "failure",
        "timedout" | "timed_out" => "timed_out",
        "cancelled" => "cancelled",
        "actionrequired" | "action_required" => "action_required",
        _ => {
            if is_job_completed(job) {
                "completed"
            } else {
                "in_progress"
            }
        }
    }
}

#[cfg(unix)]
pub(crate) fn classify_terminal_state(run: &GhRunView) -> Option<GhRunTerminalState> {
    if !run.status.eq_ignore_ascii_case("completed") && run.conclusion.is_none() {
        return None;
    }
    Some(
        match run
            .conclusion
            .as_deref()
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "success" => GhRunTerminalState::Success,
            "failure" => GhRunTerminalState::Failure,
            "timedout" | "timed_out" => GhRunTerminalState::TimedOut,
            "cancelled" => GhRunTerminalState::Cancelled,
            "actionrequired" | "action_required" => GhRunTerminalState::ActionRequired,
            _ => GhRunTerminalState::Other,
        },
    )
}

#[cfg(unix)]
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

#[cfg(unix)]
pub(crate) fn format_summary_table(run: &GhRunView) -> String {
    let mut lines = Vec::new();
    lines.push("| Job/Test | Status | Runtime |".to_string());
    lines.push("|---|---|---|".to_string());
    for job in &run.jobs {
        lines.push(format!(
            "| {} | {} | {} |",
            job.name,
            job_status_label(job),
            format_job_runtime(job)
        ));
    }
    lines.join("\n")
}

#[cfg(unix)]
pub(crate) fn format_job_runtime(job: &GhRunJob) -> String {
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
    let duration = completed_dt.signed_duration_since(started_dt);
    let secs = duration.num_seconds().max(0);
    format!("{}m {}s", secs / 60, secs % 60)
}

#[cfg(unix)]
pub(crate) fn emit_ci_monitor_message(
    home: &std::path::Path,
    from_agent: &str,
    targets: &[(String, String)],
    summary: &str,
    text: &str,
    message_id: Option<String>,
) {
    for (agent, team) in targets {
        let inbox_path = home
            .join(".claude/teams")
            .join(team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        let message = InboxMessage {
            from: from_agent.to_string(),
            text: text.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(summary.to_string()),
            message_id: message_id.clone(),
            unknown_fields: std::collections::HashMap::new(),
        };
        if let Err(e) =
            agent_team_mail_core::io::inbox::inbox_append(&inbox_path, &message, team, agent)
        {
            warn!(
                team = %team,
                agent = %agent,
                "failed to emit ci monitor message: {e}"
            );
        }
    }
}

#[cfg(unix)]
pub(crate) async fn build_failure_payload(
    run: &GhRunView,
    status_seed: &GhMonitorStatus,
    gh_request: &GhMonitorRequest,
    owner_repo: &str,
    correlation_id: &str,
) -> String {
    let failed_jobs: Vec<&GhRunJob> = run
        .jobs
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
            job.url.clone().unwrap_or_else(|| {
                format!("{}/job/{}", run.url.trim_end_matches('/'), job.database_id)
            })
        })
        .collect::<Vec<_>>();
    let first_failing_step = failed_jobs
        .iter()
        .flat_map(|job| job.steps.iter())
        .find(|step| {
            let conclusion = step
                .conclusion
                .as_deref()
                .unwrap_or_default()
                .to_lowercase();
            let status = step.status.as_deref().unwrap_or_default().to_lowercase();
            conclusion == "failure"
                || conclusion == "timed_out"
                || conclusion == "timedout"
                || status == "failed"
        })
        .map(|step| step.name.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let failed_log_excerpt = if let Some(first_job) = failed_jobs.first() {
        fetch_failed_log_excerpt(owner_repo, first_job.database_id)
            .await
            .unwrap_or_else(|_| "(log excerpt unavailable)".to_string())
    } else {
        "(no failed jobs captured)".to_string()
    };

    let classification = classify_failure(run);
    let pr_url = derive_pr_url(run, status_seed, gh_request);
    let repo_base = derive_repo_base_from_run_url(&run.url).unwrap_or_default();
    let next_action = if failed_jobs.is_empty() {
        format!("atm gh status run {}", run.database_id)
    } else {
        format!("gh run view {} --log-failed", run.database_id)
    };
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
        run_id = run.database_id,
        attempt = run.attempt.unwrap_or(1),
        branch = run.head_branch,
        sha_short = short_sha(&run.head_sha),
        sha_full = run.head_sha,
        classification = classification,
        first_failing_step = first_failing_step,
        log_excerpt = failed_log_excerpt
            .replace('\n', " ")
            .chars()
            .take(240)
            .collect::<String>(),
        correlation_id = correlation_id,
        next_action = next_action,
        repo_base = repo_base,
    )
}

#[cfg(unix)]
pub(crate) async fn fetch_failed_log_excerpt(owner_repo: &str, job_id: u64) -> Result<String> {
    let output = run_gh_command_for_repo(
        owner_repo,
        &["run", "view", "--job", &job_id.to_string(), "--log"],
    )
    .await?;
    Ok(output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join(" | "))
}

#[cfg(unix)]
pub(crate) fn classify_failure(run: &GhRunView) -> &'static str {
    match run
        .conclusion
        .as_deref()
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "timedout" | "timed_out" => "timeout",
        "cancelled" => "cancelled",
        "actionrequired" | "action_required" => "action_required",
        "failure" => {
            if is_infra_failure(run) {
                "infra"
            } else {
                "test_fail"
            }
        }
        _ => "unknown",
    }
}

#[cfg(unix)]
pub(crate) fn is_infra_failure(run: &GhRunView) -> bool {
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

    let contains_infra_hint = |value: &str| {
        let lowered = value.to_lowercase();
        INFRA_HINTS.iter().any(|hint| lowered.contains(hint))
    };

    run.jobs.iter().any(|job| {
        let failed = matches!(job_status_label(job), "failure" | "timed_out");
        failed
            && (contains_infra_hint(&job.name)
                || job.steps.iter().any(|step| contains_infra_hint(&step.name)))
    })
}

#[cfg(unix)]
pub(crate) fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect::<String>()
}

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
pub(crate) fn derive_pr_url(
    run: &GhRunView,
    status_seed: &GhMonitorStatus,
    gh_request: &GhMonitorRequest,
) -> Option<String> {
    if let Some(url) = run.pull_requests.iter().find_map(|pr| pr.url.clone()) {
        return Some(url);
    }
    if matches!(gh_request.target_kind, GhMonitorTargetKind::Pr)
        && let Some(repo_base) = derive_repo_base_from_run_url(&run.url)
    {
        return Some(format!("{}/pull/{}", repo_base, status_seed.target.trim()));
    }
    None
}

#[cfg(unix)]
pub(crate) fn emit_ci_not_started_alert(
    home: &std::path::Path,
    status: &GhMonitorStatus,
    config_cwd: Option<&str>,
) {
    let (from_agent, targets) = resolve_ci_alert_routing(home, &status.team, config_cwd, None);
    let text = format!(
        "[ci_not_started] {} target '{}' did not produce a run in the start window.\n{}",
        match status.target_kind {
            GhMonitorTargetKind::Pr => "PR monitor",
            GhMonitorTargetKind::Workflow => "workflow monitor",
            GhMonitorTargetKind::Run => "run monitor",
        },
        status.target,
        status.message.clone().unwrap_or_default()
    );
    let summary = format!("ci_not_started: {}", status.target);
    for (agent, team) in targets {
        let inbox_path = home
            .join(".claude/teams")
            .join(&team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        let message = InboxMessage {
            from: from_agent.clone(),
            text: text.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(summary.clone()),
            message_id: Some(uuid::Uuid::new_v4().to_string()),
            unknown_fields: std::collections::HashMap::new(),
        };
        if let Err(e) =
            agent_team_mail_core::io::inbox::inbox_append(&inbox_path, &message, &team, &agent)
        {
            warn!(
                team = %team,
                agent = %agent,
                "failed to emit ci_not_started alert: {e}"
            );
        }
    }
}

#[cfg(unix)]
pub(crate) fn emit_merge_conflict_alert(
    home: &std::path::Path,
    status: &GhMonitorStatus,
    pr_url: Option<&str>,
    merge_state_status: &str,
    run_conclusion: Option<&str>,
    config_cwd: Option<&str>,
) {
    let expected_repo = pr_url.and_then(extract_repo_slug_from_url);
    let (from_agent, targets) =
        resolve_ci_alert_routing(home, &status.team, config_cwd, expected_repo.as_deref());
    let target_kind = match status.target_kind {
        GhMonitorTargetKind::Pr => "pr",
        GhMonitorTargetKind::Workflow => "workflow",
        GhMonitorTargetKind::Run => "run",
    };
    let mut text = format!(
        "[merge_conflict] Merge conflict detected for monitored target.\nclassification: merge_conflict\nstatus: merge_conflict\ntarget_kind: {target_kind}\ntarget: {}\npr_url: {}\nmerge_state_status: {}",
        status.target,
        pr_url.unwrap_or("(unknown)"),
        merge_state_status
    );
    if let Some(run_conclusion) = run_conclusion {
        text.push_str(&format!("\nrun_conclusion: {run_conclusion}"));
    }
    if let Some(message) = status.message.as_deref()
        && !message.trim().is_empty()
    {
        text.push_str(&format!("\nreason: {message}"));
    }

    let summary = format!("merge_conflict: {}", status.target);
    let mut extra_fields = serde_json::Map::new();
    extra_fields.insert(
        "classification".to_string(),
        serde_json::Value::String("merge_conflict".to_string()),
    );
    extra_fields.insert(
        "status".to_string(),
        serde_json::Value::String("merge_conflict".to_string()),
    );
    extra_fields.insert(
        "target_kind".to_string(),
        serde_json::Value::String(target_kind.to_string()),
    );
    extra_fields.insert(
        "pr_url".to_string(),
        serde_json::Value::String(pr_url.unwrap_or("(unknown)").to_string()),
    );
    extra_fields.insert(
        "merge_state_status".to_string(),
        serde_json::Value::String(merge_state_status.to_string()),
    );
    if let Some(run_conclusion) = run_conclusion {
        extra_fields.insert(
            "run_conclusion".to_string(),
            serde_json::Value::String(run_conclusion.to_string()),
        );
    }
    emit_event_best_effort(EventFields {
        level: "warn",
        source: "atm-daemon",
        action: "gh_monitor_merge_conflict",
        team: Some(status.team.clone()),
        target: Some(status.target.clone()),
        result: Some("merge_conflict".to_string()),
        error: Some(format!(
            "merge_state_status={}",
            merge_state_status.trim().to_uppercase()
        )),
        extra_fields,
        ..Default::default()
    });

    for (agent, team) in targets {
        let inbox_path = home
            .join(".claude/teams")
            .join(&team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        let message = InboxMessage {
            from: from_agent.clone(),
            text: text.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(summary.clone()),
            message_id: Some(uuid::Uuid::new_v4().to_string()),
            unknown_fields: std::collections::HashMap::new(),
        };
        if let Err(e) =
            agent_team_mail_core::io::inbox::inbox_append(&inbox_path, &message, &team, &agent)
        {
            warn!(
                team = %team,
                agent = %agent,
                "failed to emit merge_conflict alert: {e}"
            );
        }
    }
}

#[cfg(unix)]
pub(crate) fn resolve_ci_alert_routing(
    home: &std::path::Path,
    team: &str,
    config_cwd: Option<&str>,
    expected_repo_slug: Option<&str>,
) -> (String, Vec<(String, String)>) {
    let current_dir = config_cwd
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.to_path_buf());
    let config = match agent_team_mail_core::config::resolve_config(
        &agent_team_mail_core::config::ConfigOverrides {
            team: Some(team.to_string()),
            ..Default::default()
        },
        &current_dir,
        home,
    ) {
        Ok(cfg) => cfg,
        Err(_) => {
            return (
                "gh-monitor".to_string(),
                vec![("team-lead".to_string(), team.to_string())],
            );
        }
    };

    let plugin_table = config.plugin_config("gh_monitor");
    let Some(plugin_table) = plugin_table else {
        return (
            "gh-monitor".to_string(),
            vec![("team-lead".to_string(), team.to_string())],
        );
    };

    let parsed = match CiMonitorConfig::from_toml(plugin_table) {
        Ok(cfg) => cfg,
        Err(_) => {
            return (
                "gh-monitor".to_string(),
                vec![("team-lead".to_string(), team.to_string())],
            );
        }
    };

    let from_agent = if parsed.agent.trim().is_empty() {
        "gh-monitor".to_string()
    } else {
        parsed.agent
    };

    if parsed.team.trim() != team {
        warn!(
            expected_team = %team,
            configured_team = %parsed.team,
            "gh monitor routing blocked: configured team does not match request team"
        );
        return (from_agent, Vec::new());
    }

    if let Some(expected) = expected_repo_slug
        && !expected.trim().is_empty()
    {
        match normalize_repo_scope(parsed.owner.as_deref(), parsed.repo.as_deref()) {
            Some(configured) if !repo_scope_matches(&configured, expected) => {
                warn!(
                    expected_repo = %expected,
                    configured_repo = %configured,
                    "gh monitor routing blocked: configured repo does not match event repo"
                );
                return (from_agent, Vec::new());
            }
            None => {
                warn!(
                    expected_repo = %expected,
                    "gh monitor routing blocked: configured repo scope unavailable"
                );
                return (from_agent, Vec::new());
            }
            _ => {}
        }
    }

    let targets = if parsed.notify_target.is_empty() {
        vec![("team-lead".to_string(), parsed.team.clone())]
    } else {
        parsed
            .notify_target
            .into_iter()
            .map(|t| (t.agent, parsed.team.clone()))
            .collect()
    };
    (from_agent, targets)
}

#[cfg(unix)]
pub(crate) fn repo_scope_matches(configured: &str, expected: &str) -> bool {
    let configured = configured.trim().to_lowercase();
    let expected = expected.trim().to_lowercase();
    if configured == expected {
        return true;
    }
    if configured.contains('/') {
        return false;
    }
    expected
        .split_once('/')
        .map(|(_, repo)| repo == configured)
        .unwrap_or(false)
}
