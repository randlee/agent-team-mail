#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use agent_team_mail_ci_monitor::GhRepoStateRecord;
#[cfg(unix)]
use agent_team_mail_core::gh_monitor_observability::read_gh_repo_state_record;
#[cfg(unix)]
use agent_team_mail_core::pid::is_pid_alive;
#[cfg(unix)]
use anyhow::Result;
#[cfg(all(test, unix))]
use tracing::warn;

#[cfg(unix)]
use super::routing::notify_gh_monitor_health_transition as emit_gh_monitor_health_transition;
#[cfg(unix)]
use super::types::{CiMonitorHealth, GhMonitorHealthFile, GhMonitorHealthUpdate};

#[cfg(unix)]
pub(crate) fn default_gh_monitor_health(team: &str) -> CiMonitorHealth {
    CiMonitorHealth {
        team: team.to_string(),
        configured: false,
        enabled: false,
        config_source: None,
        config_path: None,
        lifecycle_state: "running".to_string(),
        availability_state: "healthy".to_string(),
        in_flight: 0,
        updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        message: None,
        repo_state_updated_at: None,
        budget_limit_per_hour: None,
        budget_used_in_window: None,
        rate_limit_remaining: None,
        rate_limit_limit: None,
        poll_owner: None,
        owner_runtime_kind: None,
        owner_pid: None,
        owner_binary_path: None,
        owner_atm_home: None,
        owner_repo: None,
        owner_poll_interval_secs: None,
    }
}

#[cfg(unix)]
pub(crate) fn apply_repo_state_to_health(
    current: &mut CiMonitorHealth,
    repo_state: &GhRepoStateRecord,
) {
    current.repo_state_updated_at = Some(repo_state.updated_at.clone());
    current.budget_limit_per_hour = Some(repo_state.budget_limit_per_hour);
    current.budget_used_in_window = Some(repo_state.budget_used_in_window);
    current.rate_limit_remaining = repo_state.rate_limit.as_ref().map(|rate| rate.remaining);
    current.rate_limit_limit = repo_state.rate_limit.as_ref().map(|rate| rate.limit);
    current.owner_repo = Some(repo_state.repo.clone());
    current.owner_poll_interval_secs = Some(if repo_state.in_flight > 0 {
        repo_state.active_poll_interval_secs
    } else {
        repo_state.idle_poll_interval_secs
    });
    if let Some(owner) = repo_state.owner.as_ref() {
        current.poll_owner = Some(format!(
            "{} pid={} runtime={} home={}",
            owner.executable_path, owner.pid, owner.runtime, owner.home_scope
        ));
        current.owner_runtime_kind = Some(owner.runtime.clone());
        current.owner_pid = Some(owner.pid);
        current.owner_binary_path = Some(owner.executable_path.clone());
        current.owner_atm_home = Some(owner.home_scope.clone());
    } else {
        current.poll_owner = None;
        current.owner_runtime_kind = None;
        current.owner_pid = None;
        current.owner_binary_path = None;
        current.owner_atm_home = None;
    }
    if let Some(conflict_message) = repo_state_owner_conflict_message(repo_state)
        && current.availability_state != "disabled_config_error"
    {
        current.availability_state = "degraded".to_string();
        current.message = Some(conflict_message);
    }
}

#[cfg(unix)]
fn repo_state_owner_conflict_message(repo_state: &GhRepoStateRecord) -> Option<String> {
    let owner = repo_state.owner.as_ref()?;
    if owner.pid == std::process::id() || !is_pid_alive(owner.pid) {
        return None;
    }
    Some(format!(
        "gh_monitor lease conflict for team={} repo={}: active owner pid={} executable={} home={}",
        repo_state.team, repo_state.repo, owner.pid, owner.executable_path, owner.home_scope
    ))
}

#[cfg(unix)]
fn gh_monitor_health_path(home: &Path) -> PathBuf {
    agent_team_mail_core::daemon_client::daemon_gh_monitor_health_path_for(home)
}

#[cfg(unix)]
pub(crate) fn load_gh_monitor_health_map(home: &Path) -> Result<HashMap<String, CiMonitorHealth>> {
    let path = gh_monitor_health_path(home);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    let file = serde_json::from_str::<GhMonitorHealthFile>(&raw)?;
    let mut map = HashMap::new();
    for record in file.records {
        map.insert(record.team.clone(), record);
    }
    Ok(map)
}

#[cfg(unix)]
pub(crate) fn upsert_gh_monitor_health(home: &Path, health: CiMonitorHealth) -> Result<()> {
    let mut map = load_gh_monitor_health_map(home)?;
    map.insert(health.team.clone(), health);
    let mut records: Vec<CiMonitorHealth> = map.into_values().collect();
    records.sort_by(|a, b| a.team.cmp(&b.team));
    let file = GhMonitorHealthFile { records };
    let path = gh_monitor_health_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&file)?)?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn read_gh_monitor_health(home: &Path, team: &str) -> Result<CiMonitorHealth> {
    let map = load_gh_monitor_health_map(home)?;
    Ok(map
        .get(team)
        .cloned()
        .unwrap_or_else(|| default_gh_monitor_health(team)))
}

#[cfg(all(test, unix))]
pub(crate) fn write_health_record(
    home: &Path,
    team: &str,
    availability_state: &str,
    message: &str,
) {
    let updated_record = CiMonitorHealth {
        team: team.to_string(),
        configured: false,
        enabled: false,
        config_source: None,
        config_path: None,
        lifecycle_state: "running".to_string(),
        availability_state: availability_state.to_string(),
        in_flight: 0,
        updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        message: Some(message.to_string()),
        repo_state_updated_at: None,
        budget_limit_per_hour: None,
        budget_used_in_window: None,
        rate_limit_remaining: None,
        rate_limit_limit: None,
        poll_owner: None,
        owner_runtime_kind: None,
        owner_pid: None,
        owner_binary_path: None,
        owner_atm_home: None,
        owner_repo: None,
        owner_poll_interval_secs: None,
    };

    if let Err(e) = upsert_gh_monitor_health(home, updated_record) {
        let path = gh_monitor_health_path(home);
        warn!(
            "CI Monitor: failed writing health file {}: {}",
            path.display(),
            e
        );
    }
}

#[cfg(unix)]
pub(crate) fn set_gh_monitor_health_state(
    home: &Path,
    team: &str,
    update: GhMonitorHealthUpdate<'_>,
) -> Result<CiMonitorHealth> {
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
        if let Some(repo_scope) = config_state.owner_repo.as_deref()
            && let Ok(Some(repo_state)) = read_gh_repo_state_record(home, team, repo_scope)
        {
            apply_repo_state_to_health(&mut current, &repo_state);
        }
    }
    current.updated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    current.message = update.message;

    if old_availability != current.availability_state {
        let reason = current
            .message
            .clone()
            .unwrap_or_else(|| "availability changed".to_string());
        emit_gh_monitor_health_transition(
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
mod tests {
    use super::{
        default_gh_monitor_health, read_gh_monitor_health, set_gh_monitor_health_state,
        write_health_record,
    };
    use crate::plugins::ci_monitor::types::GhMonitorHealthUpdate;
    use agent_team_mail_core::io::inbox::inbox_read_merged;
    use tempfile::TempDir;

    fn prepare_team(home: &std::path::Path, team: &str) {
        let inboxes = home.join(".claude/teams").join(team).join("inboxes");
        std::fs::create_dir_all(&inboxes).unwrap();
        std::fs::write(inboxes.join("team-lead.json"), "[]").unwrap();
    }

    #[test]
    fn test_write_health_record_persists_snapshot() {
        let temp = TempDir::new().unwrap();
        write_health_record(
            temp.path(),
            "atm-dev",
            "disabled_config_error",
            "missing repo config",
        );

        let health = read_gh_monitor_health(temp.path(), "atm-dev").unwrap();
        assert_eq!(health.team, "atm-dev");
        assert_eq!(health.availability_state, "disabled_config_error");
        assert_eq!(health.lifecycle_state, "running");
        assert_eq!(health.message.as_deref(), Some("missing repo config"));
    }

    #[test]
    fn test_set_gh_monitor_health_state_emits_transition_alert() {
        let temp = TempDir::new().unwrap();
        prepare_team(temp.path(), "atm-dev");

        let initial = default_gh_monitor_health("atm-dev");
        super::upsert_gh_monitor_health(temp.path(), initial).unwrap();

        let health = set_gh_monitor_health_state(
            temp.path(),
            "atm-dev",
            GhMonitorHealthUpdate {
                availability_state: Some("degraded"),
                message: Some("provider timeout".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(health.availability_state, "degraded");
        assert_eq!(health.message.as_deref(), Some("provider timeout"));

        let team_dir = temp.path().join(".claude/teams/atm-dev");
        let inbox = inbox_read_merged(&team_dir, "team-lead", None).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].summary.as_deref(), Some("gh_monitor: degraded"));
        assert!(inbox[0].text.contains("healthy -> degraded"));
    }
}
