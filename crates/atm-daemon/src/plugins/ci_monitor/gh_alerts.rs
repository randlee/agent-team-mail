//! GitHub monitor routing and notification helpers.

use super::CiMonitorConfig;
use super::helpers::normalize_repo_scope;
use super::types::{CiMonitorStatus, CiMonitorTargetKind, GhAlertTargets};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::schema::InboxMessage;
use tracing::warn;

#[cfg(unix)]
pub(crate) fn emit_ci_monitor_message(
    home: &std::path::Path,
    from_agent: &str,
    targets: &[(String, String)],
    summary: &str,
    text: &str,
    message_id: Option<String>,
) {
    for (agent, team) in targets {
        let inbox_path = home
            .join(".claude/teams")
            .join(team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        let message = InboxMessage {
            from: from_agent.to_string(),
            text: text.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(summary.to_string()),
            message_id: message_id.clone(),
            unknown_fields: std::collections::HashMap::new(),
        };
        if let Err(e) =
            agent_team_mail_core::io::inbox::inbox_append(&inbox_path, &message, team, agent)
        {
            warn!(
                team = %team,
                agent = %agent,
                "failed to emit ci monitor message: {e}"
            );
        }
    }
}

#[cfg(unix)]
pub(crate) fn emit_ci_not_started_alert(
    home: &std::path::Path,
    status: &CiMonitorStatus,
    config_cwd: Option<&str>,
    repo_scope: Option<&str>,
    targets: GhAlertTargets<'_>,
) {
    let (from_agent, routing_targets) =
        resolve_ci_alert_routing(home, &status.team, config_cwd, repo_scope, targets);
    let text = format!(
        "[ci_not_started] {} target '{}' did not produce a run in the start window.\n{}",
        match status.target_kind {
            CiMonitorTargetKind::Pr => "PR monitor",
            CiMonitorTargetKind::Workflow => "workflow monitor",
            CiMonitorTargetKind::Run => "run monitor",
        },
        status.target,
        status.message.clone().unwrap_or_default()
    );
    let summary = format!("ci_not_started: {}", status.target);
    for (agent, team) in routing_targets {
        let inbox_path = home
            .join(".claude/teams")
            .join(&team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        let message = InboxMessage {
            from: from_agent.clone(),
            text: text.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(summary.clone()),
            message_id: Some(uuid::Uuid::new_v4().to_string()),
            unknown_fields: std::collections::HashMap::new(),
        };
        if let Err(e) =
            agent_team_mail_core::io::inbox::inbox_append(&inbox_path, &message, &team, &agent)
        {
            warn!(
                team = %team,
                agent = %agent,
                "failed to emit ci_not_started alert: {e}"
            );
        }
    }
}

#[cfg(unix)]
pub(crate) fn emit_merge_conflict_alert(
    home: &std::path::Path,
    status: &CiMonitorStatus,
    pr_url: Option<&str>,
    merge_state_status: &str,
    run_conclusion: Option<&str>,
    config_cwd: Option<&str>,
    targets: GhAlertTargets<'_>,
) {
    let expected_repo = pr_url.and_then(super::gh_monitor::extract_repo_slug_from_url);
    let (from_agent, routing_targets) = resolve_ci_alert_routing(
        home,
        &status.team,
        config_cwd,
        expected_repo.as_deref(),
        targets,
    );
    let target_kind = match status.target_kind {
        CiMonitorTargetKind::Pr => "pr",
        CiMonitorTargetKind::Workflow => "workflow",
        CiMonitorTargetKind::Run => "run",
    };
    let mut text = format!(
        "[merge_conflict] Merge conflict detected for monitored target.\nclassification: merge_conflict\nstatus: merge_conflict\ntarget_kind: {target_kind}\ntarget: {}\npr_url: {}\nmerge_state_status: {}",
        status.target,
        pr_url.unwrap_or("(unknown)"),
        merge_state_status
    );
    if let Some(run_conclusion) = run_conclusion {
        text.push_str(&format!("\nrun_conclusion: {run_conclusion}"));
    }
    if let Some(message) = status.message.as_deref()
        && !message.trim().is_empty()
    {
        text.push_str(&format!("\nreason: {message}"));
    }

    let summary = format!("merge_conflict: {}", status.target);
    let mut extra_fields = serde_json::Map::new();
    extra_fields.insert(
        "classification".to_string(),
        serde_json::Value::String("merge_conflict".to_string()),
    );
    extra_fields.insert(
        "status".to_string(),
        serde_json::Value::String("merge_conflict".to_string()),
    );
    extra_fields.insert(
        "target_kind".to_string(),
        serde_json::Value::String(target_kind.to_string()),
    );
    extra_fields.insert(
        "pr_url".to_string(),
        serde_json::Value::String(pr_url.unwrap_or("(unknown)").to_string()),
    );
    extra_fields.insert(
        "merge_state_status".to_string(),
        serde_json::Value::String(merge_state_status.to_string()),
    );
    if let Some(run_conclusion) = run_conclusion {
        extra_fields.insert(
            "run_conclusion".to_string(),
            serde_json::Value::String(run_conclusion.to_string()),
        );
    }
    emit_event_best_effort(EventFields {
        level: "warn",
        source: "atm-daemon",
        action: "gh_monitor_merge_conflict",
        team: Some(status.team.clone()),
        target: Some(status.target.clone()),
        result: Some("merge_conflict".to_string()),
        error: Some(format!(
            "merge_state_status={}",
            merge_state_status.trim().to_uppercase()
        )),
        extra_fields,
        ..Default::default()
    });

    for (agent, team) in routing_targets {
        let inbox_path = home
            .join(".claude/teams")
            .join(&team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        let message = InboxMessage {
            from: from_agent.clone(),
            text: text.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(summary.clone()),
            message_id: Some(uuid::Uuid::new_v4().to_string()),
            unknown_fields: std::collections::HashMap::new(),
        };
        if let Err(e) =
            agent_team_mail_core::io::inbox::inbox_append(&inbox_path, &message, &team, &agent)
        {
            warn!(
                team = %team,
                agent = %agent,
                "failed to emit merge_conflict alert: {e}"
            );
        }
    }
}

#[cfg(unix)]
pub(crate) fn resolve_ci_alert_routing(
    home: &std::path::Path,
    team: &str,
    config_cwd: Option<&str>,
    expected_repo_slug: Option<&str>,
    alert_targets: GhAlertTargets<'_>,
) -> (String, Vec<(String, String)>) {
    let current_dir = config_cwd
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.to_path_buf());
    let config = match agent_team_mail_core::config::resolve_config(
        &agent_team_mail_core::config::ConfigOverrides {
            team: Some(team.to_string()),
            ..Default::default()
        },
        &current_dir,
        home,
    ) {
        Ok(cfg) => cfg,
        Err(_) => {
            return default_command_routing("gh-monitor", team, alert_targets);
        }
    };

    let plugin_table = config.plugin_config("gh_monitor");
    let Some(plugin_table) = plugin_table else {
        return default_command_routing("gh-monitor", team, alert_targets);
    };

    let parsed = match CiMonitorConfig::from_toml(plugin_table) {
        Ok(cfg) => cfg,
        Err(_) => {
            return default_command_routing("gh-monitor", team, alert_targets);
        }
    };

    let from_agent = if parsed.agent.trim().is_empty() {
        "gh-monitor".to_string()
    } else {
        parsed.agent
    };

    if parsed.team.trim() != team {
        warn!(
            expected_team = %team,
            configured_team = %parsed.team,
            "gh monitor routing blocked: configured team does not match request team"
        );
        return (from_agent, Vec::new());
    }

    if let Some(expected) = expected_repo_slug
        && !expected.trim().is_empty()
    {
        match normalize_repo_scope(parsed.owner.as_deref(), parsed.repo.as_deref()) {
            Some(configured) if !repo_scope_matches(&configured, expected) => {
                warn!(
                    expected_repo = %expected,
                    configured_repo = %configured,
                    "gh monitor routing blocked: configured repo does not match event repo"
                );
                return (from_agent, Vec::new());
            }
            None => {
                warn!(
                    expected_repo = %expected,
                    "gh monitor routing blocked: configured repo scope unavailable"
                );
                return (from_agent, Vec::new());
            }
            _ => {}
        }
    }

    let targets = if let Some(caller_agent) = alert_targets
        .caller_agent
        .map(str::trim)
        .filter(|caller| !caller.is_empty())
    {
        build_explicit_targets(parsed.team.as_str(), caller_agent, alert_targets.cc)
    } else if parsed.notify_target.is_empty() {
        fallback_config_identity(home, &current_dir)
            .map(|identity| build_explicit_targets(parsed.team.as_str(), identity.as_str(), &[]))
            .unwrap_or_else(|| vec![("team-lead".to_string(), parsed.team.clone())])
    } else {
        parsed
            .notify_target
            .into_iter()
            .map(|t| (t.agent, parsed.team.clone()))
            .collect()
    };
    (from_agent, targets)
}

#[cfg(unix)]
fn default_command_routing(
    from_agent: &str,
    team: &str,
    alert_targets: GhAlertTargets<'_>,
) -> (String, Vec<(String, String)>) {
    let targets = alert_targets
        .caller_agent
        .map(str::trim)
        .filter(|caller| !caller.is_empty())
        .map(|caller| build_explicit_targets(team, caller, alert_targets.cc))
        .unwrap_or_else(|| vec![("team-lead".to_string(), team.to_string())]);
    (from_agent.to_string(), targets)
}

#[cfg(unix)]
fn fallback_config_identity(
    home: &std::path::Path,
    current_dir: &std::path::Path,
) -> Option<String> {
    let location = agent_team_mail_core::config::resolve_plugin_config_location(
        "gh_monitor",
        current_dir,
        home,
    )?;
    let raw = std::fs::read_to_string(location.path).ok()?;
    let config = toml::from_str::<agent_team_mail_core::config::Config>(&raw).ok()?;
    let identity = config.core.identity.trim();
    if identity.is_empty() {
        None
    } else {
        Some(identity.to_string())
    }
}

#[cfg(unix)]
fn build_explicit_targets(team: &str, caller_agent: &str, cc: &[String]) -> Vec<(String, String)> {
    let mut targets = vec![(caller_agent.to_string(), team.to_string())];
    for entry in cc {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (agent, cc_team) = match trimmed.split_once('@') {
            Some((agent, cc_team)) if !agent.trim().is_empty() && !cc_team.trim().is_empty() => {
                (agent.trim().to_string(), cc_team.trim().to_string())
            }
            _ => (trimmed.to_string(), team.to_string()),
        };
        if !targets.iter().any(|(existing_agent, existing_team)| {
            existing_agent == &agent && existing_team == &cc_team
        }) {
            targets.push((agent, cc_team));
        }
    }
    targets
}

#[cfg(unix)]
pub(crate) fn repo_scope_matches(configured: &str, expected: &str) -> bool {
    let configured = configured.trim().to_lowercase();
    let expected = expected.trim().to_lowercase();
    if configured == expected {
        return true;
    }
    if configured.contains('/') {
        return false;
    }
    expected
        .split_once('/')
        .map(|(_, repo)| repo == configured)
        .unwrap_or(false)
}

#[cfg(all(test, unix))]
mod tests {
    use super::{emit_merge_conflict_alert, resolve_ci_alert_routing};
    use crate::plugins::ci_monitor::types::{CiMonitorStatus, CiMonitorTargetKind, GhAlertTargets};
    use agent_team_mail_core::schema::InboxMessage;
    use tempfile::TempDir;

    fn read_inbox(path: &std::path::Path) -> Vec<InboxMessage> {
        let raw = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn gh_ci_fr_2_default_routing_targets_team_lead_when_notify_target_missing() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(
            repo_dir.join(".atm.toml"),
            r#"[core]
default_team = "atm-dev"
identity = "team-lead"

[plugins.gh_monitor]
enabled = true
team = "atm-dev"
agent = "gh-monitor"
repo = "randlee/agent-team-mail"
"#,
        )
        .unwrap();

        let (from_agent, targets) = resolve_ci_alert_routing(
            temp.path(),
            "atm-dev",
            Some(repo_dir.to_string_lossy().as_ref()),
            Some("randlee/agent-team-mail"),
            GhAlertTargets::default(),
        );

        assert_eq!(from_agent, "gh-monitor");
        assert_eq!(
            targets,
            vec![("team-lead".to_string(), "atm-dev".to_string())]
        );
    }

    #[test]
    fn gh_ci_fr_17_merge_conflict_alert_includes_required_payload_fields() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("repo");
        let inbox_dir = temp.path().join(".claude/teams/atm-dev/inboxes");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::create_dir_all(&inbox_dir).unwrap();
        std::fs::write(
            repo_dir.join(".atm.toml"),
            r#"[core]
default_team = "atm-dev"
identity = "team-lead"

[plugins.gh_monitor]
enabled = true
team = "atm-dev"
agent = "gh-monitor"
repo = "randlee/agent-team-mail"
notify_target = "team-lead"
"#,
        )
        .unwrap();

        let status = CiMonitorStatus {
            team: "atm-dev".to_string(),
            configured: true,
            enabled: true,
            config_source: Some("repo".to_string()),
            config_path: Some(repo_dir.join(".atm.toml").to_string_lossy().to_string()),
            target_kind: CiMonitorTargetKind::Pr,
            target: "123".to_string(),
            state: "merge_conflict".to_string(),
            run_id: Some(99),
            reference: None,
            updated_at: chrono::Utc::now().to_rfc3339(),
            message: Some("preflight dirty".to_string()),
            repo_state_updated_at: None,
        };

        emit_merge_conflict_alert(
            temp.path(),
            &status,
            Some("https://github.com/randlee/agent-team-mail/pull/123"),
            "DIRTY",
            Some("failure"),
            Some(repo_dir.to_string_lossy().as_ref()),
            GhAlertTargets::default(),
        );

        let inbox = read_inbox(&inbox_dir.join("team-lead.json"));
        let message = inbox.last().expect("merge conflict alert");
        assert!(message.text.contains("classification: merge_conflict"));
        assert!(message.text.contains("status: merge_conflict"));
        assert!(
            message
                .text
                .contains("pr_url: https://github.com/randlee/agent-team-mail/pull/123")
        );
        assert!(message.text.contains("merge_state_status: DIRTY"));
    }
}
