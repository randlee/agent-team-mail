//! Transport-free CI monitor service orchestration.
//!
//! This module forms the core CI monitor boundary. It must not depend on daemon
//! socket/router request or response types; daemon transport adapters are responsible
//! for translating wire payloads into these CI monitor request/status types before
//! calling into the service layer.

#[cfg(unix)]
use super::gh_monitor::{
    fetch_pr_merge_state, is_pr_merge_state_dirty, poll_monitored_run_once, try_find_pr_run_id,
    try_find_workflow_run_id, wait_for_pr_run_start, RunPollProgress,
};
use super::github_provider::GitHubActionsProvider;
#[cfg(unix)]
use super::health::set_gh_monitor_health_state;
use super::helpers::{
    apply_config_state_to_status, count_in_flight_monitors, evaluate_gh_monitor_config,
    gh_monitor_key, load_gh_monitor_state_map, load_gh_monitor_state_records,
};
use super::provider::ErasedCiProvider;
use super::registry::CiProviderRegistryPort;
#[cfg(unix)]
use super::routing::{notify_ci_not_started, notify_merge_conflict};
use super::types::{
    CiMonitorControlRequest, CiMonitorHealth, CiMonitorLifecycleAction, CiMonitorRequest,
    CiMonitorStatus, CiMonitorStatusRequest, CiMonitorTargetKind, GhAlertTargets,
    GhMonitorConfigState, GhMonitorHealthUpdate,
};
use agent_team_mail_core::context::GitProvider as GitProviderType;
use agent_team_mail_core::gh_monitor_observability::{
    GhCliObserverContext, build_gh_cli_observer, update_gh_repo_state_in_flight,
};
#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::sync::{Mutex, OnceLock};
#[cfg(unix)]
use tokio::task::JoinHandle;
use tracing::warn;

pub(crate) use agent_team_mail_ci_monitor::service::{
    CiMonitorServiceError, CiMonitorServiceResult, fetch_run_details, list_completed_runs,
};

#[cfg(unix)]
fn shared_pollers() -> &'static Mutex<HashMap<String, JoinHandle<()>>> {
    static SHARED_POLLERS: OnceLock<Mutex<HashMap<String, JoinHandle<()>>>> = OnceLock::new();
    SHARED_POLLERS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(unix)]
fn shared_poller_key(team: &str, repo: &str) -> String {
    format!("{}|{}", team.trim(), repo.trim().to_ascii_lowercase())
}

#[cfg(unix)]
fn status_is_terminal(state: &str) -> bool {
    matches!(
        state.trim().to_ascii_lowercase().as_str(),
        "success" | "failure" | "timed_out" | "cancelled" | "action_required" | "unknown"
    )
}

#[cfg(unix)]
fn status_has_active_subscription(status: &CiMonitorStatus) -> bool {
    !status_is_terminal(&status.state) && status.state != "ci_not_started"
}

#[cfg(unix)]
async fn refresh_shared_repo_state(
    home: &std::path::Path,
    team: &str,
    owner_repo: &str,
) -> anyhow::Result<()> {
    let (owner, repo) = owner_repo
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("invalid owner/repo scope for gh command: {owner_repo}"))?;
    let observer = build_gh_cli_observer(GhCliObserverContext {
        home: home.to_path_buf(),
        team: team.to_string(),
        repo: owner_repo.to_string(),
        runtime: "atm-daemon".to_string(),
    });
    let provider =
        GitHubActionsProvider::new(owner.to_string(), repo.to_string()).with_observer(observer);
    let repo_scope = owner_repo.trim().to_string();
    let _ = provider
        .run_gh(
            "gh_pr_list",
            &[
                "-R",
                repo_scope.as_str(),
                "pr",
                "list",
                "--state",
                "open",
                "--limit",
                "100",
                "--json",
                "number,title,url,isDraft,reviewDecision,mergeStateStatus,statusCheckRollup",
            ],
            None,
            None,
        )
        .await
        .map_err(anyhow::Error::msg)?;
    Ok(())
}

#[cfg(unix)]
async fn poll_status_once(
    home: &std::path::Path,
    owner_repo: &str,
    repo_scope: Option<&str>,
    record: &super::types::GhMonitorStateRecord,
) -> anyhow::Result<()> {
    let mut status = record.status.clone();
    match status.target_kind {
        CiMonitorTargetKind::Run => {
            if let Some(run_id) = status.run_id {
                let request = CiMonitorRequest {
                    team: status.team.clone(),
                    target_kind: status.target_kind,
                    target: status.target.clone(),
                    reference: status.reference.clone(),
                    start_timeout_secs: None,
                    config_cwd: None,
                };
                let mut progress = RunPollProgress::default();
                let _ = poll_monitored_run_once(
                    home,
                    &status,
                    &request,
                    owner_repo,
                    run_id,
                    GhAlertTargets::default(),
                    &mut progress,
                )
                .await?;
            }
        }
        CiMonitorTargetKind::Pr => {
            if status.run_id.is_none()
                && let Ok(pr_number) = status.target.trim().parse::<u64>()
                && let Some(run_id) =
                    try_find_pr_run_id(home, &status.team, owner_repo, pr_number).await?
            {
                status.run_id = Some(run_id);
                status.state = "monitoring".to_string();
                status.updated_at = chrono::Utc::now().to_rfc3339();
                super::helpers::upsert_gh_monitor_status_for_repo(
                    home,
                    status.clone(),
                    repo_scope,
                )?;
            }
            if let Some(run_id) = status.run_id {
                let request = CiMonitorRequest {
                    team: status.team.clone(),
                    target_kind: status.target_kind,
                    target: status.target.clone(),
                    reference: status.reference.clone(),
                    start_timeout_secs: None,
                    config_cwd: None,
                };
                let mut progress = RunPollProgress::default();
                let _ = poll_monitored_run_once(
                    home,
                    &status,
                    &request,
                    owner_repo,
                    run_id,
                    GhAlertTargets::default(),
                    &mut progress,
                )
                .await?;
            }
        }
        CiMonitorTargetKind::Workflow => {
            if status.run_id.is_none()
                && let Some(reference) = status.reference.as_deref()
                && let Some(run_id) = try_find_workflow_run_id(
                    home,
                    &status.team,
                    owner_repo,
                    &status.target,
                    reference,
                )
                .await?
            {
                status.run_id = Some(run_id);
                status.state = "monitoring".to_string();
                status.updated_at = chrono::Utc::now().to_rfc3339();
                super::helpers::upsert_gh_monitor_status_for_repo(
                    home,
                    status.clone(),
                    repo_scope,
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn run_shared_repo_poller(home: std::path::PathBuf, team: String, owner_repo: String) {
    let repo_scope = Some(owner_repo.as_str());
    loop {
        let records = match load_gh_monitor_state_records(&home) {
            Ok(records) => records,
            Err(err) => {
                warn!(team = %team, repo = %owner_repo, "failed to load gh monitor state: {err}");
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                continue;
            }
        };
        let scoped_records: Vec<_> = records
            .into_iter()
            .filter(|record| {
                record.status.team == team
                    && record
                        .repo_scope
                        .as_deref()
                        .map(|value| value.eq_ignore_ascii_case(&owner_repo))
                        .unwrap_or(false)
            })
            .collect();
        let active_records: Vec<_> = scoped_records
            .iter()
            .filter(|record| status_has_active_subscription(&record.status))
            .cloned()
            .collect();

        let in_flight = active_records.len() as u64;
        let _ = update_gh_repo_state_in_flight(&home, &team, &owner_repo, in_flight, "atm-daemon");
        if let Err(err) = refresh_shared_repo_state(&home, &team, &owner_repo).await {
            warn!(team = %team, repo = %owner_repo, "shared repo-state refresh failed: {err}");
        }

        for record in &active_records {
            if let Err(err) = poll_status_once(&home, &owner_repo, repo_scope, record).await {
                warn!(
                    team = %record.status.team,
                    target = %record.status.target,
                    repo = %owner_repo,
                    "shared gh monitor poll failed: {err}"
                );
            }
        }

        let _ = update_gh_repo_state_in_flight(&home, &team, &owner_repo, 0, "atm-daemon");
        let sleep_secs = if active_records.is_empty() { 300 } else { 60 };
        tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
    }
}

#[cfg(unix)]
fn ensure_shared_repo_poller(home: &std::path::Path, team: &str, owner_repo: &str) {
    let key = shared_poller_key(team, owner_repo);
    let mut pollers = shared_pollers().lock().expect("shared poller mutex");
    if pollers
        .get(&key)
        .is_some_and(|handle| !handle.is_finished())
    {
        return;
    }
    pollers.remove(&key);
    let home = home.to_path_buf();
    let team = team.to_string();
    let owner_repo = owner_repo.to_string();
    let key_for_task = key.clone();
    let handle = tokio::spawn(async move {
        run_shared_repo_poller(home, team, owner_repo).await;
        if let Ok(mut pollers) = shared_pollers().lock() {
            pollers.remove(&key_for_task);
        }
    });
    pollers.insert(key, handle);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn create_provider_from_registry(
    home: &std::path::Path,
    team: &str,
    registry: &dyn CiProviderRegistryPort,
    provider_name: &str,
    configured_owner: Option<&str>,
    configured_repo: Option<&str>,
    git_provider: Option<&GitProviderType>,
    config_table: Option<&toml::Table>,
) -> CiMonitorServiceResult<Box<dyn ErasedCiProvider>> {
    let (owner, repo) = if let Some(git_provider) = git_provider {
        match git_provider {
            GitProviderType::GitHub { owner, repo } => (owner.clone(), repo.clone()),
            GitProviderType::AzureDevOps { org, project, repo } => {
                return Err(CiMonitorServiceError::new(
                    "PROVIDER_ERROR",
                    format!(
                        "Azure DevOps not yet supported (org: {org}, project: {project}, repo: {repo})"
                    ),
                ));
            }
            GitProviderType::GitLab { namespace, repo } => {
                return Err(CiMonitorServiceError::new(
                    "PROVIDER_ERROR",
                    format!("GitLab not yet supported (namespace: {namespace}, repo: {repo})"),
                ));
            }
            GitProviderType::Bitbucket { workspace, repo } => {
                return Err(CiMonitorServiceError::new(
                    "PROVIDER_ERROR",
                    format!("Bitbucket not yet supported (workspace: {workspace}, repo: {repo})"),
                ));
            }
            GitProviderType::Unknown { host } => {
                return Err(CiMonitorServiceError::new(
                    "PROVIDER_ERROR",
                    format!("No CI provider for unknown git host: {host}"),
                ));
            }
        }
    } else if let (Some(owner), Some(repo)) = (configured_owner, configured_repo) {
        (owner.to_string(), repo.to_string())
    } else {
        return Err(CiMonitorServiceError::new(
            "PROVIDER_ERROR",
            "No repository information available",
        ));
    };

    if provider_name == "github" {
        let observer = build_gh_cli_observer(GhCliObserverContext {
            home: home.to_path_buf(),
            team: team.to_string(),
            repo: format!("{owner}/{repo}"),
            runtime: "atm-daemon".to_string(),
        });
        return Ok(Box::new(
            GitHubActionsProvider::new(owner, repo).with_observer(observer),
        ));
    }

    registry
        .create_provider(provider_name, config_table)
        .map_err(|e| CiMonitorServiceError::new("PROVIDER_ERROR", e.to_string()))
}

#[cfg(unix)]
fn validate_monitor_request(
    gh_request: &CiMonitorRequest,
    config_state: &GhMonitorConfigState,
) -> std::result::Result<(), (&'static str, String, Option<String>)> {
    if let Some(reason) = config_state.error.clone() {
        return Err((
            "CONFIG_ERROR",
            format!("gh_monitor unavailable: {reason}"),
            Some(reason),
        ));
    }

    if config_state
        .configured_team
        .as_deref()
        .is_some_and(|configured_team| configured_team != gh_request.team)
    {
        let message = format!(
            "gh_monitor team mismatch: configured '{}' but request was '{}'",
            config_state.configured_team.as_deref().unwrap_or_default(),
            gh_request.team
        );
        return Err(("CONFIG_ERROR", message.clone(), Some(message)));
    }

    if config_state
        .owner_repo
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        let message =
            "gh_monitor unavailable: unable to resolve owner/repo for GitHub provider".to_string();
        return Err(("CONFIG_ERROR", message.clone(), Some(message)));
    }

    if matches!(gh_request.target_kind, CiMonitorTargetKind::Workflow)
        && gh_request
            .reference
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
    {
        return Err((
            "MISSING_PARAMETER",
            "Missing required payload field: 'reference' for workflow monitor".to_string(),
            None,
        ));
    }

    Ok(())
}

#[cfg(unix)]
pub(crate) async fn monitor_request(
    home: &std::path::Path,
    gh_request: &CiMonitorRequest,
    repo_scope: Option<&str>,
    caller_agent: Option<&str>,
    cc: &[String],
) -> CiMonitorServiceResult<CiMonitorStatus> {
    if gh_request.team.trim().is_empty() {
        return Err(CiMonitorServiceError::new(
            "MISSING_PARAMETER",
            "Missing required payload field: 'team'",
        ));
    }

    if gh_request.target.trim().is_empty() {
        return Err(CiMonitorServiceError::new(
            "MISSING_PARAMETER",
            "Missing required payload field: 'target'",
        ));
    }

    let current_health =
        super::health::read_gh_monitor_health(home, &gh_request.team).map_err(|e| {
            CiMonitorServiceError::internal(format!("Failed to read gh monitor health: {e}"))
        })?;
    if current_health.lifecycle_state != "running" {
        return Err(CiMonitorServiceError::new(
            "MONITOR_STOPPED",
            "gh monitor lifecycle is not running (run `atm gh monitor start` first)",
        ));
    }

    let config_state =
        evaluate_gh_monitor_config(home, &gh_request.team, gh_request.config_cwd.as_deref());

    if let Err((code, message, health_message)) =
        validate_monitor_request(gh_request, &config_state)
    {
        if let Some(reason) = health_message {
            let _ = set_gh_monitor_health_state(
                home,
                &gh_request.team,
                GhMonitorHealthUpdate {
                    availability_state: Some("disabled_config_error"),
                    in_flight: Some(count_in_flight_monitors(home, &gh_request.team)),
                    message: Some(reason),
                    config_state: Some(&config_state),
                    config_cwd: gh_request.config_cwd.as_deref(),
                    ..Default::default()
                },
            );
        }
        return Err(CiMonitorServiceError::new(code, message));
    }

    let owner_repo = repo_scope
        .filter(|value| !value.trim().is_empty())
        .or(config_state.owner_repo.as_deref())
        .unwrap_or_default();

    let now = chrono::Utc::now().to_rfc3339();
    let mut status = CiMonitorStatus {
        team: gh_request.team.clone(),
        configured: config_state.configured,
        enabled: config_state.enabled,
        config_source: config_state.config_source.clone(),
        config_path: config_state.config_path.clone(),
        target_kind: gh_request.target_kind,
        target: gh_request.target.clone(),
        state: "monitoring".to_string(),
        run_id: None,
        reference: gh_request.reference.clone(),
        updated_at: now,
        message: None,
        repo_state_updated_at: None,
    };

    let mut transient_failure: Option<String> = None;
    match gh_request.target_kind {
        CiMonitorTargetKind::Run => {
            status.run_id = gh_request.target.parse::<u64>().ok();
        }
        CiMonitorTargetKind::Workflow => {
            if let Some(reference) = gh_request.reference.as_deref() {
                match try_find_workflow_run_id(
                    home,
                    &gh_request.team,
                    owner_repo,
                    &gh_request.target,
                    reference,
                )
                .await
                {
                    Ok(Some(run_id)) => status.run_id = Some(run_id),
                    Ok(None) => {}
                    Err(e) => {
                        transient_failure = Some(format!("{e}"));
                        status.message = Some(format!(
                            "workflow run lookup unavailable; tracking without run id: {e}"
                        ));
                    }
                }
            }
        }
        CiMonitorTargetKind::Pr => {
            let pr_number = match gh_request.target.parse::<u64>() {
                Ok(value) if value > 0 => value,
                _ => {
                    return Err(CiMonitorServiceError::new(
                        "INVALID_PAYLOAD",
                        "PR target must be a positive integer",
                    ));
                }
            };
            let mut preflight_blocked = false;
            match fetch_pr_merge_state(home, &gh_request.team, owner_repo, pr_number).await {
                Ok(Some(pr_view)) => {
                    if let Some(merge_state_status) = pr_view.merge_state_status.as_deref()
                        && is_pr_merge_state_dirty(merge_state_status)
                    {
                        status.state = "merge_conflict".to_string();
                        status.message = Some(format!(
                            "PR #{pr_number} has mergeStateStatus={merge_state_status}; resolve conflicts before CI monitoring."
                        ));
                        notify_merge_conflict(
                            home,
                            &status,
                            pr_view.url.as_deref(),
                            merge_state_status,
                            None,
                            gh_request.config_cwd.as_deref(),
                            GhAlertTargets { caller_agent, cc },
                        );
                        preflight_blocked = true;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        team = %gh_request.team,
                        pr = pr_number,
                        "gh-monitor preflight mergeStateStatus lookup failed: {e}"
                    );
                }
            }

            if !preflight_blocked {
                let timeout_secs = gh_request.start_timeout_secs.unwrap_or(120);
                if timeout_secs == 0 {
                    status.state = "ci_not_started".to_string();
                    status.message =
                        Some("No workflow run observed before start-timeout (0s).".to_string());
                } else {
                    match wait_for_pr_run_start(
                        home,
                        &gh_request.team,
                        owner_repo,
                        pr_number,
                        timeout_secs,
                    )
                    .await
                    {
                        Ok(Some(run_id)) => {
                            status.run_id = Some(run_id);
                        }
                        Ok(None) => {
                            status.state = "ci_not_started".to_string();
                            status.message = Some(format!(
                                "No workflow run observed for PR #{pr_number} within {timeout_secs}s."
                            ));
                        }
                        Err(e) => {
                            transient_failure = Some(format!("{e}"));
                            status.state = "ci_not_started".to_string();
                            status.message = Some(format!(
                                "Unable to query workflow runs for PR #{pr_number}: {e}"
                            ));
                        }
                    }
                }
            }
        }
    }

    super::helpers::upsert_gh_monitor_status_for_repo(home, status.clone(), repo_scope).map_err(
        |e| {
            let _ = set_gh_monitor_health_state(
                home,
                &gh_request.team,
                GhMonitorHealthUpdate {
                    availability_state: Some("degraded"),
                    message: Some(format!("failed to persist monitor status: {e}")),
                    config_state: Some(&config_state),
                    config_cwd: gh_request.config_cwd.as_deref(),
                    ..Default::default()
                },
            );
            CiMonitorServiceError::internal(format!("Failed to persist gh monitor state: {e}"))
        },
    )?;

    if status.state == "ci_not_started" {
        notify_ci_not_started(
            home,
            &status,
            gh_request.config_cwd.as_deref(),
            repo_scope,
            GhAlertTargets { caller_agent, cc },
        );
    } else {
        ensure_shared_repo_poller(home, &gh_request.team, owner_repo);
    }

    if let Some(reason) = transient_failure {
        let _ = set_gh_monitor_health_state(
            home,
            &gh_request.team,
            GhMonitorHealthUpdate {
                availability_state: Some("degraded"),
                in_flight: Some(count_in_flight_monitors(home, &gh_request.team)),
                message: Some(format!("transient provider/gh failure: {reason}")),
                config_state: Some(&config_state),
                config_cwd: gh_request.config_cwd.as_deref(),
                ..Default::default()
            },
        );
    } else {
        let _ = set_gh_monitor_health_state(
            home,
            &gh_request.team,
            GhMonitorHealthUpdate {
                lifecycle_state: Some("running"),
                availability_state: Some("healthy"),
                in_flight: Some(count_in_flight_monitors(home, &gh_request.team)),
                message: Some("monitor request succeeded".to_string()),
                config_state: Some(&config_state),
                config_cwd: gh_request.config_cwd.as_deref(),
            },
        );
    }

    Ok(status)
}

#[cfg(unix)]
pub(crate) async fn control_request(
    home: &std::path::Path,
    control: &CiMonitorControlRequest,
) -> CiMonitorServiceResult<CiMonitorHealth> {
    if control.team.trim().is_empty() {
        return Err(CiMonitorServiceError::new(
            "MISSING_PARAMETER",
            "Missing required payload field: 'team'",
        ));
    }

    let config_state =
        evaluate_gh_monitor_config(home, &control.team, control.config_cwd.as_deref());

    let health = match control.action {
        CiMonitorLifecycleAction::Start => set_gh_monitor_health_state(
            home,
            &control.team,
            GhMonitorHealthUpdate {
                lifecycle_state: Some("running"),
                in_flight: Some(count_in_flight_monitors(home, &control.team)),
                message: Some("gh monitor lifecycle started".to_string()),
                config_state: Some(&config_state),
                config_cwd: control.config_cwd.as_deref(),
                ..Default::default()
            },
        )
        .map_err(|e| {
            CiMonitorServiceError::internal(format!(
                "failed to update monitor lifecycle state: {e}"
            ))
        })?,
        CiMonitorLifecycleAction::Stop => {
            let drain_timeout_secs = control.drain_timeout_secs.unwrap_or(30);
            let _ = set_gh_monitor_health_state(
                home,
                &control.team,
                GhMonitorHealthUpdate {
                    lifecycle_state: Some("draining"),
                    in_flight: Some(count_in_flight_monitors(home, &control.team)),
                    message: Some(format!(
                        "draining in-flight monitors (timeout={}s)",
                        drain_timeout_secs
                    )),
                    config_state: Some(&config_state),
                    config_cwd: control.config_cwd.as_deref(),
                    ..Default::default()
                },
            );

            let deadline = std::time::Instant::now()
                + std::time::Duration::from_secs(drain_timeout_secs.max(1));
            let mut in_flight = count_in_flight_monitors(home, &control.team);
            while in_flight > 0 && std::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                in_flight = count_in_flight_monitors(home, &control.team);
            }

            let message = if in_flight == 0 {
                "gh monitor lifecycle stopped after in-flight drain".to_string()
            } else {
                format!(
                    "drain timeout reached; stopped with {} in-flight monitor(s)",
                    in_flight
                )
            };
            set_gh_monitor_health_state(
                home,
                &control.team,
                GhMonitorHealthUpdate {
                    lifecycle_state: Some("stopped"),
                    in_flight: Some(in_flight),
                    message: Some(message),
                    config_state: Some(&config_state),
                    config_cwd: control.config_cwd.as_deref(),
                    ..Default::default()
                },
            )
            .map_err(|e| {
                CiMonitorServiceError::internal(format!("failed to stop monitor lifecycle: {e}"))
            })?
        }
        CiMonitorLifecycleAction::Restart => {
            let drain_timeout_secs = control.drain_timeout_secs.unwrap_or(30);
            let _ = set_gh_monitor_health_state(
                home,
                &control.team,
                GhMonitorHealthUpdate {
                    lifecycle_state: Some("draining"),
                    in_flight: Some(count_in_flight_monitors(home, &control.team)),
                    message: Some(format!(
                        "draining in-flight monitors before restart (timeout={}s)",
                        drain_timeout_secs
                    )),
                    config_state: Some(&config_state),
                    config_cwd: control.config_cwd.as_deref(),
                    ..Default::default()
                },
            );

            let deadline = std::time::Instant::now()
                + std::time::Duration::from_secs(drain_timeout_secs.max(1));
            let mut in_flight = count_in_flight_monitors(home, &control.team);
            while in_flight > 0 && std::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                in_flight = count_in_flight_monitors(home, &control.team);
            }

            let reloaded_config =
                evaluate_gh_monitor_config(home, &control.team, control.config_cwd.as_deref());
            if let Some(reason) = reloaded_config.error.as_deref() {
                let message = format!("gh monitor restart blocked: {reason}");
                let _ = set_gh_monitor_health_state(
                    home,
                    &control.team,
                    GhMonitorHealthUpdate {
                        lifecycle_state: Some("stopped"),
                        availability_state: Some("disabled_config_error"),
                        in_flight: Some(in_flight),
                        message: Some(message),
                        config_state: Some(&reloaded_config),
                        config_cwd: control.config_cwd.as_deref(),
                    },
                );
                return Err(CiMonitorServiceError::new(
                    "CONFIG_ERROR",
                    format!("gh_monitor unavailable after reload: {reason}"),
                ));
            }

            set_gh_monitor_health_state(
                home,
                &control.team,
                GhMonitorHealthUpdate {
                    lifecycle_state: Some("running"),
                    availability_state: Some("healthy"),
                    in_flight: Some(in_flight),
                    message: Some(if in_flight == 0 {
                        "gh monitor lifecycle restarted after in-flight drain".to_string()
                    } else {
                        format!(
                            "gh monitor lifecycle restarted after drain timeout; {} in-flight monitor(s) remain",
                            in_flight
                        )
                    }),
                    config_state: Some(&reloaded_config),
                    config_cwd: control.config_cwd.as_deref(),
                },
            )
            .map_err(|e| {
                CiMonitorServiceError::internal(format!("failed to restart monitor lifecycle: {e}"))
            })?
        }
    };

    Ok(health)
}

#[cfg(unix)]
pub(crate) fn health_request(
    home: &std::path::Path,
    team: &str,
    config_cwd: Option<&str>,
    repo_scope: Option<&str>,
) -> CiMonitorServiceResult<CiMonitorHealth> {
    if team.trim().is_empty() {
        return Err(CiMonitorServiceError::new(
            "MISSING_PARAMETER",
            "Missing required payload field: 'team'",
        ));
    }

    let config_state = evaluate_gh_monitor_config(home, team, config_cwd);
    let mut health = super::health::read_gh_monitor_health(home, team).map_err(|e| {
        CiMonitorServiceError::internal(format!("Failed to read gh monitor health: {e}"))
    })?;
    health.in_flight = count_in_flight_monitors(home, team);
    health.configured = config_state.configured;
    health.enabled = config_state.enabled;
    health.config_source = config_state.config_source.clone();
    health.config_path = config_state.config_path.clone();
    if let Some(reason) = config_state.error.as_deref() {
        health.availability_state = "disabled_config_error".to_string();
        health.message = Some(reason.to_string());
    }
    if let Some(repo_scope) = repo_scope.or(config_state.owner_repo.as_deref())
        && let Ok(Some(repo_state)) =
            agent_team_mail_core::gh_monitor_observability::read_gh_repo_state_record(
                home, team, repo_scope,
            )
    {
        health.repo_state_updated_at = Some(repo_state.updated_at.clone());
        health.budget_limit_per_hour = Some(repo_state.budget_limit_per_hour);
        health.budget_used_in_window = Some(repo_state.budget_used_in_window);
        health.rate_limit_remaining = repo_state.rate_limit.as_ref().map(|rate| rate.remaining);
        health.rate_limit_limit = repo_state.rate_limit.as_ref().map(|rate| rate.limit);
        health.poll_owner = repo_state.owner.as_ref().map(|owner| {
            format!(
                "{} pid={} runtime={} home={}",
                owner.executable_path, owner.pid, owner.runtime, owner.home_scope
            )
        });
    }
    Ok(health)
}

#[cfg(unix)]
pub(crate) fn status_request(
    home: &std::path::Path,
    gh_request: &CiMonitorStatusRequest,
    repo_scope: Option<&str>,
) -> CiMonitorServiceResult<CiMonitorStatus> {
    let config_state =
        evaluate_gh_monitor_config(home, &gh_request.team, gh_request.config_cwd.as_deref());
    if let Some(reason) = config_state.error.as_deref() {
        return Err(CiMonitorServiceError::new(
            "CONFIG_ERROR",
            format!("gh_monitor unavailable: {reason}"),
        ));
    }

    let state = load_gh_monitor_state_map(home).map_err(|e| {
        CiMonitorServiceError::internal(format!("Failed to read gh monitor state: {e}"))
    })?;

    let key = gh_monitor_key(
        &gh_request.team,
        gh_request.target_kind,
        &gh_request.target,
        gh_request.reference.as_deref(),
        repo_scope,
    );
    if let Some(mut status) = state.get(&key).cloned() {
        apply_config_state_to_status(&mut status, &config_state);
        return Ok(status);
    }

    if matches!(gh_request.target_kind, CiMonitorTargetKind::Workflow) {
        let mut candidates: Vec<&CiMonitorStatus> = state
            .values()
            .filter(|record| {
                record.team == gh_request.team
                    && record.target_kind == CiMonitorTargetKind::Workflow
                    && record.target == gh_request.target
                    && gh_request
                        .reference
                        .as_deref()
                        .is_none_or(|reference| record.reference.as_deref() == Some(reference))
            })
            .collect();
        candidates.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        if let Some(mut status) = candidates.last().cloned().cloned() {
            apply_config_state_to_status(&mut status, &config_state);
            return Ok(status);
        }
    }

    Err(CiMonitorServiceError::new(
        "MONITOR_NOT_FOUND",
        "No gh monitor state found for requested target",
    ))
}
