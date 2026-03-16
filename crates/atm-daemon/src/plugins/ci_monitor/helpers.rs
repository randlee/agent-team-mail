#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use agent_team_mail_ci_monitor::consts::{
    GH_MONITOR_HEADROOM_FLOOR, GH_MONITOR_HEADROOM_RECOVERY_FLOOR,
};
#[cfg(unix)]
use agent_team_mail_ci_monitor::read_gh_repo_state_record;
use agent_team_mail_core::daemon_client::isolated_runtime_allows_live_github;
#[cfg(unix)]
use anyhow::Result;

#[cfg(unix)]
use super::types::{
    CiMonitorStatus, CiMonitorTargetKind, GhMonitorConfigState, GhMonitorStateFile,
    GhMonitorStateRecord,
};

#[cfg(unix)]
pub(crate) fn lifecycle_state_allows_polling(state: &str) -> bool {
    state.trim().eq_ignore_ascii_case("running")
}

#[cfg(unix)]
pub(crate) fn current_monitor_lifecycle_state(home: &Path, team: &str) -> Option<String> {
    super::health::read_gh_monitor_health(home, team)
        .ok()
        .map(|health| health.lifecycle_state)
}

#[cfg(unix)]
pub(crate) fn repo_state_polling_suppressed(
    record: &agent_team_mail_ci_monitor::GhRepoStateRecord,
) -> bool {
    if record.budget_used_in_window >= record.budget_limit_per_hour {
        return true;
    }
    let Some(rate_limit) = record.rate_limit.as_ref() else {
        return record.blocked;
    };
    if record.blocked {
        rate_limit.remaining < GH_MONITOR_HEADROOM_RECOVERY_FLOOR
    } else {
        rate_limit.remaining <= GH_MONITOR_HEADROOM_FLOOR
    }
}

#[cfg(unix)]
pub(crate) fn count_in_flight_monitors(home: &Path, team: &str) -> u64 {
    if current_monitor_lifecycle_state(home, team)
        .as_deref()
        .is_some_and(|state| !lifecycle_state_allows_polling(state))
    {
        return 0;
    }

    load_gh_monitor_state_records(home)
        .ok()
        .map(|records| {
            records
                .into_iter()
                .filter(|record| {
                    record.status.team == team
                        && matches!(record.status.state.as_str(), "tracking" | "monitoring")
                        && record.repo_scope.as_deref().is_none_or(|repo_scope| {
                            read_gh_repo_state_record(home, team, repo_scope)
                                .ok()
                                .flatten()
                                .is_none_or(|repo_state| {
                                    !repo_state_polling_suppressed(&repo_state)
                                })
                        })
                })
                .count() as u64
        })
        .unwrap_or(0)
}

#[cfg(unix)]
pub(crate) fn evaluate_gh_monitor_config(
    home: &Path,
    team: &str,
    config_cwd: Option<&str>,
) -> GhMonitorConfigState {
    let current_dir = config_cwd
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.to_path_buf());
    let location = agent_team_mail_core::config::resolve_plugin_config_location(
        "gh_monitor",
        &current_dir,
        home,
    );

    let config = agent_team_mail_core::config::resolve_config(
        &agent_team_mail_core::config::ConfigOverrides {
            team: Some(team.to_string()),
            ..Default::default()
        },
        &current_dir,
        home,
    );
    let mut state = GhMonitorConfigState {
        configured: false,
        enabled: false,
        config_source: location.as_ref().map(|loc| loc.source.clone()),
        config_path: location
            .as_ref()
            .map(|loc| loc.path.to_string_lossy().to_string()),
        configured_team: None,
        owner_repo: None,
        error: None,
    };

    let config = match config {
        Ok(config) => config,
        Err(e) => {
            state.error = Some(e.to_string());
            return state;
        }
    };

    let Some(table) = config.plugin_config("gh_monitor") else {
        state.error = Some("missing [plugins.gh_monitor] configuration".to_string());
        return state;
    };
    state.configured = true;

    let parsed = match crate::plugins::ci_monitor::CiMonitorConfig::from_toml(table) {
        Ok(parsed) => parsed,
        Err(e) => {
            state.error = Some(e.to_string());
            return state;
        }
    };
    state.enabled = parsed.enabled;
    state.configured_team = Some(parsed.team.clone());
    state.owner_repo = normalize_repo_scope(parsed.owner.as_deref(), parsed.repo.as_deref());

    if !parsed.enabled {
        state.error = Some("gh_monitor plugin disabled in configuration".to_string());
        return state;
    }

    if parsed
        .repo
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        state.error = Some(
            "gh_monitor configuration missing required field: repo (run `atm gh init`)".to_string(),
        );
        return state;
    }

    match isolated_runtime_allows_live_github(home) {
        Ok(false) => {
            state.enabled = false;
            state.error = Some(
                "gh_monitor disabled in isolated runtime unless explicitly allowed".to_string(),
            );
            return state;
        }
        Ok(true) => {}
        Err(e) => {
            state.enabled = false;
            state.error = Some(format!("failed to resolve runtime GitHub policy: {e}"));
            return state;
        }
    }

    state
}

#[cfg(unix)]
pub(crate) fn apply_config_state_to_status(
    status: &mut CiMonitorStatus,
    config_state: &GhMonitorConfigState,
) {
    status.configured = config_state.configured;
    status.enabled = config_state.enabled;
    status.config_source = config_state.config_source.clone();
    status.config_path = config_state.config_path.clone();
}

#[cfg(unix)]
pub(crate) fn normalize_repo_scope(owner: Option<&str>, repo: Option<&str>) -> Option<String> {
    let repo = repo.map(str::trim).filter(|repo| !repo.is_empty())?;
    if repo.contains('/') {
        return Some(repo.to_lowercase());
    }
    owner
        .map(str::trim)
        .filter(|owner| !owner.is_empty())
        .map(|owner| format!("{}/{}", owner.to_lowercase(), repo.to_lowercase()))
        .or_else(|| Some(repo.to_lowercase()))
}

#[cfg(unix)]
fn gh_monitor_state_path(home: &Path) -> PathBuf {
    home.join(".atm/daemon/gh-monitor-state.json")
}

#[cfg(unix)]
pub(crate) fn gh_monitor_key(
    team: &str,
    target_kind: CiMonitorTargetKind,
    target: &str,
    reference: Option<&str>,
    repo_scope: Option<&str>,
) -> String {
    let kind = match target_kind {
        CiMonitorTargetKind::Pr => "pr",
        CiMonitorTargetKind::Workflow => "workflow",
        CiMonitorTargetKind::Run => "run",
    };
    let reference = reference.unwrap_or_default();
    let repo_scope = repo_scope.unwrap_or_default();
    format!(
        "{}|{}|{}|{}|{}",
        team.trim(),
        kind,
        target.trim(),
        reference.trim(),
        repo_scope.trim().to_lowercase()
    )
}

#[cfg(unix)]
pub(crate) fn load_gh_monitor_state_map(home: &Path) -> Result<HashMap<String, CiMonitorStatus>> {
    Ok(load_gh_monitor_state_records(home)?
        .into_iter()
        .map(|record| {
            let key = gh_monitor_key(
                &record.status.team,
                record.status.target_kind,
                &record.status.target,
                record.status.reference.as_deref(),
                record.repo_scope.as_deref(),
            );
            (key, record.status)
        })
        .collect())
}

#[cfg(unix)]
pub(crate) fn load_gh_monitor_state_records(home: &Path) -> Result<Vec<GhMonitorStateRecord>> {
    let path = gh_monitor_state_path(home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    let state = serde_json::from_str::<GhMonitorStateFile>(&raw)?;
    Ok(state.records)
}

#[cfg(all(test, unix))]
pub(crate) fn upsert_gh_monitor_status(home: &Path, status: CiMonitorStatus) -> Result<()> {
    upsert_gh_monitor_status_for_repo(home, status, None)
}

#[cfg(unix)]
pub(crate) fn upsert_gh_monitor_status_for_repo(
    home: &Path,
    mut status: CiMonitorStatus,
    repo_scope: Option<&str>,
) -> Result<()> {
    if let Some(repo_scope) = repo_scope
        && let Ok(Some(repo_state)) = read_gh_repo_state_record(home, &status.team, repo_scope)
    {
        status.repo_state_updated_at = Some(repo_state.updated_at);
    }

    let path = gh_monitor_state_path(home);
    let mut map: HashMap<String, GhMonitorStateRecord> = if path.exists() {
        let raw = std::fs::read_to_string(&path)?;
        let state = serde_json::from_str::<GhMonitorStateFile>(&raw)?;
        state
            .records
            .into_iter()
            .map(|record| {
                let key = gh_monitor_key(
                    &record.status.team,
                    record.status.target_kind,
                    &record.status.target,
                    record.status.reference.as_deref(),
                    record.repo_scope.as_deref(),
                );
                (key, record)
            })
            .collect()
    } else {
        HashMap::new()
    };
    let key = gh_monitor_key(
        &status.team,
        status.target_kind,
        &status.target,
        status.reference.as_deref(),
        repo_scope,
    );
    map.insert(
        key,
        GhMonitorStateRecord {
            repo_scope: repo_scope.map(|value| value.trim().to_lowercase()),
            status,
        },
    );
    let mut records: Vec<GhMonitorStateRecord> = map.into_values().collect();
    records.sort_by(|a, b| {
        let ak = gh_monitor_key(
            &a.status.team,
            a.status.target_kind,
            &a.status.target,
            a.status.reference.as_deref(),
            a.repo_scope.as_deref(),
        );
        let bk = gh_monitor_key(
            &b.status.team,
            b.status.target_kind,
            &b.status.target,
            b.status.reference.as_deref(),
            b.repo_scope.as_deref(),
        );
        ak.cmp(&bk)
    });
    let state = GhMonitorStateFile { records };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&state)?)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::evaluate_gh_monitor_config;

    #[test]
    fn test_evaluate_gh_monitor_config_disables_isolated_runtime_by_default() {
        let runtime = agent_team_mail_core::daemon_client::create_isolated_runtime_root(
            Some("ci-monitor"),
            std::time::Duration::from_secs(
                agent_team_mail_core::consts::ISOLATED_RUNTIME_DEFAULT_TTL_SECS,
            ),
            false,
        )
        .unwrap();

        std::fs::write(
            runtime.home.join(".atm.toml"),
            r#"
[core]
default_team = "atm-dev"
identity = "team-lead"

[plugins.gh_monitor]
enabled = true
team = "atm-dev"
repo = "randlee/agent-team-mail"
"#,
        )
        .unwrap();

        let state = evaluate_gh_monitor_config(&runtime.home, "atm-dev", None);
        assert!(state.configured, "config should still be discovered");
        assert!(!state.enabled, "isolated runtime must disable live polling");
        assert!(
            state
                .error
                .as_deref()
                .is_some_and(|msg| msg.contains("isolated runtime")),
            "isolated runtime rejection should be explicit"
        );
    }
}
