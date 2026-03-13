#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use agent_team_mail_core::daemon_client::GhMonitorHealth;
#[cfg(unix)]
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
#[cfg(unix)]
use agent_team_mail_core::schema::InboxMessage;
#[cfg(unix)]
use anyhow::Result;
#[cfg(unix)]
use tracing::warn;

#[cfg(unix)]
use super::gh_monitor::resolve_ci_alert_routing;
#[cfg(unix)]
use super::types::{GhMonitorHealthFile, GhMonitorHealthUpdate};

#[cfg(unix)]
pub(crate) fn default_gh_monitor_health(team: &str) -> GhMonitorHealth {
    GhMonitorHealth {
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
    }
}

#[cfg(unix)]
fn gh_monitor_health_path(home: &Path) -> PathBuf {
    agent_team_mail_core::daemon_client::daemon_gh_monitor_health_path_for(home)
}

#[cfg(unix)]
pub(crate) fn load_gh_monitor_health_map(home: &Path) -> Result<HashMap<String, GhMonitorHealth>> {
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
pub(crate) fn upsert_gh_monitor_health(home: &Path, health: GhMonitorHealth) -> Result<()> {
    let mut map = load_gh_monitor_health_map(home)?;
    map.insert(health.team.clone(), health);
    let mut records: Vec<GhMonitorHealth> = map.into_values().collect();
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
pub(crate) fn read_gh_monitor_health(home: &Path, team: &str) -> Result<GhMonitorHealth> {
    let map = load_gh_monitor_health_map(home)?;
    Ok(map
        .get(team)
        .cloned()
        .unwrap_or_else(|| default_gh_monitor_health(team)))
}

#[cfg(unix)]
pub(crate) fn write_health_record(
    home: &Path,
    team: &str,
    availability_state: &str,
    message: &str,
) {
    let updated_record = GhMonitorHealth {
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
pub(crate) fn emit_gh_monitor_health_transition(
    home: &Path,
    team: &str,
    config_cwd: Option<&str>,
    old_state: &str,
    new_state: &str,
    reason: &str,
) {
    if old_state == new_state {
        return;
    }

    let level = if new_state == "healthy" {
        "info"
    } else {
        "warn"
    };
    emit_event_best_effort(EventFields {
        level,
        source: "atm-daemon",
        action: "gh_monitor_health_transition",
        team: Some(team.to_string()),
        result: Some(format!("{old_state}->{new_state}")),
        error: Some(reason.to_string()),
        ..Default::default()
    });

    let (from_agent, targets) = resolve_ci_alert_routing(home, team, config_cwd, None);
    let text = format!(
        "[gh_monitor] availability transition {} -> {}\nreason: {}",
        old_state, new_state, reason
    );
    for (agent, target_team) in targets {
        let inbox_path = home
            .join(".claude/teams")
            .join(&target_team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        let message = InboxMessage {
            from: from_agent.clone(),
            text: text.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(format!("gh_monitor: {new_state}")),
            message_id: Some(uuid::Uuid::new_v4().to_string()),
            unknown_fields: std::collections::HashMap::new(),
        };
        if let Err(e) = agent_team_mail_core::io::inbox::inbox_append(
            &inbox_path,
            &message,
            &target_team,
            &agent,
        ) {
            warn!(
                team = %target_team,
                agent = %agent,
                "failed to emit gh_monitor transition alert: {e}"
            );
        }
    }
}

#[cfg(unix)]
pub(crate) fn set_gh_monitor_health_state(
    home: &Path,
    team: &str,
    update: GhMonitorHealthUpdate<'_>,
) -> Result<GhMonitorHealth> {
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
