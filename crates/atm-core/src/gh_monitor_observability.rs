use agent_team_mail_ci_monitor::repo_state::{
    gh_repo_state_path_for as ci_repo_state_path_for, load_repo_state, repo_state_key,
    write_repo_state,
};
use agent_team_mail_ci_monitor::{
    CiProviderError, GhBranchRefCount, GhCliCallMetadata, GhCliCallOutcome, GhCliObserver,
    GhRateLimitSnapshot, GhRepoStateFile, GhRepoStateRecord, GhRuntimeOwner,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::event_log::{emit_event_best_effort, EventFields};

const GH_BUDGET_LIMIT_PER_HOUR: u64 = 100;
const GH_WARNING_THRESHOLD: u64 = 50;
const GH_REPO_STATE_TTL_SECS: i64 = 300;
const GH_ACTIVE_POLL_INTERVAL_SECS: u64 = 60;
const GH_IDLE_POLL_INTERVAL_SECS: u64 = 300;

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
        let record = read_or_create_record(&self.ctx.home, &self.ctx.team, &self.ctx.repo)
            .map_err(|err| CiProviderError::runtime(err.to_string()))?;
        if record.budget_used_in_window >= record.budget_limit_per_hour {
            emit_rate_limit_event("rate_limit_critical", &self.ctx, metadata, &record, None);
            return Err(CiProviderError::provider(format!(
                "GitHub budget exhausted for team {} on repo {} ({}/{})",
                self.ctx.team,
                self.ctx.repo,
                record.budget_used_in_window,
                record.budget_limit_per_hour
            )));
        }
        Ok(())
    }

    fn after_gh_call(&self, outcome: &GhCliCallOutcome) {
        let _ = record_call_outcome(&self.ctx, outcome);
    }
}

pub fn build_gh_cli_observer(ctx: GhCliObserverContext) -> Arc<dyn GhCliObserver> {
    Arc::new(SharedGhCliObserver::new(ctx))
}

pub fn gh_repo_state_path_for(home: &Path) -> PathBuf {
    ci_repo_state_path_for(home)
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
    runtime: &str,
    remaining: u64,
    limit: u64,
    reset_at: Option<String>,
    source: &str,
) -> Result<GhRepoStateRecord> {
    mutate_record(home, team, repo, runtime, |record, now| {
        record.rate_limit = Some(GhRateLimitSnapshot {
            remaining,
            limit,
            updated_at: now.to_rfc3339(),
            reset_at,
            source: source.to_string(),
        });
        record.updated_at = now.to_rfc3339();
        record.cache_expires_at = (now + Duration::seconds(GH_REPO_STATE_TTL_SECS)).to_rfc3339();
    })
}

fn record_call_outcome(ctx: &GhCliObserverContext, outcome: &GhCliCallOutcome) -> Result<()> {
    let record = mutate_record(&ctx.home, &ctx.team, &ctx.repo, &ctx.runtime, |record, now| {
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
        record.cache_expires_at = (now + Duration::seconds(GH_REPO_STATE_TTL_SECS)).to_rfc3339();
    })?;

    emit_call_event(ctx, outcome, &record);

    if record.budget_used_in_window >= record.budget_limit_per_hour {
        emit_rate_limit_event(
            "rate_limit_critical",
            ctx,
            &outcome.metadata,
            &record,
            outcome.error.as_deref(),
        );
    } else if record.budget_used_in_window >= record.budget_warning_threshold
        && record.warning_emitted_at.is_some()
    {
        emit_rate_limit_event(
            "rate_limit_warning",
            ctx,
            &outcome.metadata,
            &record,
            outcome.error.as_deref(),
        );
    }

    Ok(())
}

fn emit_call_event(
    ctx: &GhCliObserverContext,
    outcome: &GhCliCallOutcome,
    record: &GhRepoStateRecord,
) {
    let mut extra = serde_json::Map::new();
    extra.insert("repo".to_string(), json!(ctx.repo));
    extra.insert("used_calls".to_string(), json!(record.budget_used_in_window));
    extra.insert("budget_limit_per_hour".to_string(), json!(record.budget_limit_per_hour));
    extra.insert("duration_ms".to_string(), json!(outcome.duration_ms));
    if let Some(branch) = outcome.metadata.branch.as_deref() {
        extra.insert("branch".to_string(), json!(branch));
    }
    if let Some(reference) = outcome.metadata.reference.as_deref() {
        extra.insert("reference".to_string(), json!(reference));
    }
    emit_event_best_effort(EventFields {
        level: if outcome.success { "info" } else { "warn" },
        source: "atm",
        action: "gh_api_call",
        team: Some(ctx.team.clone()),
        target: Some(ctx.repo.clone()),
        result: Some(if outcome.success { "success" } else { "failure" }.to_string()),
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
    extra.insert("repo".to_string(), json!(ctx.repo));
    extra.insert("used_calls".to_string(), json!(record.budget_used_in_window));
    extra.insert("budget_limit_per_hour".to_string(), json!(record.budget_limit_per_hour));
    if let Some(branch) = metadata.branch.as_deref() {
        extra.insert("branch".to_string(), json!(branch));
    }
    if let Some(reference) = metadata.reference.as_deref() {
        extra.insert("reference".to_string(), json!(reference));
    }
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
    record.owner = Some(runtime_owner(runtime, home));
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

fn read_or_create_record(home: &Path, team: &str, repo: &str) -> Result<GhRepoStateRecord> {
    mutate_record(home, team, repo, "atm", |_, _| {})
}

fn default_repo_state_record(team: &str, repo: &str, runtime: &str, home: &Path) -> GhRepoStateRecord {
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

    if record.budget_used_in_window + 1 >= record.budget_warning_threshold
        && record.warning_emitted_at.is_none()
    {
        record.warning_emitted_at = Some(now.to_rfc3339());
    }
}

fn bump_branch_ref_count(
    counts: &mut Vec<GhBranchRefCount>,
    branch: Option<&str>,
    reference: Option<&str>,
) {
    let branch = branch.map(str::trim).filter(|value| !value.is_empty());
    let reference = reference.map(str::trim).filter(|value| !value.is_empty());
    if let Some(bucket) = counts
        .iter_mut()
        .find(|bucket| bucket.branch.as_deref() == branch && bucket.reference.as_deref() == reference)
    {
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

fn purge_stale_records(records: &mut Vec<GhRepoStateRecord>) {
    let now = Utc::now();
    records.retain(|record| {
        parse_rfc3339(&record.cache_expires_at)
            .map(|expires_at| expires_at > now)
            .unwrap_or(true)
    });
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|ts| ts.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        mutate_record(temp.path(), "atm-dev", "owner/repo", "atm-daemon", |record, _| {
            record.budget_used_in_window = record.budget_limit_per_hour;
        })
        .unwrap();
        let err = observer
            .before_gh_call(&GhCliCallMetadata {
                repo_scope: "owner/repo".to_string(),
                action: "gh_run_list".to_string(),
                args: vec!["run".to_string(), "list".to_string()],
                branch: Some("main".to_string()),
                reference: None,
            })
            .unwrap_err();
        assert!(err.to_string().contains("budget exhausted"));
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
                    repo_scope: "owner/repo".to_string(),
                    action: "gh_run_list".to_string(),
                    args: vec!["run".to_string(), "list".to_string()],
                    branch: Some("develop".to_string()),
                    reference: Some("develop".to_string()),
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
    }
}
