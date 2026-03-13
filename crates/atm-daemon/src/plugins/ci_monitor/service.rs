//! CI monitor service orchestration.

#[cfg(unix)]
use super::gh_monitor::{
    fetch_pr_merge_state, is_pr_merge_state_dirty, monitor_gh_run, try_find_workflow_run_id,
    wait_for_pr_run_start,
};
#[cfg(unix)]
use super::health::set_gh_monitor_health_state;
use super::helpers::{
    apply_config_state_to_status, count_in_flight_monitors, evaluate_gh_monitor_config,
    gh_monitor_key, load_gh_monitor_state_map,
};
use super::provider::ErasedCiProvider;
#[cfg(unix)]
use super::routing::{notify_ci_not_started, notify_merge_conflict};
use super::types::{CiFilter, CiRun, CiRunStatus, GhMonitorConfigState, GhMonitorHealthUpdate};
use agent_team_mail_core::daemon_client::{
    GhMonitorControlRequest, GhMonitorHealth, GhMonitorLifecycleAction, GhMonitorRequest,
    GhMonitorStatus, GhMonitorTargetKind, GhStatusRequest,
};
use tracing::warn;

const CI_MONITOR_INTERNAL_ERROR: &str = "INTERNAL_ERROR";

#[derive(Debug, Clone)]
pub(crate) struct CiMonitorServiceError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl CiMonitorServiceError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(CI_MONITOR_INTERNAL_ERROR, message)
    }
}

impl std::fmt::Display for CiMonitorServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CiMonitorServiceError {}

pub(crate) type CiMonitorServiceResult<T> = std::result::Result<T, CiMonitorServiceError>;

#[cfg(unix)]
fn validate_monitor_request(
    gh_request: &GhMonitorRequest,
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

    if matches!(gh_request.target_kind, GhMonitorTargetKind::Workflow)
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

pub(crate) async fn list_completed_runs(
    provider: &dyn ErasedCiProvider,
) -> CiMonitorServiceResult<Vec<CiRun>> {
    let filter = CiFilter {
        status: Some(CiRunStatus::Completed),
        per_page: Some(20),
        ..Default::default()
    };

    provider
        .list_runs(&filter)
        .await
        .map_err(|e| CiMonitorServiceError::internal(format!("Failed to list runs: {e}")))
}

pub(crate) async fn fetch_run_details(
    provider: &dyn ErasedCiProvider,
    run_id: u64,
) -> CiMonitorServiceResult<CiRun> {
    provider
        .get_run(run_id)
        .await
        .map_err(|e| CiMonitorServiceError::internal(format!("Failed to fetch run details: {e}")))
}

#[cfg(unix)]
pub(crate) async fn monitor_request(
    home: &std::path::Path,
    gh_request: &GhMonitorRequest,
) -> CiMonitorServiceResult<GhMonitorStatus> {
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
                    in_flight: Some(0),
                    message: Some(reason),
                    config_state: Some(&config_state),
                    config_cwd: gh_request.config_cwd.as_deref(),
                    ..Default::default()
                },
            );
        }
        return Err(CiMonitorServiceError::new(code, message));
    }

    let owner_repo = config_state.owner_repo.as_deref().unwrap_or_default();

    let now = chrono::Utc::now().to_rfc3339();
    let mut status = GhMonitorStatus {
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
    };

    let mut transient_failure: Option<String> = None;
    match gh_request.target_kind {
        GhMonitorTargetKind::Run => {
            status.run_id = gh_request.target.parse::<u64>().ok();
        }
        GhMonitorTargetKind::Workflow => {
            if let Some(reference) = gh_request.reference.as_deref() {
                match try_find_workflow_run_id(owner_repo, &gh_request.target, reference).await {
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
        GhMonitorTargetKind::Pr => {
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
            match fetch_pr_merge_state(owner_repo, pr_number).await {
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
                    match wait_for_pr_run_start(owner_repo, pr_number, timeout_secs).await {
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

    super::helpers::upsert_gh_monitor_status(home, status.clone()).map_err(|e| {
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
    })?;

    if status.state == "ci_not_started" {
        notify_ci_not_started(home, &status, gh_request.config_cwd.as_deref());
    } else if let Some(run_id) = status.run_id {
        let home = home.to_path_buf();
        let status_seed = status.clone();
        let gh_request = gh_request.clone();
        let owner_repo = owner_repo.to_string();
        tokio::spawn(async move {
            if let Err(e) = monitor_gh_run(
                home.as_path(),
                &status_seed,
                &gh_request,
                &owner_repo,
                run_id,
            )
            .await
            {
                warn!(
                    team = %status_seed.team,
                    target = %status_seed.target,
                    run_id = run_id,
                    "gh monitor background task failed: {e}"
                );
            }
        });
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
    control: &GhMonitorControlRequest,
) -> CiMonitorServiceResult<GhMonitorHealth> {
    if control.team.trim().is_empty() {
        return Err(CiMonitorServiceError::new(
            "MISSING_PARAMETER",
            "Missing required payload field: 'team'",
        ));
    }

    let config_state =
        evaluate_gh_monitor_config(home, &control.team, control.config_cwd.as_deref());

    let health = match control.action {
        GhMonitorLifecycleAction::Start => set_gh_monitor_health_state(
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
        GhMonitorLifecycleAction::Stop => {
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
        GhMonitorLifecycleAction::Restart => {
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
) -> CiMonitorServiceResult<GhMonitorHealth> {
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
    Ok(health)
}

#[cfg(unix)]
pub(crate) fn status_request(
    home: &std::path::Path,
    gh_request: &GhStatusRequest,
) -> CiMonitorServiceResult<GhMonitorStatus> {
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
    );
    if let Some(mut status) = state.get(&key).cloned() {
        apply_config_state_to_status(&mut status, &config_state);
        return Ok(status);
    }

    if matches!(gh_request.target_kind, GhMonitorTargetKind::Workflow) {
        let mut candidates: Vec<&GhMonitorStatus> = state
            .values()
            .filter(|record| {
                record.team == gh_request.team
                    && record.target_kind == GhMonitorTargetKind::Workflow
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::ci_monitor::{
        CiRunConclusion, MockCall, MockCiProvider, create_test_job, create_test_run,
    };

    #[tokio::test]
    async fn test_list_completed_runs_uses_completed_filter() {
        let run = create_test_run(
            7,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Success),
        );
        let provider = MockCiProvider::with_runs(vec![run.clone()]);

        let runs = list_completed_runs(&provider).await.unwrap();

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run.id);
        assert_eq!(runs[0].name, run.name);
        assert_eq!(
            provider.get_calls(),
            vec![MockCall::ListRuns(CiFilter {
                status: Some(CiRunStatus::Completed),
                per_page: Some(20),
                ..Default::default()
            })]
        );
    }

    #[tokio::test]
    async fn test_fetch_run_details_uses_provider_boundary() {
        let run = create_test_run(
            42,
            "CI",
            "develop",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );
        let provider = MockCiProvider::with_runs_and_jobs(
            vec![run],
            vec![create_test_job(
                9001,
                "unit",
                CiRunStatus::Completed,
                Some(CiRunConclusion::Failure),
            )],
        );

        let full_run = fetch_run_details(&provider, 42).await.unwrap();

        assert_eq!(full_run.id, 42);
        assert_eq!(full_run.jobs.as_ref().map(Vec::len), Some(1));
        assert_eq!(provider.get_calls(), vec![MockCall::GetRun(42)]);
    }
}
