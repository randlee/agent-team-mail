#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use agent_team_mail_core::daemon_client::{GhMonitorStatus, GhMonitorTargetKind};
#[cfg(unix)]
use anyhow::Result;

#[cfg(unix)]
use super::types::{GhMonitorConfigState, GhMonitorStateFile};

#[cfg(unix)]
pub(crate) fn count_in_flight_monitors(home: &Path, team: &str) -> u64 {
    load_gh_monitor_state_map(home)
        .ok()
        .map(|map| {
            map.values()
                .filter(|status| status.team == team && status.state == "tracking")
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

    state
}

#[cfg(unix)]
pub(crate) fn apply_config_state_to_status(
    status: &mut GhMonitorStatus,
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
    target_kind: GhMonitorTargetKind,
    target: &str,
    reference: Option<&str>,
) -> String {
    let kind = match target_kind {
        GhMonitorTargetKind::Pr => "pr",
        GhMonitorTargetKind::Workflow => "workflow",
        GhMonitorTargetKind::Run => "run",
    };
    let reference = reference.unwrap_or_default();
    format!(
        "{}|{}|{}|{}",
        team.trim(),
        kind,
        target.trim(),
        reference.trim()
    )
}

#[cfg(unix)]
pub(crate) fn load_gh_monitor_state_map(home: &Path) -> Result<HashMap<String, GhMonitorStatus>> {
    let path = gh_monitor_state_path(home);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    let state = serde_json::from_str::<GhMonitorStateFile>(&raw)?;
    let mut map = HashMap::new();
    for record in state.records {
        let key = gh_monitor_key(
            &record.team,
            record.target_kind,
            &record.target,
            record.reference.as_deref(),
        );
        map.insert(key, record);
    }
    Ok(map)
}

#[cfg(unix)]
pub(crate) fn upsert_gh_monitor_status(home: &Path, status: GhMonitorStatus) -> Result<()> {
    let mut map = load_gh_monitor_state_map(home)?;
    let key = gh_monitor_key(
        &status.team,
        status.target_kind,
        &status.target,
        status.reference.as_deref(),
    );
    map.insert(key, status);
    let mut records: Vec<GhMonitorStatus> = map.into_values().collect();
    records.sort_by(|a, b| {
        let ak = gh_monitor_key(&a.team, a.target_kind, &a.target, a.reference.as_deref());
        let bk = gh_monitor_key(&b.team, b.target_kind, &b.target, b.reference.as_deref());
        ak.cmp(&bk)
    });
    let state = GhMonitorStateFile { records };
    let path = gh_monitor_state_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&state)?)?;
    Ok(())
}
#[cfg(all(test, unix))]
mod tests {
    use super::{gh_monitor_key, normalize_repo_scope};
    use agent_team_mail_core::daemon_client::GhMonitorTargetKind;

    #[test]
    #[cfg(unix)]
    fn test_normalize_repo_scope_combines_owner_and_repo() {
        assert_eq!(
            normalize_repo_scope(Some("OpenAI"), Some("Agent-Team-Mail")),
            Some("openai/agent-team-mail".to_string())
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_normalize_repo_scope_preserves_repo_slug_without_owner() {
        assert_eq!(
            normalize_repo_scope(None, Some("OpenAI/Agent-Team-Mail")),
            Some("openai/agent-team-mail".to_string())
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_normalize_repo_scope_rejects_missing_repo() {
        assert_eq!(normalize_repo_scope(Some("OpenAI"), Some("   ")), None);
    }

    #[test]
    #[cfg(unix)]
    fn test_gh_monitor_key_includes_kind_target_and_reference() {
        assert_eq!(
            gh_monitor_key(
                "atm-dev",
                GhMonitorTargetKind::Workflow,
                "ci",
                Some("release/v1")
            ),
            "atm-dev|workflow|ci|release/v1"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_gh_monitor_key_trims_inputs() {
        assert_eq!(
            gh_monitor_key(
                " atm-dev ",
                GhMonitorTargetKind::Pr,
                " 123 ",
                Some(" main ")
            ),
            "atm-dev|pr|123|main"
        );
    }
}
