//! CI monitor service orchestration.

use super::alerts::{
    emit_ci_not_started_alert as send_ci_not_started_alert,
    emit_gh_monitor_health_transition as emit_health_transition,
    emit_merge_conflict_alert as send_merge_conflict_alert,
};
use super::gh_cli::{
    fetch_pull_request, fetch_run, is_pr_merge_state_dirty as pr_merge_state_dirty,
    try_find_pr_run_id as find_pr_run_id_with_provider,
    try_find_workflow_run_id as find_workflow_run_id_with_provider,
};
use super::helpers::{
    apply_config_state_to_status, count_in_flight_monitors, evaluate_gh_monitor_config,
    gh_monitor_key, load_gh_monitor_state_map, read_gh_monitor_health, upsert_gh_monitor_health,
    upsert_gh_monitor_status,
};
use super::polling::monitor_run;
use super::provider::ErasedCiProvider;
#[cfg(test)]
use super::types::{CiFilter, CiJob, CiPullRequest, CiRunStatus, CiStep};
use super::types::{CiRun, GhMonitorConfigState, GhMonitorHealthUpdate};
use agent_team_mail_core::daemon_client::{
    GhMonitorControlRequest, GhMonitorHealth, GhMonitorLifecycleAction, GhMonitorRequest,
    GhMonitorStatus, GhMonitorTargetKind, GhStatusRequest,
};
use std::sync::Arc;
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
#[cfg(all(test, unix))]
pub(crate) type GhRunView = CiRun;
#[cfg(all(test, unix))]
pub(crate) type GhRunJob = CiJob;
#[cfg(all(test, unix))]
pub(crate) type GhRunStep = CiStep;
#[cfg(all(test, unix))]
pub(crate) type GhPrView = CiPullRequest;
#[cfg(all(test, unix))]
pub(crate) type GhPullRequest = CiPullRequest;
#[cfg(all(test, unix))]
pub(crate) type GhPrLookupView = CiPullRequest;
#[cfg(all(test, unix))]
pub(crate) type GhRunListEntry = CiRun;
#[cfg(all(test, unix))]
pub(crate) type GhRunTerminalState = super::polling::GhRunTerminalState;

#[derive(Clone)]
pub(crate) struct CiMonitorService {
    provider: Arc<dyn ErasedCiProvider>,
    owner_repo: String,
}

impl std::fmt::Debug for CiMonitorService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CiMonitorService")
            .field("provider", &self.provider.provider_name())
            .field("owner_repo", &self.owner_repo)
            .finish()
    }
}

impl CiMonitorService {
    pub(crate) fn new(provider: Arc<dyn ErasedCiProvider>, owner_repo: impl Into<String>) -> Self {
        Self {
            provider,
            owner_repo: owner_repo.into(),
        }
    }

    fn from_config_state(config_state: &GhMonitorConfigState) -> CiMonitorServiceResult<Self> {
        let owner_repo = config_state
            .owner_repo
            .as_deref()
            .unwrap_or_default()
            .trim();
        let Some((owner, repo)) = owner_repo.split_once('/') else {
            return Err(CiMonitorServiceError::new(
                "CONFIG_ERROR",
                "gh_monitor unavailable: unable to resolve owner/repo for GitHub provider",
            ));
        };
        if owner.is_empty() || repo.is_empty() {
            return Err(CiMonitorServiceError::new(
                "CONFIG_ERROR",
                "gh_monitor unavailable: unable to resolve owner/repo for GitHub provider",
            ));
        }

        Ok(Self::new(
            Arc::new(super::github::GitHubActionsProvider::new(
                owner.to_string(),
                repo.to_string(),
            )),
            owner_repo.to_string(),
        ))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn fetch_run_view(&self, run_id: u64) -> CiMonitorServiceResult<CiRun> {
        fetch_run(self.provider.as_ref(), run_id)
            .await
            .map_err(|e| {
                CiMonitorServiceError::internal(format!("Failed to fetch run details: {e}"))
            })
    }

    pub(crate) async fn try_find_pr_run_id(
        &self,
        pr_number: u64,
    ) -> CiMonitorServiceResult<Option<u64>> {
        find_pr_run_id_with_provider(self.provider.as_ref(), pr_number)
            .await
            .map_err(|e| CiMonitorServiceError::internal(format!("Failed to find PR run: {e}")))
    }

    async fn try_find_workflow_run_id(
        &self,
        workflow: &str,
        reference: &str,
    ) -> CiMonitorServiceResult<Option<u64>> {
        find_workflow_run_id_with_provider(self.provider.as_ref(), workflow, reference)
            .await
            .map_err(|e| {
                CiMonitorServiceError::internal(format!("Failed to find workflow run: {e}"))
            })
    }

    pub(crate) async fn monitor_request(
        &self,
        home: &std::path::Path,
        gh_request: &GhMonitorRequest,
        config_state: &GhMonitorConfigState,
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

        let current_health = read_gh_monitor_health(home, &gh_request.team).map_err(|e| {
            CiMonitorServiceError::internal(format!("Failed to read gh monitor health: {e}"))
        })?;
        if current_health.lifecycle_state != "running" {
            return Err(CiMonitorServiceError::new(
                "MONITOR_STOPPED",
                "gh monitor lifecycle is not running (run `atm gh monitor start` first)",
            ));
        }

        if let Some(reason) = config_state.error.clone() {
            let _ = set_gh_monitor_health_state(
                home,
                &gh_request.team,
                GhMonitorHealthUpdate {
                    availability_state: Some("disabled_config_error"),
                    in_flight: Some(0),
                    message: Some(reason.clone()),
                    config_state: Some(config_state),
                    config_cwd: gh_request.config_cwd.as_deref(),
                    ..Default::default()
                },
            );
            return Err(CiMonitorServiceError::new(
                "CONFIG_ERROR",
                format!("gh_monitor unavailable: {reason}"),
            ));
        }

        if config_state
            .configured_team
            .as_deref()
            .is_some_and(|configured_team| configured_team != gh_request.team)
        {
            return Err(CiMonitorServiceError::new(
                "CONFIG_ERROR",
                format!(
                    "gh_monitor team mismatch: configured '{}' but request was '{}'",
                    config_state.configured_team.as_deref().unwrap_or_default(),
                    gh_request.team
                ),
            ));
        }

        if matches!(gh_request.target_kind, GhMonitorTargetKind::Workflow)
            && gh_request
                .reference
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none()
        {
            return Err(CiMonitorServiceError::new(
                "MISSING_PARAMETER",
                "Missing required payload field: 'reference' for workflow monitor",
            ));
        }

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
            updated_at: chrono::Utc::now().to_rfc3339(),
            message: None,
        };

        let mut transient_failure: Option<String> = None;
        match gh_request.target_kind {
            GhMonitorTargetKind::Run => {
                status.run_id = gh_request.target.parse::<u64>().ok();
            }
            GhMonitorTargetKind::Workflow => {
                if let Some(reference) = gh_request.reference.as_deref() {
                    match self
                        .try_find_workflow_run_id(&gh_request.target, reference)
                        .await
                    {
                        Ok(Some(run_id)) => status.run_id = Some(run_id),
                        Ok(None) => {}
                        Err(e) => {
                            transient_failure = Some(e.to_string());
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
                match fetch_pull_request(self.provider.as_ref(), pr_number).await {
                    Ok(Some(pr_view)) => {
                        if let Some(merge_state_status) = pr_view.merge_state_status.as_deref()
                            && pr_merge_state_dirty(merge_state_status)
                        {
                            status.state = "merge_conflict".to_string();
                            status.message = Some(format!(
                                "PR #{pr_number} has mergeStateStatus={merge_state_status}; resolve conflicts before CI monitoring."
                            ));
                            send_merge_conflict_alert(
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
                        match wait_for_pr_run_start_with_service(self, pr_number, timeout_secs)
                            .await
                        {
                            Ok(Some(run_id)) => status.run_id = Some(run_id),
                            Ok(None) => {
                                status.state = "ci_not_started".to_string();
                                status.message = Some(format!(
                                    "No workflow run observed for PR #{pr_number} within {timeout_secs}s."
                                ));
                            }
                            Err(e) => {
                                transient_failure = Some(e.to_string());
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

        upsert_gh_monitor_status(home, status.clone()).map_err(|e| {
            let _ = set_gh_monitor_health_state(
                home,
                &gh_request.team,
                GhMonitorHealthUpdate {
                    availability_state: Some("degraded"),
                    message: Some(format!("failed to persist monitor status: {e}")),
                    config_state: Some(config_state),
                    config_cwd: gh_request.config_cwd.as_deref(),
                    ..Default::default()
                },
            );
            CiMonitorServiceError::internal(format!("Failed to persist gh monitor state: {e}"))
        })?;

        if status.state == "ci_not_started" {
            send_ci_not_started_alert(home, &status, gh_request.config_cwd.as_deref());
        } else if let Some(run_id) = status.run_id {
            let home = home.to_path_buf();
            let service = self.clone();
            let status_seed = status.clone();
            let gh_request = gh_request.clone();
            let expected_repo = config_state.owner_repo.clone();
            tokio::spawn(async move {
                if let Err(e) = monitor_run(
                    service.provider.as_ref(),
                    &home,
                    &status_seed,
                    &gh_request,
                    expected_repo.as_deref(),
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

        let health_update = if let Some(reason) = transient_failure {
            GhMonitorHealthUpdate {
                availability_state: Some("degraded"),
                in_flight: Some(count_in_flight_monitors(home, &gh_request.team)),
                message: Some(format!("transient provider failure: {reason}")),
                config_state: Some(config_state),
                config_cwd: gh_request.config_cwd.as_deref(),
                ..Default::default()
            }
        } else {
            GhMonitorHealthUpdate {
                lifecycle_state: Some("running"),
                availability_state: Some("healthy"),
                in_flight: Some(count_in_flight_monitors(home, &gh_request.team)),
                message: Some("monitor request succeeded".to_string()),
                config_state: Some(config_state),
                config_cwd: gh_request.config_cwd.as_deref(),
            }
        };
        let _ = set_gh_monitor_health_state(home, &gh_request.team, health_update);

        Ok(status)
    }

    pub(crate) async fn control_request(
        home: &std::path::Path,
        control: &GhMonitorControlRequest,
        config_state: &GhMonitorConfigState,
    ) -> CiMonitorServiceResult<GhMonitorHealth> {
        if control.team.trim().is_empty() {
            return Err(CiMonitorServiceError::new(
                "MISSING_PARAMETER",
                "Missing required payload field: 'team'",
            ));
        }

        let health = match control.action {
            GhMonitorLifecycleAction::Start => set_gh_monitor_health_state(
                home,
                &control.team,
                GhMonitorHealthUpdate {
                    lifecycle_state: Some("running"),
                    in_flight: Some(count_in_flight_monitors(home, &control.team)),
                    message: Some("gh monitor lifecycle started".to_string()),
                    config_state: Some(config_state),
                    config_cwd: control.config_cwd.as_deref(),
                    ..Default::default()
                },
            ),
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
                        config_state: Some(config_state),
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

                set_gh_monitor_health_state(
                    home,
                    &control.team,
                    GhMonitorHealthUpdate {
                        lifecycle_state: Some("stopped"),
                        in_flight: Some(in_flight),
                        message: Some(if in_flight == 0 {
                            "gh monitor lifecycle stopped after in-flight drain".to_string()
                        } else {
                            format!(
                                "drain timeout reached; stopped with {} in-flight monitor(s)",
                                in_flight
                            )
                        }),
                        config_state: Some(config_state),
                        config_cwd: control.config_cwd.as_deref(),
                        ..Default::default()
                    },
                )
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
                        config_state: Some(config_state),
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
                    let _ = set_gh_monitor_health_state(
                        home,
                        &control.team,
                        GhMonitorHealthUpdate {
                            lifecycle_state: Some("stopped"),
                            availability_state: Some("disabled_config_error"),
                            in_flight: Some(in_flight),
                            message: Some(format!("gh monitor restart blocked: {reason}")),
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
            }
        };

        health.map_err(|e| {
            CiMonitorServiceError::internal(format!(
                "failed to update monitor lifecycle state: {e}"
            ))
        })
    }

    pub(crate) fn health_request(
        home: &std::path::Path,
        team: &str,
        config_state: &GhMonitorConfigState,
    ) -> CiMonitorServiceResult<GhMonitorHealth> {
        if team.trim().is_empty() {
            return Err(CiMonitorServiceError::new(
                "MISSING_PARAMETER",
                "Missing required payload field: 'team'",
            ));
        }
        let mut health = read_gh_monitor_health(home, team).map_err(|e| {
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

    pub(crate) fn status_request(
        home: &std::path::Path,
        gh_request: &GhStatusRequest,
        config_state: &GhMonitorConfigState,
    ) -> CiMonitorServiceResult<GhMonitorStatus> {
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
            apply_config_state_to_status(&mut status, config_state);
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
                apply_config_state_to_status(&mut status, config_state);
                return Ok(status);
            }
        }

        Err(CiMonitorServiceError::new(
            "MONITOR_NOT_FOUND",
            "No gh monitor state found for requested target",
        ))
    }
}

#[cfg(test)]
#[allow(dead_code)]
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

#[cfg(test)]
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
    let config_state =
        evaluate_gh_monitor_config(home, &gh_request.team, gh_request.config_cwd.as_deref());
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
    let current_health = read_gh_monitor_health(home, &gh_request.team).map_err(|e| {
        CiMonitorServiceError::internal(format!("Failed to read gh monitor health: {e}"))
    })?;
    if current_health.lifecycle_state != "running" {
        return Err(CiMonitorServiceError::new(
            "MONITOR_STOPPED",
            "gh monitor lifecycle is not running (run `atm gh monitor start` first)",
        ));
    }
    if let Some(reason) = config_state.error.clone() {
        let _ = set_gh_monitor_health_state(
            home,
            &gh_request.team,
            GhMonitorHealthUpdate {
                availability_state: Some("disabled_config_error"),
                in_flight: Some(0),
                message: Some(reason.clone()),
                config_state: Some(&config_state),
                config_cwd: gh_request.config_cwd.as_deref(),
                ..Default::default()
            },
        );
        return Err(CiMonitorServiceError::new(
            "CONFIG_ERROR",
            format!("gh_monitor unavailable: {reason}"),
        ));
    }
    let service = CiMonitorService::from_config_state(&config_state)?;
    service
        .monitor_request(home, gh_request, &config_state)
        .await
}

#[cfg(unix)]
pub(crate) async fn control_request(
    home: &std::path::Path,
    control: &GhMonitorControlRequest,
) -> CiMonitorServiceResult<GhMonitorHealth> {
    let config_state =
        evaluate_gh_monitor_config(home, &control.team, control.config_cwd.as_deref());
    CiMonitorService::control_request(home, control, &config_state).await
}

#[cfg(unix)]
pub(crate) fn health_request(
    home: &std::path::Path,
    team: &str,
    config_cwd: Option<&str>,
) -> CiMonitorServiceResult<GhMonitorHealth> {
    let config_state = evaluate_gh_monitor_config(home, team, config_cwd);
    CiMonitorService::health_request(home, team, &config_state)
}

#[cfg(unix)]
pub(crate) fn status_request(
    home: &std::path::Path,
    gh_request: &GhStatusRequest,
) -> CiMonitorServiceResult<GhMonitorStatus> {
    let config_state =
        evaluate_gh_monitor_config(home, &gh_request.team, gh_request.config_cwd.as_deref());
    CiMonitorService::status_request(home, gh_request, &config_state)
}

#[cfg(unix)]
async fn wait_for_pr_run_start_with_service(
    service: &CiMonitorService,
    pr_number: u64,
    timeout_secs: u64,
) -> CiMonitorServiceResult<Option<u64>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        if let Some(run_id) = service.try_find_pr_run_id(pr_number).await? {
            return Ok(Some(run_id));
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(
            deadline
                .saturating_duration_since(now)
                .min(std::time::Duration::from_secs(5)),
        )
        .await;
    }
}

#[cfg(unix)]
pub(crate) fn set_gh_monitor_health_state(
    home: &std::path::Path,
    team: &str,
    update: GhMonitorHealthUpdate<'_>,
) -> anyhow::Result<GhMonitorHealth> {
    let mut current = read_gh_monitor_health(home, team)?;
    let old_availability = current.availability_state.clone();

    if let Some(lifecycle_state) = update.lifecycle_state {
        current.lifecycle_state = lifecycle_state.to_string();
    }
    if let Some(availability_state) = update.availability_state {
        current.availability_state = availability_state.to_string();
    }
    if let Some(in_flight) = update.in_flight {
        current.in_flight = in_flight;
    }
    if let Some(config_state) = update.config_state {
        current.configured = config_state.configured;
        current.enabled = config_state.enabled;
        current.config_source = config_state.config_source.clone();
        current.config_path = config_state.config_path.clone();
    }
    current.updated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    current.message = update.message;

    if old_availability != current.availability_state {
        let reason = current
            .message
            .clone()
            .unwrap_or_else(|| "availability changed".to_string());
        emit_health_transition(
            home,
            team,
            update.config_cwd,
            &old_availability,
            &current.availability_state,
            &reason,
        );
    }

    upsert_gh_monitor_health(home, current.clone())?;
    Ok(current)
}

#[cfg(all(test, unix))]
pub(crate) async fn try_find_pr_run_id(
    owner_repo: &str,
    pr_number: u64,
) -> anyhow::Result<Option<u64>> {
    let service = build_github_service(owner_repo)?;
    service
        .try_find_pr_run_id(pr_number)
        .await
        .map_err(anyhow::Error::from)
}

#[cfg(all(test, unix))]
pub(crate) async fn wait_for_pr_run_start(
    owner_repo: &str,
    pr_number: u64,
    timeout_secs: u64,
) -> anyhow::Result<Option<u64>> {
    let service = build_github_service(owner_repo)?;
    wait_for_pr_run_start_with_service(&service, pr_number, timeout_secs)
        .await
        .map_err(anyhow::Error::from)
}

#[cfg(all(test, unix))]
pub(crate) async fn fetch_pr_merge_state(
    owner_repo: &str,
    pr_number: u64,
) -> anyhow::Result<Option<GhPrView>> {
    let service = build_github_service(owner_repo)?;
    fetch_pull_request(service.provider.as_ref(), pr_number)
        .await
        .map_err(anyhow::Error::from)
}

#[cfg(all(test, unix))]
pub(crate) async fn fetch_run_view(owner_repo: &str, run_id: u64) -> anyhow::Result<GhRunView> {
    let service = build_github_service(owner_repo)?;
    service
        .fetch_run_view(run_id)
        .await
        .map_err(anyhow::Error::from)
}

#[cfg(all(test, unix))]
pub(crate) async fn try_find_workflow_run_id(
    owner_repo: &str,
    workflow: &str,
    reference: &str,
) -> anyhow::Result<Option<u64>> {
    let service = build_github_service(owner_repo)?;
    service
        .try_find_workflow_run_id(workflow, reference)
        .await
        .map_err(anyhow::Error::from)
}

#[cfg(all(test, unix))]
pub(crate) async fn run_gh_command(args: &[&str]) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("gh")
        .args(args)
        .output()
        .await?;
    if !output.status.success() {
        anyhow::bail!(
            "gh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(all(test, unix))]
pub(crate) async fn run_gh_command_for_repo(
    owner_repo: &str,
    args: &[&str],
) -> anyhow::Result<String> {
    if owner_repo.trim().is_empty() {
        anyhow::bail!("missing owner/repo scope for gh command");
    }
    let mut command_args = vec!["-R", owner_repo];
    command_args.extend_from_slice(args);
    run_gh_command(&command_args).await
}

#[cfg(all(test, unix))]
pub(crate) async fn monitor_gh_run(
    home: &std::path::Path,
    status_seed: &GhMonitorStatus,
    gh_request: &GhMonitorRequest,
    owner_repo: &str,
    run_id: u64,
) -> anyhow::Result<()> {
    let service = build_github_service(owner_repo)?;
    monitor_run(
        service.provider.as_ref(),
        home,
        status_seed,
        gh_request,
        Some(owner_repo),
        run_id,
    )
    .await
    .map_err(anyhow::Error::from)
}

#[cfg(all(test, unix))]
pub(crate) async fn build_failure_payload(
    run: &GhRunView,
    status_seed: &GhMonitorStatus,
    gh_request: &GhMonitorRequest,
    owner_repo: &str,
    correlation_id: &str,
) -> String {
    let service = build_github_service(owner_repo).expect("valid owner/repo");
    super::polling::build_failure_payload(
        service.provider.as_ref(),
        run,
        status_seed,
        gh_request,
        correlation_id,
    )
    .await
}

#[cfg(all(test, unix))]
pub(crate) async fn fetch_failed_log_excerpt(
    owner_repo: &str,
    job_id: u64,
) -> anyhow::Result<String> {
    let service = build_github_service(owner_repo)?;
    super::gh_cli::fetch_failed_log_excerpt(service.provider.as_ref(), job_id)
        .await
        .map_err(anyhow::Error::from)
}

#[cfg(all(test, unix))]
fn build_github_service(owner_repo: &str) -> anyhow::Result<CiMonitorService> {
    let (owner, repo) = owner_repo
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("missing owner/repo scope for gh command"))?;
    Ok(CiMonitorService::new(
        Arc::new(super::github::GitHubActionsProvider::new(
            owner.to_string(),
            repo.to_string(),
        )),
        owner_repo.to_string(),
    ))
}

#[cfg(all(test, unix))]
pub(crate) use super::alerts::{
    emit_ci_monitor_message, emit_ci_not_started_alert, emit_gh_monitor_health_transition,
    emit_merge_conflict_alert, repo_scope_matches, resolve_ci_alert_routing,
};
#[cfg(all(test, unix))]
pub(crate) use super::gh_cli::{is_pr_merge_state_dirty, run_passes_pr_recency_gate};
#[cfg(all(test, unix))]
pub(crate) use super::polling::{
    classify_failure, classify_terminal_state, count_completed_jobs, derive_pr_url,
    derive_repo_base_from_run_url, extract_repo_slug_from_url, format_job_runtime,
    format_progress_message, format_summary_table, is_infra_failure, is_job_completed,
    job_status_label, short_sha, should_emit_progress, terminal_state_label,
};

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::plugins::ci_monitor::mock_provider::{
        MockCall, MockCiProvider, create_test_job, create_test_run,
    };
    use serial_test::serial;
    use tempfile::TempDir;

    fn config_state() -> GhMonitorConfigState {
        GhMonitorConfigState {
            configured: true,
            enabled: true,
            config_source: None,
            config_path: None,
            configured_team: Some("atm-dev".to_string()),
            owner_repo: Some("o/r".to_string()),
            error: None,
        }
    }

    fn seed_health_and_config(temp: &TempDir) {
        std::fs::create_dir_all(temp.path().join(".atm/daemon")).unwrap();
        set_gh_monitor_health_state(
            temp.path(),
            "atm-dev",
            GhMonitorHealthUpdate {
                lifecycle_state: Some("running"),
                availability_state: Some("healthy"),
                in_flight: Some(0),
                message: Some("seed".to_string()),
                config_state: Some(&config_state()),
                config_cwd: None,
            },
        )
        .unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn test_service_monitor_request_uses_injected_provider_for_pr_lookup() {
        let temp = TempDir::new().unwrap();
        seed_health_and_config(&temp);
        let provider = Arc::new(
            MockCiProvider::with_runs(vec![create_test_run(
                123456,
                "ci",
                "feature/test",
                CiRunStatus::Completed,
                Some(super::super::types::CiRunConclusion::Success),
            )])
            .with_pull_requests(vec![super::super::types::CiPullRequest {
                number: 42,
                url: Some("https://github.com/o/r/pull/42".to_string()),
                head_ref_name: Some("feature/test".to_string()),
                head_ref_oid: Some("sha123456".to_string()),
                created_at: Some("2026-02-13T10:00:00Z".to_string()),
                merge_state_status: Some("clean".to_string()),
            }]),
        );
        let service = CiMonitorService::new(provider.clone(), "o/r");

        let status = service
            .monitor_request(
                temp.path(),
                &GhMonitorRequest {
                    team: "atm-dev".to_string(),
                    target_kind: GhMonitorTargetKind::Pr,
                    target: "42".to_string(),
                    reference: None,
                    start_timeout_secs: Some(1),
                    config_cwd: None,
                },
                &config_state(),
            )
            .await
            .unwrap();

        assert_eq!(status.run_id, Some(123456));
        assert_eq!(
            provider.get_calls(),
            vec![
                MockCall::GetPullRequest(42),
                MockCall::GetPullRequest(42),
                MockCall::ListRuns(CiFilter {
                    branch: Some("feature/test".to_string()),
                    per_page: Some(20),
                    ..Default::default()
                }),
            ]
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_service_monitor_request_uses_injected_provider_for_workflow_lookup() {
        let temp = TempDir::new().unwrap();
        seed_health_and_config(&temp);
        let provider = Arc::new(MockCiProvider::with_runs(vec![create_test_run(
            987654,
            "ci",
            "develop",
            CiRunStatus::Completed,
            Some(super::super::types::CiRunConclusion::Success),
        )]));
        let service = CiMonitorService::new(provider.clone(), "o/r");

        let status = service
            .monitor_request(
                temp.path(),
                &GhMonitorRequest {
                    team: "atm-dev".to_string(),
                    target_kind: GhMonitorTargetKind::Workflow,
                    target: "ci".to_string(),
                    reference: Some("develop".to_string()),
                    start_timeout_secs: Some(1),
                    config_cwd: None,
                },
                &config_state(),
            )
            .await
            .unwrap();

        assert_eq!(status.run_id, Some(987654));
        assert_eq!(
            provider.get_calls(),
            vec![MockCall::ListRuns(CiFilter {
                branch: Some("develop".to_string()),
                per_page: Some(20),
                ..Default::default()
            }),]
        );
    }

    #[test]
    fn test_status_request_returns_seeded_run_monitor_status() {
        let temp = TempDir::new().unwrap();
        seed_health_and_config(&temp);
        upsert_gh_monitor_status(
            temp.path(),
            GhMonitorStatus {
                team: "atm-dev".to_string(),
                configured: true,
                enabled: true,
                config_source: None,
                config_path: None,
                target_kind: GhMonitorTargetKind::Run,
                target: "456".to_string(),
                state: "monitoring".to_string(),
                run_id: Some(456),
                reference: None,
                updated_at: "2026-03-13T00:00:00Z".to_string(),
                message: Some("seeded".to_string()),
            },
        )
        .unwrap();

        let status = CiMonitorService::status_request(
            temp.path(),
            &GhStatusRequest {
                team: "atm-dev".to_string(),
                target_kind: GhMonitorTargetKind::Run,
                target: "456".to_string(),
                reference: None,
                config_cwd: None,
            },
            &config_state(),
        )
        .unwrap();

        assert_eq!(status.run_id, Some(456));
        assert_eq!(status.state, "monitoring");
    }

    #[test]
    fn test_fetch_run_details_uses_provider_boundary() {
        let run = create_test_run(
            7,
            "ci",
            "main",
            CiRunStatus::Completed,
            Some(super::super::types::CiRunConclusion::Success),
        );
        let job = create_test_job(
            8,
            "build",
            CiRunStatus::Completed,
            Some(super::super::types::CiRunConclusion::Success),
        );
        let provider = MockCiProvider::with_runs_and_jobs(vec![run], vec![job]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let fetched = rt.block_on(fetch_run_details(&provider, 7)).unwrap();
        assert_eq!(fetched.id, 7);
        assert_eq!(provider.get_calls(), vec![MockCall::GetRun(7)]);
    }
}
