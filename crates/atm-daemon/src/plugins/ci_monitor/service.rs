//! Transport-free CI monitor service orchestration.
//!
//! This module forms the core CI monitor boundary. It must not depend on daemon
//! socket/router request or response types; daemon transport adapters are responsible
//! for translating wire payloads into these CI monitor request/status types before
//! calling into the service layer.

use crate::daemon::consts::{
    DEFAULT_DRAIN_TIMEOUT_SECS, DRAIN_SLEEP_MS, SHARED_POLLER_ACTIVE_SLEEP_SECS,
    SHARED_POLLER_ERROR_BACKOFF_SECS, SHARED_POLLER_IDLE_SLEEP_SECS,
};
use agent_team_mail_ci_monitor::consts::{
    GH_MONITOR_HEADROOM_FLOOR, GH_MONITOR_HEADROOM_RECOVERY_FLOOR,
    GH_MONITOR_PER_ACTIVE_MONITOR_MAX_CALLS,
};

#[cfg(unix)]
use super::gh_monitor::{
    RunPollProgress, fetch_pr_merge_state, is_pr_merge_state_dirty, poll_monitored_run_once,
    try_find_pr_run_id, try_find_workflow_run_id, wait_for_pr_run_start,
};
use super::github_provider::GitHubActionsProvider;
#[cfg(unix)]
use super::health::{apply_repo_state_to_health, set_gh_monitor_health_state};
use super::helpers::{
    apply_config_state_to_status, count_in_flight_monitors, evaluate_gh_monitor_config,
    gh_monitor_key, lifecycle_state_allows_polling, load_gh_monitor_state_map,
    load_gh_monitor_state_records, repo_state_polling_suppressed,
};
use super::provider::ErasedCiProvider;
use super::registry::CiProviderRegistryPort;
#[cfg(unix)]
use super::routing::{notify_ci_not_started, notify_merge_conflict};
use super::types::{
    CiMonitorControlRequest, CiMonitorHealth, CiMonitorLifecycleAction, CiMonitorRequest,
    CiMonitorStatus, CiMonitorStatusRequest, CiMonitorTargetKind, GhAlertTargets,
    GhMonitorConfigState, GhMonitorHealthUpdate, GhMonitorStateRecord,
};
use agent_team_mail_core::context::GitProvider as GitProviderType;
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::gh_monitor_observability::{
    GhCliObserverContext, build_gh_cli_observer, emit_gh_info_requested,
    emit_gh_info_served_from_cache, gh_repo_state_cache_age_secs, new_gh_info_request_id,
    read_gh_repo_state_record, update_gh_repo_state_blocked, update_gh_repo_state_in_flight,
};
use agent_team_mail_core::home::teams_root_dir_for;
use agent_team_mail_core::io::inbox::inbox_append;
use agent_team_mail_core::schema::InboxMessage;
use agent_team_mail_core::team_config_store::TeamConfigStore;
use serde_json::json;
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
fn notify_team_lead_of_monitor_control(
    home: &std::path::Path,
    actor: &str,
    actor_team: &str,
    target_team: &str,
    action_word: &str,
    reason: &str,
) -> anyhow::Result<()> {
    let teams_root = teams_root_dir_for(home);
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
    let inbox_path = team_dir.join("inboxes").join(format!("{lead_agent}.json"));
    let now = chrono::Utc::now().to_rfc3339();
    let message = InboxMessage {
        from: actor.to_string(),
        text: format!(
            "your gh monitor was {action_word} by {actor}@{actor_team} for {}",
            reason.trim()
        ),
        timestamp: now.clone(),
        read: false,
        summary: Some(format!("gh monitor {action_word} by {actor}@{actor_team}")),
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
#[derive(Debug, Clone, PartialEq, Eq)]
enum SharedPollerSuppression {
    Lifecycle { state: String },
    Headroom { remaining: u64 },
    BudgetExhausted { used: u64, limit: u64 },
}

#[cfg(unix)]
#[derive(Debug, Clone)]
struct SharedPollerPlan {
    active_records: Vec<GhMonitorStateRecord>,
    in_flight: u64,
    sleep_secs: u64,
    suppression: Option<SharedPollerSuppression>,
}

#[cfg(unix)]
fn read_monitor_lifecycle_state(home: &std::path::Path, team: &str) -> String {
    super::health::read_gh_monitor_health(home, team)
        .map(|health| health.lifecycle_state)
        .unwrap_or_else(|_| "running".to_string())
}

#[cfg(unix)]
fn shared_poller_suppression(
    lifecycle_state: &str,
    repo_state: Option<&agent_team_mail_ci_monitor::GhRepoStateRecord>,
) -> Option<SharedPollerSuppression> {
    if !lifecycle_state_allows_polling(lifecycle_state) {
        return Some(SharedPollerSuppression::Lifecycle {
            state: lifecycle_state.to_string(),
        });
    }
    let record = repo_state?;
    if record.budget_used_in_window >= record.budget_limit_per_hour {
        return Some(SharedPollerSuppression::BudgetExhausted {
            used: record.budget_used_in_window,
            limit: record.budget_limit_per_hour,
        });
    }
    if repo_state_polling_suppressed(record) {
        return Some(SharedPollerSuppression::Headroom {
            remaining: record
                .rate_limit
                .as_ref()
                .map(|snapshot| snapshot.remaining)
                .unwrap_or(0),
        });
    }
    None
}

#[cfg(unix)]
fn build_shared_poller_plan(
    home: &std::path::Path,
    team: &str,
    owner_repo: &str,
    scoped_records: Vec<GhMonitorStateRecord>,
) -> SharedPollerPlan {
    let active_records: Vec<_> = scoped_records
        .into_iter()
        .filter(|record| status_has_active_subscription(&record.status))
        .collect();
    let repo_state = read_gh_repo_state_record(home, team, owner_repo)
        .ok()
        .flatten();
    let lifecycle_state = read_monitor_lifecycle_state(home, team);
    let suppression = shared_poller_suppression(&lifecycle_state, repo_state.as_ref());
    let sleep_secs = if active_records.is_empty() {
        SHARED_POLLER_IDLE_SLEEP_SECS
    } else {
        SHARED_POLLER_ACTIVE_SLEEP_SECS
    };
    if suppression.is_some() {
        return SharedPollerPlan {
            active_records: Vec::new(),
            in_flight: 0,
            sleep_secs,
            suppression,
        };
    }
    SharedPollerPlan {
        in_flight: active_records.len() as u64,
        active_records,
        sleep_secs,
        suppression: None,
    }
}

#[cfg(unix)]
fn headroom_suppression_message(remaining: u64) -> String {
    format!(
        "shared gh polling paused: remaining GitHub quota {remaining} is at/below floor {}; resume requires at least {} remaining",
        GH_MONITOR_HEADROOM_FLOOR, GH_MONITOR_HEADROOM_RECOVERY_FLOOR
    )
}

#[cfg(unix)]
fn emit_shared_poller_suppression_event(
    team: &str,
    owner_repo: &str,
    action: &'static str,
    message: &str,
    remaining: Option<u64>,
) {
    let mut extra_fields = serde_json::Map::new();
    extra_fields.insert("repo".to_string(), json!(owner_repo));
    extra_fields.insert(
        "headroom_floor".to_string(),
        json!(GH_MONITOR_HEADROOM_FLOOR),
    );
    extra_fields.insert(
        "headroom_recovery_floor".to_string(),
        json!(GH_MONITOR_HEADROOM_RECOVERY_FLOOR),
    );
    if let Some(remaining) = remaining {
        extra_fields.insert("remaining".to_string(), json!(remaining));
    }
    emit_event_best_effort(EventFields {
        level: if action == "gh_poll_suppressed_headroom" {
            "warn"
        } else {
            "info"
        },
        source: "atm",
        action,
        team: Some(team.to_string()),
        target: Some(owner_repo.to_string()),
        runtime: Some("atm-daemon".to_string()),
        result: Some(action.to_string()),
        error: Some(message.to_string()),
        extra_fields,
        ..Default::default()
    });
}

#[cfg(unix)]
fn sync_headroom_suppression_state(
    home: &std::path::Path,
    team: &str,
    owner_repo: &str,
    suppression: &Option<SharedPollerSuppression>,
) {
    let current = read_gh_repo_state_record(home, team, owner_repo)
        .ok()
        .flatten();
    let currently_blocked = current.as_ref().is_some_and(|record| record.blocked);
    let should_block = matches!(
        suppression,
        Some(
            SharedPollerSuppression::Headroom { .. }
                | SharedPollerSuppression::BudgetExhausted { .. }
        )
    );
    if should_block == currently_blocked {
        return;
    }

    let _ = update_gh_repo_state_blocked(home, team, owner_repo, should_block, "atm-daemon");

    match suppression {
        Some(SharedPollerSuppression::Headroom { remaining }) => {
            let message = headroom_suppression_message(*remaining);
            emit_shared_poller_suppression_event(
                team,
                owner_repo,
                "gh_poll_suppressed_headroom",
                &message,
                Some(*remaining),
            );
            let _ = set_gh_monitor_health_state(
                home,
                team,
                GhMonitorHealthUpdate {
                    availability_state: Some("degraded"),
                    in_flight: Some(0),
                    message: Some(message),
                    ..Default::default()
                },
            );
        }
        Some(SharedPollerSuppression::BudgetExhausted { used, limit }) => {
            let message = format!(
                "shared gh polling paused: budget exhausted at {used}/{limit} calls in the current window"
            );
            let _ = set_gh_monitor_health_state(
                home,
                team,
                GhMonitorHealthUpdate {
                    availability_state: Some("degraded"),
                    in_flight: Some(0),
                    message: Some(message),
                    ..Default::default()
                },
            );
        }
        _ => {
            emit_shared_poller_suppression_event(
                team,
                owner_repo,
                "gh_poll_resumed_headroom",
                "shared gh polling resumed after headroom recovery",
                current
                    .as_ref()
                    .and_then(|record| record.rate_limit.as_ref().map(|rate| rate.remaining)),
            );
            let _ = set_gh_monitor_health_state(
                home,
                team,
                GhMonitorHealthUpdate {
                    availability_state: Some("healthy"),
                    message: Some("shared gh polling resumed after headroom recovery".to_string()),
                    ..Default::default()
                },
            );
        }
    }
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
                tokio::time::sleep(std::time::Duration::from_secs(
                    SHARED_POLLER_ERROR_BACKOFF_SECS,
                ))
                .await;
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
        let mut plan = build_shared_poller_plan(&home, &team, &owner_repo, scoped_records);
        sync_headroom_suppression_state(&home, &team, &owner_repo, &plan.suppression);

        let _ =
            update_gh_repo_state_in_flight(&home, &team, &owner_repo, plan.in_flight, "atm-daemon");
        if plan.suppression.is_some() {
            tokio::time::sleep(std::time::Duration::from_secs(plan.sleep_secs)).await;
            continue;
        }

        if let Err(err) = refresh_shared_repo_state(&home, &team, &owner_repo).await {
            warn!(team = %team, repo = %owner_repo, "shared repo-state refresh failed: {err}");
        }

        plan = build_shared_poller_plan(
            &home,
            &team,
            &owner_repo,
            load_gh_monitor_state_records(&home)
                .unwrap_or_default()
                .into_iter()
                .filter(|record| {
                    record.status.team == team
                        && record
                            .repo_scope
                            .as_deref()
                            .map(|value| value.eq_ignore_ascii_case(&owner_repo))
                            .unwrap_or(false)
                })
                .collect(),
        );
        sync_headroom_suppression_state(&home, &team, &owner_repo, &plan.suppression);
        let _ =
            update_gh_repo_state_in_flight(&home, &team, &owner_repo, plan.in_flight, "atm-daemon");
        if plan.suppression.is_some() {
            tokio::time::sleep(std::time::Duration::from_secs(plan.sleep_secs)).await;
            continue;
        }

        let start_budget_used = read_gh_repo_state_record(&home, &team, &owner_repo)
            .ok()
            .flatten()
            .map(|record| record.budget_used_in_window)
            .unwrap_or(0);
        let cycle_call_cap = plan
            .in_flight
            .saturating_mul(GH_MONITOR_PER_ACTIVE_MONITOR_MAX_CALLS);

        for record in &plan.active_records {
            let current_budget_used = read_gh_repo_state_record(&home, &team, &owner_repo)
                .ok()
                .flatten()
                .map(|repo_state| repo_state.budget_used_in_window)
                .unwrap_or(start_budget_used);
            if current_budget_used.saturating_sub(start_budget_used) >= cycle_call_cap {
                let message = format!(
                    "shared gh polling backed off after reaching cycle cap of {cycle_call_cap} calls for {} active monitor(s)",
                    plan.in_flight
                );
                emit_event_best_effort(EventFields {
                    level: "warn",
                    source: "atm",
                    action: "gh_poll_suppressed_budget_cap",
                    team: Some(team.clone()),
                    target: Some(owner_repo.clone()),
                    runtime: Some("atm-daemon".to_string()),
                    result: Some("gh_poll_suppressed_budget_cap".to_string()),
                    error: Some(message.clone()),
                    extra_fields: serde_json::Map::from_iter([
                        ("repo".to_string(), json!(owner_repo)),
                        ("cycle_call_cap".to_string(), json!(cycle_call_cap)),
                        ("active_monitor_count".to_string(), json!(plan.in_flight)),
                    ]),
                    ..Default::default()
                });
                let _ = set_gh_monitor_health_state(
                    &home,
                    &team,
                    GhMonitorHealthUpdate {
                        availability_state: Some("degraded"),
                        in_flight: Some(plan.in_flight),
                        message: Some(message),
                        ..Default::default()
                    },
                );
                warn!(
                    team = %team,
                    repo = %owner_repo,
                    cycle_call_cap,
                    "shared gh monitor poll cycle reached per-active-monitor budget cap"
                );
                break;
            }
            if let Err(err) = poll_status_once(&home, &owner_repo, repo_scope, record).await {
                warn!(
                    team = %record.status.team,
                    target = %record.status.target,
                    repo = %owner_repo,
                    "shared gh monitor poll failed: {err}"
                );
            }
            let refreshed_plan = build_shared_poller_plan(
                &home,
                &team,
                &owner_repo,
                load_gh_monitor_state_records(&home)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|record| {
                        record.status.team == team
                            && record
                                .repo_scope
                                .as_deref()
                                .map(|value| value.eq_ignore_ascii_case(&owner_repo))
                                .unwrap_or(false)
                    })
                    .collect(),
            );
            sync_headroom_suppression_state(&home, &team, &owner_repo, &refreshed_plan.suppression);
            if refreshed_plan.suppression.is_some() {
                break;
            }
        }

        let _ = update_gh_repo_state_in_flight(&home, &team, &owner_repo, 0, "atm-daemon");
        tokio::time::sleep(std::time::Duration::from_secs(plan.sleep_secs)).await;
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
    let caller_team = control
        .actor_team
        .as_deref()
        .unwrap_or(control.team.as_str());
    let cross_team = caller_team.trim() != control.team.trim();
    if cross_team && !control.user_authorized {
        return Err(CiMonitorServiceError::new(
            "AUTHORIZATION_REQUIRED",
            "cross-team gh monitor control requires --user-authorized",
        ));
    }
    if cross_team
        && control
            .operator_reason
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
    {
        return Err(CiMonitorServiceError::new(
            "MISSING_PARAMETER",
            "cross-team gh monitor control requires a non-empty --reason",
        ));
    }
    let repo_scope = control
        .repo
        .as_deref()
        .or(config_state.owner_repo.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

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
            let drain_timeout_secs = control
                .drain_timeout_secs
                .unwrap_or(DEFAULT_DRAIN_TIMEOUT_SECS);
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
                tokio::time::sleep(std::time::Duration::from_millis(DRAIN_SLEEP_MS)).await;
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
            let health = set_gh_monitor_health_state(
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
            })?;
            if cross_team {
                notify_team_lead_of_monitor_control(
                    home,
                    control.actor.as_deref().unwrap_or("team-lead"),
                    caller_team,
                    &control.team,
                    "stopped",
                    control
                        .operator_reason
                        .as_deref()
                        .unwrap_or("operator-authorized cross-team stop"),
                )
                .map_err(|e| {
                    CiMonitorServiceError::internal(format!(
                        "failed to notify team lead about cross-team stop: {e}"
                    ))
                })?;
            }
            health
        }
        CiMonitorLifecycleAction::Restart => {
            let drain_timeout_secs = control
                .drain_timeout_secs
                .unwrap_or(DEFAULT_DRAIN_TIMEOUT_SECS);
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
                tokio::time::sleep(std::time::Duration::from_millis(DRAIN_SLEEP_MS)).await;
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

            let health = set_gh_monitor_health_state(
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
            })?;
            if cross_team {
                notify_team_lead_of_monitor_control(
                    home,
                    control.actor.as_deref().unwrap_or("team-lead"),
                    caller_team,
                    &control.team,
                    "restarted",
                    control
                        .operator_reason
                        .as_deref()
                        .unwrap_or("operator-authorized cross-team restart"),
                )
                .map_err(|e| {
                    CiMonitorServiceError::internal(format!(
                        "failed to notify team lead about cross-team restart: {e}"
                    ))
                })?;
            }
            health
        }
    };

    let mut health = health;
    if let Some(repo_scope) = repo_scope.as_deref()
        && let Ok(Some(repo_state)) = read_gh_repo_state_record(home, &control.team, repo_scope)
    {
        let observer_ctx = GhCliObserverContext {
            home: home.to_path_buf(),
            team: control.team.clone(),
            repo: repo_scope.to_string(),
            runtime: "atm-daemon".to_string(),
        };
        let request_id = new_gh_info_request_id();
        emit_gh_info_requested(&observer_ctx, &request_id, "gh_monitor_control_health");
        emit_gh_info_served_from_cache(
            &observer_ctx,
            &request_id,
            "gh_monitor_control_health",
            gh_repo_state_cache_age_secs(&repo_state),
        );
        apply_repo_state_to_health(&mut health, &repo_state);
    }
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
        && let Ok(Some(repo_state)) = read_gh_repo_state_record(home, team, repo_scope)
    {
        let observer_ctx = GhCliObserverContext {
            home: home.to_path_buf(),
            team: team.to_string(),
            repo: repo_scope.to_string(),
            runtime: "atm-daemon".to_string(),
        };
        let request_id = new_gh_info_request_id();
        emit_gh_info_requested(&observer_ctx, &request_id, "gh_monitor_health");
        emit_gh_info_served_from_cache(
            &observer_ctx,
            &request_id,
            "gh_monitor_health",
            gh_repo_state_cache_age_secs(&repo_state),
        );
        apply_repo_state_to_health(&mut health, &repo_state);
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

#[cfg(test)]
mod tests {
    use super::{build_shared_poller_plan, health_request, sync_headroom_suppression_state};
    use agent_team_mail_ci_monitor::{
        CiMonitorHealth, CiMonitorStatus, CiMonitorTargetKind, GhObservedCall, GhRateLimitSnapshot,
        GhRepoStateFile, GhRepoStateRecord, GhRuntimeOwner,
        consts::{GH_MONITOR_HEADROOM_FLOOR, GH_MONITOR_HEADROOM_RECOVERY_FLOOR},
        read_gh_observability_records,
        repo_state::write_repo_state,
    };
    use chrono::{Duration, Utc};
    use std::fs;
    use tempfile::TempDir;

    fn write_plugin_config(workdir: &std::path::Path, team: &str) {
        fs::create_dir_all(workdir).unwrap();
        fs::write(
            workdir.join(".atm.toml"),
            format!(
                r#"[core]
default_team = "{team}"
identity = "team-lead"

[plugins.gh_monitor]
enabled = true
provider = "github"
team = "{team}"
agent = "gh-monitor"
owner = "acme"
repo = "agent-team-mail"
poll_interval_secs = 60
"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn health_request_emits_cache_freshness_without_live_gh_call() {
        let temp = TempDir::new().unwrap();
        let workdir = temp.path().join("workdir");
        write_plugin_config(&workdir, "atm-dev");
        let now = Utc::now();
        write_repo_state(
            temp.path(),
            &GhRepoStateFile {
                records: vec![GhRepoStateRecord {
                    team: "atm-dev".to_string(),
                    repo: "acme/agent-team-mail".to_string(),
                    updated_at: now.to_rfc3339(),
                    cache_expires_at: (now + Duration::minutes(5)).to_rfc3339(),
                    last_refresh_at: Some((now - Duration::seconds(30)).to_rfc3339()),
                    budget_limit_per_hour: 100,
                    budget_used_in_window: 3,
                    budget_window_started_at: now.to_rfc3339(),
                    budget_warning_threshold: 75,
                    warning_emitted_at: None,
                    blocked: false,
                    in_flight: 0,
                    idle_poll_interval_secs: 300,
                    active_poll_interval_secs: 60,
                    branch_ref_counts: Vec::new(),
                    last_call: Some(GhObservedCall {
                        action: "gh_pr_list".to_string(),
                        branch: None,
                        reference: None,
                        duration_ms: 12,
                        success: true,
                        error: None,
                        at: now.to_rfc3339(),
                    }),
                    rate_limit: Some(GhRateLimitSnapshot {
                        remaining: 4990,
                        limit: 5000,
                        updated_at: now.to_rfc3339(),
                        reset_at: None,
                        source: "cache".to_string(),
                    }),
                    owner: Some(GhRuntimeOwner {
                        runtime: "dev".to_string(),
                        executable_path: "/tmp/fake-atm-daemon".to_string(),
                        home_scope: temp.path().display().to_string(),
                        pid: std::process::id(),
                    }),
                }],
            },
        )
        .unwrap();

        let health = health_request(
            temp.path(),
            "atm-dev",
            Some(workdir.to_string_lossy().as_ref()),
            Some("acme/agent-team-mail"),
        )
        .unwrap();
        assert_eq!(health.owner_repo.as_deref(), Some("acme/agent-team-mail"));

        let records = read_gh_observability_records(temp.path()).unwrap();
        assert!(
            records
                .iter()
                .any(|record| record.action == "gh_info_requested"),
            "health requests must emit gh_info_requested"
        );
        assert!(
            records
                .iter()
                .any(|record| record.action == "gh_info_served_from_cache"),
            "health requests with cached repo-state must emit gh_info_served_from_cache"
        );
        assert!(
            !records
                .iter()
                .any(|record| record.action == "gh_call_started"),
            "cache-only health requests must not emit gh_call_started"
        );
    }

    fn active_monitor_status(team: &str) -> CiMonitorStatus {
        CiMonitorStatus {
            team: team.to_string(),
            configured: true,
            enabled: true,
            config_source: None,
            config_path: None,
            target_kind: CiMonitorTargetKind::Pr,
            target: "101".to_string(),
            state: "monitoring".to_string(),
            run_id: Some(42),
            reference: Some("main".to_string()),
            updated_at: Utc::now().to_rfc3339(),
            message: None,
            repo_state_updated_at: None,
        }
    }

    fn running_health(team: &str, lifecycle_state: &str) -> CiMonitorHealth {
        CiMonitorHealth {
            team: team.to_string(),
            configured: true,
            enabled: true,
            config_source: None,
            config_path: None,
            lifecycle_state: lifecycle_state.to_string(),
            availability_state: "healthy".to_string(),
            in_flight: 0,
            updated_at: Utc::now().to_rfc3339(),
            message: None,
            repo_state_updated_at: None,
            budget_limit_per_hour: None,
            budget_used_in_window: None,
            rate_limit_remaining: None,
            rate_limit_limit: None,
            rate_limit_reset_at: None,
            poll_owner: None,
            owner_runtime_kind: None,
            owner_pid: None,
            owner_binary_path: None,
            owner_atm_home: None,
            owner_repo: None,
            owner_poll_interval_secs: None,
        }
    }

    fn repo_state_record(
        home: &std::path::Path,
        remaining: u64,
        blocked: bool,
    ) -> GhRepoStateRecord {
        let now = Utc::now();
        GhRepoStateRecord {
            team: "atm-dev".to_string(),
            repo: "acme/agent-team-mail".to_string(),
            updated_at: now.to_rfc3339(),
            cache_expires_at: (now + Duration::minutes(5)).to_rfc3339(),
            last_refresh_at: Some((now - Duration::seconds(10)).to_rfc3339()),
            budget_limit_per_hour: 100,
            budget_used_in_window: 3,
            budget_window_started_at: now.to_rfc3339(),
            budget_warning_threshold: 75,
            warning_emitted_at: None,
            blocked,
            in_flight: 0,
            idle_poll_interval_secs: 300,
            active_poll_interval_secs: 60,
            branch_ref_counts: Vec::new(),
            last_call: Some(GhObservedCall {
                action: "gh_pr_list".to_string(),
                branch: None,
                reference: None,
                duration_ms: 12,
                success: true,
                error: None,
                at: now.to_rfc3339(),
            }),
            rate_limit: Some(GhRateLimitSnapshot {
                remaining,
                limit: 5000,
                updated_at: now.to_rfc3339(),
                reset_at: None,
                source: "cache".to_string(),
            }),
            owner: Some(GhRuntimeOwner {
                runtime: "dev".to_string(),
                executable_path: "/tmp/fake-atm-daemon".to_string(),
                home_scope: home.display().to_string(),
                pid: std::process::id(),
            }),
        }
    }

    #[test]
    fn shared_poller_plan_suppresses_when_lifecycle_is_draining() {
        let temp = TempDir::new().unwrap();
        super::health::upsert_gh_monitor_health(temp.path(), running_health("atm-dev", "draining"))
            .unwrap();
        super::helpers::upsert_gh_monitor_status_for_repo(
            temp.path(),
            active_monitor_status("atm-dev"),
            Some("acme/agent-team-mail"),
        )
        .unwrap();

        let records = super::load_gh_monitor_state_records(temp.path()).unwrap();
        let plan =
            build_shared_poller_plan(temp.path(), "atm-dev", "acme/agent-team-mail", records);
        assert_eq!(plan.in_flight, 0);
        assert!(plan.active_records.is_empty());
        assert!(matches!(
            plan.suppression,
            Some(super::SharedPollerSuppression::Lifecycle { ref state }) if state == "draining"
        ));
        assert_eq!(super::count_in_flight_monitors(temp.path(), "atm-dev"), 0);
    }

    #[test]
    fn shared_poller_plan_suppresses_when_headroom_hits_floor() {
        let temp = TempDir::new().unwrap();
        super::health::upsert_gh_monitor_health(temp.path(), running_health("atm-dev", "running"))
            .unwrap();
        super::helpers::upsert_gh_monitor_status_for_repo(
            temp.path(),
            active_monitor_status("atm-dev"),
            Some("acme/agent-team-mail"),
        )
        .unwrap();
        write_repo_state(
            temp.path(),
            &GhRepoStateFile {
                records: vec![repo_state_record(
                    temp.path(),
                    GH_MONITOR_HEADROOM_FLOOR,
                    false,
                )],
            },
        )
        .unwrap();

        let records = super::load_gh_monitor_state_records(temp.path()).unwrap();
        let plan =
            build_shared_poller_plan(temp.path(), "atm-dev", "acme/agent-team-mail", records);
        assert_eq!(plan.in_flight, 0);
        assert!(plan.active_records.is_empty());
        assert!(matches!(
            plan.suppression,
            Some(super::SharedPollerSuppression::Headroom { remaining })
                if remaining == GH_MONITOR_HEADROOM_FLOOR
        ));

        sync_headroom_suppression_state(
            temp.path(),
            "atm-dev",
            "acme/agent-team-mail",
            &plan.suppression,
        );
        let health = super::health::read_gh_monitor_health(temp.path(), "atm-dev").unwrap();
        assert_eq!(health.availability_state, "degraded");
        assert!(
            health
                .message
                .as_deref()
                .is_some_and(|message| message.contains("shared gh polling paused")),
        );
        assert_eq!(super::count_in_flight_monitors(temp.path(), "atm-dev"), 0);
    }

    #[test]
    fn shared_poller_plan_only_recovers_after_headroom_recovery_floor() {
        let temp = TempDir::new().unwrap();
        super::health::upsert_gh_monitor_health(temp.path(), running_health("atm-dev", "running"))
            .unwrap();
        super::helpers::upsert_gh_monitor_status_for_repo(
            temp.path(),
            active_monitor_status("atm-dev"),
            Some("acme/agent-team-mail"),
        )
        .unwrap();
        write_repo_state(
            temp.path(),
            &GhRepoStateFile {
                records: vec![repo_state_record(
                    temp.path(),
                    GH_MONITOR_HEADROOM_RECOVERY_FLOOR - 1,
                    true,
                )],
            },
        )
        .unwrap();

        let suppressed = build_shared_poller_plan(
            temp.path(),
            "atm-dev",
            "acme/agent-team-mail",
            super::load_gh_monitor_state_records(temp.path()).unwrap(),
        );
        assert!(matches!(
            suppressed.suppression,
            Some(super::SharedPollerSuppression::Headroom { remaining })
                if remaining == GH_MONITOR_HEADROOM_RECOVERY_FLOOR - 1
        ));

        write_repo_state(
            temp.path(),
            &GhRepoStateFile {
                records: vec![repo_state_record(
                    temp.path(),
                    GH_MONITOR_HEADROOM_RECOVERY_FLOOR,
                    true,
                )],
            },
        )
        .unwrap();
        let resumed = build_shared_poller_plan(
            temp.path(),
            "atm-dev",
            "acme/agent-team-mail",
            super::load_gh_monitor_state_records(temp.path()).unwrap(),
        );
        assert!(resumed.suppression.is_none());
        assert_eq!(resumed.in_flight, 1);
    }
}
