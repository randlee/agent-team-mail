use agent_team_mail_ci_monitor::repo_state::{
    gh_repo_state_path_for as ci_repo_state_path_for, load_repo_state, repo_state_key,
    write_repo_state,
};
use agent_team_mail_ci_monitor::{
    CiProviderError, GhBranchRefCount, GhCliCallMetadata, GhCliCallOutcome, GhCliObserver,
    GhLedgerKind, GhLedgerRecord, GhRateLimitSnapshot, GhRepoStateFile, GhRepoStateRecord,
    GhRuntimeOwner, GitHubActionsProvider, append_gh_observability_record, new_gh_call_id,
    new_gh_request_id,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::consts::{
    GH_ACTIVE_POLL_INTERVAL_SECS, GH_BUDGET_LIMIT_PER_HOUR, GH_IDLE_POLL_INTERVAL_SECS,
    GH_REPO_STATE_TTL_SECS, GH_WARNING_THRESHOLD,
};
use crate::event_log::{EventFields, emit_event_best_effort};
use crate::io::inbox::inbox_append;
use crate::schema::InboxMessage;
use crate::team_config_store::TeamConfigStore;
#[cfg(test)]
const FAKE_FOREIGN_DAEMON_BINARY: &str = "fake-daemon-binary";

#[derive(Debug, Clone)]
pub struct GhCliObserverContext {
    pub home: PathBuf,
    pub team: String,
    pub repo: String,
    pub runtime: String,
}

#[derive(Debug, Clone)]
pub struct SharedGhCliObserver {
    ctx: GhCliObserverContext,
}

impl SharedGhCliObserver {
    pub fn new(ctx: GhCliObserverContext) -> Self {
        Self { ctx }
    }
}

impl GhCliObserver for SharedGhCliObserver {
    fn before_gh_call(&self, metadata: &GhCliCallMetadata) -> Result<(), CiProviderError> {
        let record = match read_or_create_record(&self.ctx.home, &self.ctx.team, &self.ctx.repo) {
            Ok(record) => record,
            Err(err) => {
                emit_execution_ledger_blocked(&self.ctx, metadata, None, &err.to_string());
                return Err(CiProviderError::runtime(err.to_string()));
            }
        };
        if record.budget_used_in_window >= record.budget_limit_per_hour {
            emit_rate_limit_event("rate_limit_critical", &self.ctx, metadata, &record, None);
            let blocked = format_firewall_blocked_reason(
                "budget_exhausted",
                "GitHub budget exhausted",
                json!({
                    "team": self.ctx.team,
                    "repo": self.ctx.repo,
                    "budget_used_in_window": record.budget_used_in_window,
                    "budget_limit_per_hour": record.budget_limit_per_hour,
                    "action": metadata.action,
                }),
            );
            emit_execution_ledger_blocked(&self.ctx, metadata, Some(&record), &blocked);
            return Err(CiProviderError::provider(blocked));
        }
        emit_execution_ledger_started(&self.ctx, metadata, &record);
        Ok(())
    }

    fn after_gh_call(&self, outcome: &GhCliCallOutcome) {
        let _ = record_call_outcome(&self.ctx, outcome);
    }
}

pub fn build_gh_cli_observer(ctx: GhCliObserverContext) -> Arc<dyn GhCliObserver> {
    Arc::new(SharedGhCliObserver::new(ctx))
}

pub fn run_attributed_gh_command(
    ctx: &GhCliObserverContext,
    action: &str,
    args: &[&str],
    branch: Option<&str>,
    reference: Option<&str>,
) -> Result<String> {
    run_attributed_gh_command_with_ids(
        ctx,
        action,
        args,
        branch,
        reference,
        new_gh_request_id(),
        new_gh_call_id(),
    )
}

pub fn run_attributed_gh_command_with_ids(
    ctx: &GhCliObserverContext,
    action: &str,
    args: &[&str],
    branch: Option<&str>,
    reference: Option<&str>,
    request_id: String,
    call_id: String,
) -> Result<String> {
    let observer: Arc<dyn GhCliObserver> = Arc::new(SharedGhCliObserver::new(ctx.clone()));
    let metadata = GhCliCallMetadata {
        request_id,
        call_id,
        repo_scope: ctx.repo.clone(),
        caller: action.to_string(),
        action: action.to_string(),
        args: args.iter().map(|value| (*value).to_string()).collect(),
        branch: branch.map(str::to_string),
        reference: reference.map(str::to_string),
        ledger_home: Some(ctx.home.clone()),
        team: Some(ctx.team.clone()),
        runtime: Some(ctx.runtime.clone()),
    };
    GitHubActionsProvider::run_gh_with_metadata_blocking(Some(observer), metadata)
        .map_err(|err| anyhow::anyhow!(err.to_string()))
}

pub fn gh_repo_state_path_for(home: &Path) -> PathBuf {
    ci_repo_state_path_for(home)
}

pub fn new_gh_info_request_id() -> String {
    new_gh_request_id()
}

pub fn emit_gh_info_requested(ctx: &GhCliObserverContext, request_id: &str, info_type: &str) {
    emit_freshness_record(
        ctx,
        GhLedgerRecord {
            request_id: Some(request_id.to_string()),
            caller: Some(info_type.to_string()),
            info_type: Some(info_type.to_string()),
            ..base_freshness_record("gh_info_requested", ctx)
        },
    );
}

pub fn emit_gh_info_served_from_cache(
    ctx: &GhCliObserverContext,
    request_id: &str,
    info_type: &str,
    cache_age_secs: Option<u64>,
) {
    emit_freshness_record(
        ctx,
        GhLedgerRecord {
            request_id: Some(request_id.to_string()),
            caller: Some(info_type.to_string()),
            info_type: Some(info_type.to_string()),
            cache_age_secs,
            result: Some("cache".to_string()),
            ..base_freshness_record("gh_info_served_from_cache", ctx)
        },
    );
}

pub fn emit_gh_info_live_refresh(
    ctx: &GhCliObserverContext,
    request_id: &str,
    info_type: &str,
    call_id: &str,
) {
    emit_freshness_record(
        ctx,
        GhLedgerRecord {
            request_id: Some(request_id.to_string()),
            caller: Some(info_type.to_string()),
            info_type: Some(info_type.to_string()),
            linked_call_ids: Some(vec![call_id.to_string()]),
            result: Some("live_refresh".to_string()),
            ..base_freshness_record("gh_info_live_refresh", ctx)
        },
    );
}

pub fn emit_gh_info_degraded(
    ctx: &GhCliObserverContext,
    request_id: &str,
    info_type: &str,
    reason: &str,
) {
    emit_freshness_record(
        ctx,
        GhLedgerRecord {
            request_id: Some(request_id.to_string()),
            caller: Some(info_type.to_string()),
            info_type: Some(info_type.to_string()),
            degraded_reason: Some(reason.to_string()),
            result: Some("degraded".to_string()),
            ..base_freshness_record("gh_info_degraded", ctx)
        },
    );
}

pub fn emit_gh_info_denied(
    ctx: &GhCliObserverContext,
    request_id: &str,
    info_type: &str,
    reason: &str,
) {
    emit_freshness_record(
        ctx,
        GhLedgerRecord {
            request_id: Some(request_id.to_string()),
            caller: Some(info_type.to_string()),
            info_type: Some(info_type.to_string()),
            degraded_reason: Some(reason.to_string()),
            result: Some("denied".to_string()),
            ..base_freshness_record("gh_info_denied", ctx)
        },
    );
}

pub fn read_gh_repo_state(home: &Path) -> Result<GhRepoStateFile> {
    load_repo_state(home).context("failed to load gh monitor repo-state")
}

pub fn read_gh_repo_state_record(
    home: &Path,
    team: &str,
    repo: &str,
) -> Result<Option<GhRepoStateRecord>> {
    let mut state = load_repo_state(home).context("failed to load gh monitor repo-state")?;
    purge_stale_records(&mut state.records);
    Ok(state
        .records
        .into_iter()
        .find(|record| record.team == team && record.repo.eq_ignore_ascii_case(repo)))
}

pub fn update_gh_repo_state_in_flight(
    home: &Path,
    team: &str,
    repo: &str,
    in_flight: u64,
    runtime: &str,
) -> Result<GhRepoStateRecord> {
    mutate_record(home, team, repo, runtime, |record, now| {
        record.in_flight = in_flight;
        record.updated_at = now.to_rfc3339();
        record.cache_expires_at = (now + Duration::seconds(GH_REPO_STATE_TTL_SECS)).to_rfc3339();
    })
}

pub fn update_gh_repo_state_rate_limit(
    home: &Path,
    team: &str,
    repo: &str,
    update: RateLimitUpdate<'_>,
) -> Result<GhRepoStateRecord> {
    mutate_record(home, team, repo, &update.runtime, |record, now| {
        record.rate_limit = Some(GhRateLimitSnapshot {
            remaining: update.remaining,
            limit: update.limit,
            updated_at: now.to_rfc3339(),
            reset_at: update.reset_at.clone(),
            source: update.source.to_string(),
        });
        record.updated_at = now.to_rfc3339();
        record.cache_expires_at = (now + Duration::seconds(GH_REPO_STATE_TTL_SECS)).to_rfc3339();
    })
}

pub fn gh_repo_state_cache_age_secs(record: &GhRepoStateRecord) -> Option<u64> {
    record
        .last_refresh_at
        .as_deref()
        .and_then(parse_rfc3339)
        .map(|last_refresh| {
            Utc::now()
                .signed_duration_since(last_refresh)
                .num_seconds()
                .max(0) as u64
        })
}

#[derive(Debug, Clone)]
pub struct RateLimitUpdate<'a> {
    pub runtime: String,
    pub remaining: u64,
    pub limit: u64,
    pub reset_at: Option<String>,
    pub source: &'a str,
}

fn record_call_outcome(ctx: &GhCliObserverContext, outcome: &GhCliCallOutcome) -> Result<()> {
    let mut warning_crossed = false;
    let record = mutate_record(
        &ctx.home,
        &ctx.team,
        &ctx.repo,
        &ctx.runtime,
        |record, now| {
            maybe_reset_budget_window(record, now);
            record.budget_used_in_window += 1;
            bump_branch_ref_count(
                &mut record.branch_ref_counts,
                outcome.metadata.branch.as_deref(),
                outcome.metadata.reference.as_deref(),
            );
            record.last_call = Some(agent_team_mail_ci_monitor::GhObservedCall {
                action: outcome.metadata.action.clone(),
                branch: outcome.metadata.branch.clone(),
                reference: outcome.metadata.reference.clone(),
                duration_ms: outcome.duration_ms,
                success: outcome.success,
                error: outcome.error.clone(),
                at: now.to_rfc3339(),
            });
            record.updated_at = now.to_rfc3339();
            record.last_refresh_at = Some(now.to_rfc3339());
            record.cache_expires_at =
                (now + Duration::seconds(GH_REPO_STATE_TTL_SECS)).to_rfc3339();
            if record.budget_used_in_window >= record.budget_warning_threshold
                && record.warning_emitted_at.is_none()
            {
                record.warning_emitted_at = Some(now.to_rfc3339());
                warning_crossed = true;
            }
        },
    )?;

    emit_execution_ledger_finished(ctx, outcome, &record);
    emit_call_event(ctx, outcome, &record);

    if record.budget_used_in_window >= record.budget_limit_per_hour {
        emit_rate_limit_event(
            "rate_limit_critical",
            ctx,
            &outcome.metadata,
            &record,
            outcome.error.as_deref(),
        );
    } else if warning_crossed {
        emit_rate_limit_event(
            "rate_limit_warning",
            ctx,
            &outcome.metadata,
            &record,
            outcome.error.as_deref(),
        );
        emit_budget_warning_message(ctx, &record, &outcome.metadata);
    }

    Ok(())
}

fn emit_execution_ledger_started(
    ctx: &GhCliObserverContext,
    metadata: &GhCliCallMetadata,
    record: &GhRepoStateRecord,
) {
    emit_execution_record(
        ctx,
        GhLedgerRecord {
            request_id: Some(metadata.request_id.clone()),
            call_id: Some(metadata.call_id.clone()),
            caller: Some(metadata.caller.clone()),
            argv: Some(metadata.args.clone()),
            branch: metadata.branch.clone(),
            reference: metadata.reference.clone(),
            in_flight: Some(record.in_flight),
            budget_used_in_window: Some(record.budget_used_in_window),
            budget_limit_per_hour: Some(record.budget_limit_per_hour),
            rate_limit_remaining: record.rate_limit.as_ref().map(|rate| rate.remaining),
            rate_limit_limit: record.rate_limit.as_ref().map(|rate| rate.limit),
            rate_limit_reset_at: record
                .rate_limit
                .as_ref()
                .and_then(|rate| rate.reset_at.clone()),
            result: Some("started".to_string()),
            ..base_execution_record("gh_call_started", ctx)
        },
    );
}

fn emit_execution_ledger_blocked(
    ctx: &GhCliObserverContext,
    metadata: &GhCliCallMetadata,
    record: Option<&GhRepoStateRecord>,
    error: &str,
) {
    emit_execution_record(
        ctx,
        GhLedgerRecord {
            request_id: Some(metadata.request_id.clone()),
            call_id: Some(metadata.call_id.clone()),
            caller: Some(metadata.caller.clone()),
            argv: Some(metadata.args.clone()),
            branch: metadata.branch.clone(),
            reference: metadata.reference.clone(),
            in_flight: record.map(|value| value.in_flight),
            budget_used_in_window: record.map(|value| value.budget_used_in_window),
            budget_limit_per_hour: record.map(|value| value.budget_limit_per_hour),
            rate_limit_remaining: record
                .and_then(|value| value.rate_limit.as_ref().map(|rate| rate.remaining)),
            rate_limit_limit: record
                .and_then(|value| value.rate_limit.as_ref().map(|rate| rate.limit)),
            rate_limit_reset_at: record.and_then(|value| {
                value
                    .rate_limit
                    .as_ref()
                    .and_then(|rate| rate.reset_at.clone())
            }),
            block_reason: extract_firewall_reason(error),
            error: Some(error.to_string()),
            result: Some("blocked".to_string()),
            ..base_execution_record("gh_call_blocked", ctx)
        },
    );
}

fn emit_execution_ledger_finished(
    ctx: &GhCliObserverContext,
    outcome: &GhCliCallOutcome,
    record: &GhRepoStateRecord,
) {
    emit_execution_record(
        ctx,
        GhLedgerRecord {
            request_id: Some(outcome.metadata.request_id.clone()),
            call_id: Some(outcome.metadata.call_id.clone()),
            caller: Some(outcome.metadata.caller.clone()),
            argv: Some(outcome.metadata.args.clone()),
            branch: outcome.metadata.branch.clone(),
            reference: outcome.metadata.reference.clone(),
            in_flight: Some(record.in_flight),
            budget_used_in_window: Some(record.budget_used_in_window),
            budget_limit_per_hour: Some(record.budget_limit_per_hour),
            rate_limit_remaining: record.rate_limit.as_ref().map(|rate| rate.remaining),
            rate_limit_limit: record.rate_limit.as_ref().map(|rate| rate.limit),
            rate_limit_reset_at: record
                .rate_limit
                .as_ref()
                .and_then(|rate| rate.reset_at.clone()),
            duration_ms: Some(outcome.duration_ms),
            error: outcome.error.clone(),
            result: Some(
                if outcome.success {
                    "success"
                } else {
                    "failure"
                }
                .to_string(),
            ),
            ..base_execution_record("gh_call_finished", ctx)
        },
    );
}

fn base_execution_record(action: &str, ctx: &GhCliObserverContext) -> GhLedgerRecord {
    let mut record = GhLedgerRecord::new(GhLedgerKind::Execution, action);
    record.team = Some(ctx.team.clone());
    record.repo = Some(ctx.repo.clone());
    record.runtime = Some(ctx.runtime.clone());
    record
}

fn base_freshness_record(action: &str, ctx: &GhCliObserverContext) -> GhLedgerRecord {
    let mut record = GhLedgerRecord::new(GhLedgerKind::Freshness, action);
    record.team = Some(ctx.team.clone());
    record.repo = Some(ctx.repo.clone());
    record.runtime = Some(ctx.runtime.clone());
    record
}

fn emit_execution_record(ctx: &GhCliObserverContext, record: GhLedgerRecord) {
    let _ = append_gh_observability_record(&ctx.home, &record);
}

fn emit_freshness_record(ctx: &GhCliObserverContext, record: GhLedgerRecord) {
    let _ = append_gh_observability_record(&ctx.home, &record);
}

fn extract_firewall_reason(error: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(error)
        .ok()
        .and_then(|value| {
            value
                .get("reason")
                .and_then(|reason| reason.as_str())
                .map(str::to_string)
        })
}

fn emit_call_event(
    ctx: &GhCliObserverContext,
    outcome: &GhCliCallOutcome,
    record: &GhRepoStateRecord,
) {
    let mut extra = serde_json::Map::new();
    extra.insert("repo".to_string(), json!(ctx.repo));
    extra.insert(
        "budget_used".to_string(),
        json!(record.budget_used_in_window),
    );
    extra.insert(
        "budget_limit".to_string(),
        json!(record.budget_limit_per_hour),
    );
    extra.insert("duration_ms".to_string(), json!(outcome.duration_ms));
    extra.insert("success".to_string(), json!(outcome.success));
    extra.insert("runtime_kind".to_string(), json!(ctx.runtime));
    extra.insert(
        "poll_interval_secs".to_string(),
        json!(current_poll_interval_secs(record)),
    );
    extra.insert(
        "gh_subcommand".to_string(),
        json!(format_gh_subcommand(&outcome.metadata)),
    );
    if let Some(owner) = record.owner.as_ref() {
        extra.insert("binary_path".to_string(), json!(owner.executable_path));
        extra.insert("pid".to_string(), json!(owner.pid));
    }
    if let Some(branch) = outcome.metadata.branch.as_deref() {
        extra.insert("branch".to_string(), json!(branch));
    }
    if let Some(reference) = outcome.metadata.reference.as_deref() {
        extra.insert("target_ref".to_string(), json!(reference));
    }
    emit_event_best_effort(EventFields {
        level: if outcome.success { "info" } else { "warn" },
        source: "atm",
        action: "gh_api_call",
        team: Some(ctx.team.clone()),
        target: Some(ctx.repo.clone()),
        result: Some(
            if outcome.success {
                "success"
            } else {
                "failure"
            }
            .to_string(),
        ),
        error: outcome.error.clone(),
        runtime: Some(ctx.runtime.clone()),
        count: Some(record.budget_used_in_window),
        extra_fields: extra,
        ..Default::default()
    });
}

fn emit_rate_limit_event(
    action: &'static str,
    ctx: &GhCliObserverContext,
    metadata: &GhCliCallMetadata,
    record: &GhRepoStateRecord,
    error: Option<&str>,
) {
    let mut extra = serde_json::Map::new();
    let fallback_reset_at = parse_rfc3339(&record.budget_window_started_at)
        .map(|started| (started + Duration::hours(1)).to_rfc3339());
    let remaining = record
        .rate_limit
        .as_ref()
        .map(|rate_limit| rate_limit.remaining)
        .unwrap_or_else(|| {
            record
                .budget_limit_per_hour
                .saturating_sub(record.budget_used_in_window)
        });
    let limit = record
        .rate_limit
        .as_ref()
        .map(|rate_limit| rate_limit.limit)
        .unwrap_or(record.budget_limit_per_hour);
    let reset_at = record
        .rate_limit
        .as_ref()
        .and_then(|rate_limit| rate_limit.reset_at.clone())
        .or(fallback_reset_at);
    extra.insert("repo".to_string(), json!(ctx.repo));
    extra.insert(
        "budget_used".to_string(),
        json!(record.budget_used_in_window),
    );
    extra.insert(
        "budget_limit".to_string(),
        json!(record.budget_limit_per_hour),
    );
    extra.insert(
        "budget_window".to_string(),
        json!(record.budget_window_started_at),
    );
    extra.insert("runtime_kind".to_string(), json!(ctx.runtime));
    extra.insert(
        "poll_interval_secs".to_string(),
        json!(current_poll_interval_secs(record)),
    );
    extra.insert(
        "gh_subcommand".to_string(),
        json!(format_gh_subcommand(metadata)),
    );
    if let Some(owner) = record.owner.as_ref() {
        extra.insert("binary_path".to_string(), json!(owner.executable_path));
        extra.insert("pid".to_string(), json!(owner.pid));
    }
    if let Some(branch) = metadata.branch.as_deref() {
        extra.insert("branch".to_string(), json!(branch));
    }
    if let Some(reference) = metadata.reference.as_deref() {
        extra.insert("target_ref".to_string(), json!(reference));
    }
    extra.insert("remaining".to_string(), json!(remaining));
    extra.insert("limit".to_string(), json!(limit));
    extra.insert("reset_at".to_string(), json!(reset_at));
    emit_event_best_effort(EventFields {
        level: if action == "rate_limit_critical" {
            "warn"
        } else {
            "info"
        },
        source: "atm",
        action,
        team: Some(ctx.team.clone()),
        target: Some(ctx.repo.clone()),
        runtime: Some(ctx.runtime.clone()),
        result: Some(action.to_string()),
        error: error.map(str::to_string),
        count: Some(record.budget_used_in_window),
        extra_fields: extra,
        ..Default::default()
    });
}

fn mutate_record<F>(
    home: &Path,
    team: &str,
    repo: &str,
    runtime: &str,
    mutator: F,
) -> Result<GhRepoStateRecord>
where
    F: FnOnce(&mut GhRepoStateRecord, DateTime<Utc>),
{
    let runtime_dir = home.join(".atm/daemon");
    std::fs::create_dir_all(&runtime_dir)?;
    let lock_path = runtime_dir.join("gh-monitor-repo-state.lock");
    let _guard = crate::io::lock::acquire_lock(&lock_path, 5)
        .map_err(|err| anyhow::anyhow!("failed to lock gh repo-state: {err}"))?;
    let now = Utc::now();
    let mut state = load_repo_state(home).context("failed to load gh monitor repo-state")?;
    purge_stale_records(&mut state.records);
    let key = repo_state_key(team, repo);
    let mut by_key = state
        .records
        .into_iter()
        .map(|record| (repo_state_key(&record.team, &record.repo), record))
        .collect::<std::collections::HashMap<_, _>>();
    let mut record = by_key
        .remove(&key)
        .unwrap_or_else(|| default_repo_state_record(team, repo, runtime, home));
    let current_owner = runtime_owner(runtime, home);
    if let Some(existing_owner) = record.owner.as_ref()
        && existing_owner.pid != current_owner.pid
        && owner_pid_alive(existing_owner.pid)
    {
        emit_event_best_effort(build_lease_conflict_event_fields(
            team,
            repo,
            &current_owner.runtime,
            existing_owner,
        ));
        anyhow::bail!(
            "{}",
            format_lease_conflict_error(team, repo, existing_owner)
        );
    }
    record.owner = Some(current_owner);
    mutator(&mut record, now);
    if record.budget_used_in_window >= record.budget_limit_per_hour {
        record.blocked = true;
    }
    by_key.insert(key, record.clone());
    let mut records: Vec<GhRepoStateRecord> = by_key.into_values().collect();
    records.sort_by(|a, b| a.team.cmp(&b.team).then(a.repo.cmp(&b.repo)));
    write_repo_state(home, &GhRepoStateFile { records })
        .context("failed to persist gh monitor repo-state")?;
    Ok(record)
}

fn build_lease_conflict_event_fields(
    team: &str,
    repo: &str,
    runtime: &str,
    existing_owner: &GhRuntimeOwner,
) -> EventFields {
    EventFields {
        level: "warn",
        source: "atm",
        action: "gh_monitor_lease_conflict",
        team: Some(team.to_string()),
        target: Some(repo.to_ascii_lowercase()),
        runtime: Some(runtime.to_string()),
        error: Some(format!(
            "repo lease already owned by pid {} at {}",
            existing_owner.pid, existing_owner.executable_path
        )),
        ..Default::default()
    }
}

fn format_lease_conflict_error(team: &str, repo: &str, existing_owner: &GhRuntimeOwner) -> String {
    format_firewall_blocked_reason(
        "lease_conflict",
        "gh_monitor lease conflict",
        json!({
            "team": team,
            "repo": repo,
            "owner_pid": existing_owner.pid,
            "owner_executable_path": existing_owner.executable_path,
            "owner_home_scope": existing_owner.home_scope,
        }),
    )
}

fn format_firewall_blocked_reason(
    reason: &str,
    message: &str,
    details: serde_json::Value,
) -> String {
    json!({
        "code": "gh_firewall_blocked",
        "reason": reason,
        "message": message,
        "details": details,
    })
    .to_string()
}

fn read_or_create_record(home: &Path, team: &str, repo: &str) -> Result<GhRepoStateRecord> {
    mutate_record(home, team, repo, "atm", |_, _| {})
}

fn default_repo_state_record(
    team: &str,
    repo: &str,
    runtime: &str,
    home: &Path,
) -> GhRepoStateRecord {
    let now = Utc::now();
    GhRepoStateRecord {
        team: team.to_string(),
        repo: repo.to_ascii_lowercase(),
        updated_at: now.to_rfc3339(),
        cache_expires_at: (now + Duration::seconds(GH_REPO_STATE_TTL_SECS)).to_rfc3339(),
        last_refresh_at: None,
        budget_limit_per_hour: GH_BUDGET_LIMIT_PER_HOUR,
        budget_used_in_window: 0,
        budget_window_started_at: now.to_rfc3339(),
        budget_warning_threshold: GH_WARNING_THRESHOLD,
        warning_emitted_at: None,
        blocked: false,
        in_flight: 0,
        idle_poll_interval_secs: GH_IDLE_POLL_INTERVAL_SECS,
        active_poll_interval_secs: GH_ACTIVE_POLL_INTERVAL_SECS,
        branch_ref_counts: Vec::new(),
        last_call: None,
        rate_limit: None,
        owner: Some(runtime_owner(runtime, home)),
    }
}

fn runtime_owner(runtime: &str, home: &Path) -> GhRuntimeOwner {
    if let Ok(owner) = crate::daemon_client::validate_runtime_admission_for_current_process(home) {
        return GhRuntimeOwner {
            runtime: owner.runtime_kind.as_str().to_string(),
            executable_path: owner.executable_path,
            home_scope: owner.home_scope,
            pid: std::process::id(),
        };
    }

    GhRuntimeOwner {
        runtime: runtime.to_string(),
        executable_path: std::env::current_exe()
            .ok()
            .and_then(|path| std::fs::canonicalize(path).ok())
            .unwrap_or_else(|| PathBuf::from("<unknown>"))
            .to_string_lossy()
            .to_string(),
        home_scope: std::fs::canonicalize(home)
            .unwrap_or_else(|_| home.to_path_buf())
            .to_string_lossy()
            .to_string(),
        pid: std::process::id(),
    }
}

#[cfg(unix)]
fn owner_pid_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn owner_pid_alive(pid: u32) -> bool {
    pid == std::process::id()
}

fn maybe_reset_budget_window(record: &mut GhRepoStateRecord, now: DateTime<Utc>) {
    let window_started_at = parse_rfc3339(&record.budget_window_started_at).unwrap_or(now);
    if now - window_started_at >= Duration::hours(1) {
        record.budget_used_in_window = 0;
        record.branch_ref_counts.clear();
        record.blocked = false;
        record.warning_emitted_at = None;
        record.budget_window_started_at = now.to_rfc3339();
    } else if record.budget_used_in_window < record.budget_limit_per_hour {
        record.blocked = false;
    }
}

fn current_poll_interval_secs(record: &GhRepoStateRecord) -> u64 {
    if record.in_flight > 0 {
        record.active_poll_interval_secs
    } else {
        record.idle_poll_interval_secs
    }
}

fn format_gh_subcommand(metadata: &GhCliCallMetadata) -> String {
    metadata
        .args
        .iter()
        .filter(|arg| !arg.starts_with('-') && !arg.contains('/'))
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

fn emit_budget_warning_message(
    ctx: &GhCliObserverContext,
    record: &GhRepoStateRecord,
    metadata: &GhCliCallMetadata,
) {
    let team_dir = ctx.home.join(".claude/teams").join(&ctx.team);
    let lead_agent = TeamConfigStore::open(&team_dir)
        .read()
        .ok()
        .and_then(|config| config.lead_agent_id.split('@').next().map(str::to_string))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "team-lead".to_string());
    let inbox_path = team_dir.join("inboxes").join(format!("{lead_agent}.json"));
    let message = InboxMessage {
        from: "gh_monitor".to_string(),
        text: format!(
            "GitHub monitor budget warning for {} on {}: {}/{} calls used in current window while running `{}`.",
            ctx.team,
            ctx.repo,
            record.budget_used_in_window,
            record.budget_limit_per_hour,
            format_gh_subcommand(metadata)
        ),
        timestamp: Utc::now().to_rfc3339(),
        read: false,
        summary: Some(format!("gh_monitor budget warning: {}", ctx.repo)),
        message_id: Some(format!(
            "gh-budget-warning-{}-{}",
            ctx.team,
            ctx.repo.replace('/', "-")
        )),
        unknown_fields: Default::default(),
    };
    let _ = inbox_append(&inbox_path, &message, &ctx.team, &lead_agent);
}

fn bump_branch_ref_count(
    counts: &mut Vec<GhBranchRefCount>,
    branch: Option<&str>,
    reference: Option<&str>,
) {
    let branch = branch.map(str::trim).filter(|value| !value.is_empty());
    let reference = reference.map(str::trim).filter(|value| !value.is_empty());
    if let Some(bucket) = counts.iter_mut().find(|bucket| {
        bucket.branch.as_deref() == branch && bucket.reference.as_deref() == reference
    }) {
        bucket.count += 1;
        return;
    }
    counts.push(GhBranchRefCount {
        branch: branch.map(str::to_string),
        reference: reference.map(str::to_string),
        count: 1,
    });
    counts.sort_by(|a, b| a.branch.cmp(&b.branch).then(a.reference.cmp(&b.reference)));
}

fn purge_stale_records(records: &mut [GhRepoStateRecord]) {
    let now = Utc::now();
    for record in records.iter_mut() {
        let is_stale = parse_rfc3339(&record.cache_expires_at)
            .map(|expires_at| expires_at <= now)
            .unwrap_or(false);
        if is_stale {
            evict_stale_cache_snapshot(record, now);
        }
    }
}

fn evict_stale_cache_snapshot(record: &mut GhRepoStateRecord, now: DateTime<Utc>) {
    record.cache_expires_at = now.to_rfc3339();
    record.last_refresh_at = None;
    record.in_flight = 0;
    record.last_call = None;
    record.rate_limit = None;
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|ts| ts.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_ci_monitor::read_gh_observability_records;
    #[cfg(unix)]
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn test_observer_blocks_after_budget_exhaustion() {
        let temp = TempDir::new().unwrap();
        let ctx = GhCliObserverContext {
            home: temp.path().to_path_buf(),
            team: "atm-dev".to_string(),
            repo: "owner/repo".to_string(),
            runtime: "atm-daemon".to_string(),
        };
        let observer = SharedGhCliObserver::new(ctx.clone());
        mutate_record(
            temp.path(),
            "atm-dev",
            "owner/repo",
            "atm-daemon",
            |record, _| {
                record.budget_used_in_window = record.budget_limit_per_hour;
            },
        )
        .unwrap();
        let err = observer
            .before_gh_call(&GhCliCallMetadata {
                request_id: new_gh_request_id(),
                call_id: new_gh_call_id(),
                repo_scope: "owner/repo".to_string(),
                caller: "gh_run_list".to_string(),
                action: "gh_run_list".to_string(),
                args: vec!["run".to_string(), "list".to_string()],
                branch: Some("main".to_string()),
                reference: None,
                ledger_home: Some(temp.path().to_path_buf()),
                team: Some("atm-dev".to_string()),
                runtime: Some("atm-daemon".to_string()),
            })
            .unwrap_err();
        assert!(err.to_string().contains("\"code\":\"gh_firewall_blocked\""));
        assert!(err.to_string().contains("\"reason\":\"budget_exhausted\""));
        let records = read_gh_observability_records(temp.path()).unwrap();
        assert!(
            records
                .iter()
                .any(|record| record.action == "gh_call_blocked"),
            "blocked requests must emit gh_call_blocked"
        );
        assert!(
            !records
                .iter()
                .any(|record| record.action == "gh_call_started"),
            "blocked requests must not emit gh_call_started"
        );
    }

    #[test]
    fn test_observer_records_branch_counts() {
        let temp = TempDir::new().unwrap();
        let ctx = GhCliObserverContext {
            home: temp.path().to_path_buf(),
            team: "atm-dev".to_string(),
            repo: "owner/repo".to_string(),
            runtime: "atm-daemon".to_string(),
        };
        record_call_outcome(
            &ctx,
            &GhCliCallOutcome {
                metadata: GhCliCallMetadata {
                    request_id: new_gh_request_id(),
                    call_id: new_gh_call_id(),
                    repo_scope: "owner/repo".to_string(),
                    caller: "gh_run_list".to_string(),
                    action: "gh_run_list".to_string(),
                    args: vec!["run".to_string(), "list".to_string()],
                    branch: Some("develop".to_string()),
                    reference: Some("develop".to_string()),
                    ledger_home: Some(temp.path().to_path_buf()),
                    team: Some("atm-dev".to_string()),
                    runtime: Some("atm-daemon".to_string()),
                },
                duration_ms: 42,
                success: true,
                error: None,
            },
        )
        .unwrap();
        let record = read_gh_repo_state_record(temp.path(), "atm-dev", "owner/repo")
            .unwrap()
            .expect("repo state");
        assert_eq!(record.budget_used_in_window, 1);
        assert_eq!(record.branch_ref_counts.len(), 1);
        assert_eq!(
            record.branch_ref_counts[0].branch.as_deref(),
            Some("develop")
        );
        let records = read_gh_observability_records(temp.path()).unwrap();
        assert!(
            records
                .iter()
                .any(|entry| entry.action == "gh_call_finished"),
            "successful calls must emit gh_call_finished"
        );
    }

    #[test]
    fn test_ttl_eviction_preserves_budget_state_and_owner() {
        let temp = TempDir::new().unwrap();
        let now = Utc::now();
        let stale = now - Duration::seconds(GH_REPO_STATE_TTL_SECS + 5);
        write_repo_state(
            temp.path(),
            &GhRepoStateFile {
                records: vec![GhRepoStateRecord {
                    team: "atm-dev".to_string(),
                    repo: "owner/repo".to_string(),
                    updated_at: stale.to_rfc3339(),
                    cache_expires_at: stale.to_rfc3339(),
                    last_refresh_at: Some(stale.to_rfc3339()),
                    budget_limit_per_hour: GH_BUDGET_LIMIT_PER_HOUR,
                    budget_used_in_window: 73,
                    budget_window_started_at: (now - Duration::minutes(20)).to_rfc3339(),
                    budget_warning_threshold: GH_WARNING_THRESHOLD,
                    warning_emitted_at: Some((now - Duration::minutes(10)).to_rfc3339()),
                    blocked: true,
                    in_flight: 2,
                    idle_poll_interval_secs: GH_IDLE_POLL_INTERVAL_SECS,
                    active_poll_interval_secs: GH_ACTIVE_POLL_INTERVAL_SECS,
                    branch_ref_counts: vec![GhBranchRefCount {
                        branch: Some("main".to_string()),
                        reference: Some("refs/heads/main".to_string()),
                        count: 73,
                    }],
                    last_call: Some(agent_team_mail_ci_monitor::GhObservedCall {
                        action: "gh_pr_list".to_string(),
                        branch: Some("main".to_string()),
                        reference: Some("refs/heads/main".to_string()),
                        duration_ms: 11,
                        success: true,
                        error: None,
                        at: stale.to_rfc3339(),
                    }),
                    rate_limit: Some(GhRateLimitSnapshot {
                        remaining: 12,
                        limit: 5000,
                        updated_at: stale.to_rfc3339(),
                        reset_at: Some((now + Duration::minutes(30)).to_rfc3339()),
                        source: "cache".to_string(),
                    }),
                    owner: Some(GhRuntimeOwner {
                        runtime: "dev".to_string(),
                        executable_path: "fake-daemon-binary".to_string(),
                        home_scope: temp.path().to_string_lossy().to_string(),
                        pid: 12345,
                    }),
                }],
            },
        )
        .unwrap();

        let record = read_gh_repo_state_record(temp.path(), "atm-dev", "owner/repo")
            .unwrap()
            .expect("repo state");

        assert_eq!(record.budget_used_in_window, 73);
        assert_eq!(record.budget_limit_per_hour, GH_BUDGET_LIMIT_PER_HOUR);
        assert_eq!(record.branch_ref_counts.len(), 1);
        assert!(record.blocked);
        assert_eq!(
            record.owner.as_ref().map(|owner| owner.pid),
            Some(12345),
            "ttl eviction must preserve owner visibility"
        );
        assert!(
            record.warning_emitted_at.is_some(),
            "ttl eviction must preserve warning_emitted_at"
        );
        assert_eq!(record.last_refresh_at, None);
        assert_eq!(record.in_flight, 0);
        assert!(record.last_call.is_none());
        assert!(record.rate_limit.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn test_observer_rejects_live_foreign_owner() {
        let temp = TempDir::new().unwrap();
        let mut child = Command::new("sleep").arg("2").spawn().unwrap();
        let now = Utc::now();
        write_repo_state(
            temp.path(),
            &GhRepoStateFile {
                records: vec![GhRepoStateRecord {
                    team: "atm-dev".to_string(),
                    repo: "owner/repo".to_string(),
                    updated_at: now.to_rfc3339(),
                    cache_expires_at: (now + Duration::seconds(GH_REPO_STATE_TTL_SECS))
                        .to_rfc3339(),
                    last_refresh_at: None,
                    budget_limit_per_hour: GH_BUDGET_LIMIT_PER_HOUR,
                    budget_used_in_window: 0,
                    budget_window_started_at: now.to_rfc3339(),
                    budget_warning_threshold: GH_WARNING_THRESHOLD,
                    warning_emitted_at: None,
                    blocked: false,
                    in_flight: 0,
                    idle_poll_interval_secs: GH_IDLE_POLL_INTERVAL_SECS,
                    active_poll_interval_secs: GH_ACTIVE_POLL_INTERVAL_SECS,
                    branch_ref_counts: Vec::new(),
                    last_call: None,
                    rate_limit: None,
                    owner: Some(GhRuntimeOwner {
                        runtime: "dev".to_string(),
                        executable_path: FAKE_FOREIGN_DAEMON_BINARY.to_string(),
                        home_scope: temp.path().to_string_lossy().to_string(),
                        pid: child.id(),
                    }),
                }],
            },
        )
        .unwrap();

        let ctx = GhCliObserverContext {
            home: temp.path().to_path_buf(),
            team: "atm-dev".to_string(),
            repo: "owner/repo".to_string(),
            runtime: "atm-daemon".to_string(),
        };
        let observer = SharedGhCliObserver::new(ctx);
        let err = observer
            .before_gh_call(&GhCliCallMetadata {
                request_id: new_gh_request_id(),
                call_id: new_gh_call_id(),
                repo_scope: "owner/repo".to_string(),
                caller: "gh_run_list".to_string(),
                action: "gh_run_list".to_string(),
                args: vec!["run".to_string(), "list".to_string()],
                branch: Some("main".to_string()),
                reference: None,
                ledger_home: Some(temp.path().to_path_buf()),
                team: Some("atm-dev".to_string()),
                runtime: Some("atm-daemon".to_string()),
            })
            .unwrap_err();
        assert!(err.to_string().contains("\"code\":\"gh_firewall_blocked\""));
        assert!(err.to_string().contains("\"reason\":\"lease_conflict\""));
        assert!(err.to_string().contains(&child.id().to_string()));
        assert!(err.to_string().contains(FAKE_FOREIGN_DAEMON_BINARY));
        let fields = build_lease_conflict_event_fields(
            "atm-dev",
            "owner/repo",
            "atm-daemon",
            &GhRuntimeOwner {
                runtime: "dev".to_string(),
                executable_path: FAKE_FOREIGN_DAEMON_BINARY.to_string(),
                home_scope: temp.path().to_string_lossy().to_string(),
                pid: child.id(),
            },
        );
        assert_eq!(fields.action, "gh_monitor_lease_conflict");
        assert_eq!(fields.team.as_deref(), Some("atm-dev"));
        assert_eq!(fields.target.as_deref(), Some("owner/repo"));
        assert_eq!(fields.runtime.as_deref(), Some("atm-daemon"));
        assert!(
            fields
                .error
                .as_deref()
                .unwrap_or_default()
                .contains(&child.id().to_string())
        );
        let _ = child.kill();
        let _ = child.wait();
    }
}
