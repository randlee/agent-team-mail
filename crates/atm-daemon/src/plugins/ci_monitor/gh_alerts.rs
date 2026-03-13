//! GitHub monitor routing and notification helpers.

use super::CiMonitorConfig;
use super::helpers::normalize_repo_scope;
use agent_team_mail_core::daemon_client::{GhMonitorStatus, GhMonitorTargetKind};
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
    status: &GhMonitorStatus,
    config_cwd: Option<&str>,
) {
    let (from_agent, targets) = resolve_ci_alert_routing(home, &status.team, config_cwd, None);
    let text = format!(
        "[ci_not_started] {} target '{}' did not produce a run in the start window.\n{}",
        match status.target_kind {
            GhMonitorTargetKind::Pr => "PR monitor",
            GhMonitorTargetKind::Workflow => "workflow monitor",
            GhMonitorTargetKind::Run => "run monitor",
        },
        status.target,
        status.message.clone().unwrap_or_default()
    );
    let summary = format!("ci_not_started: {}", status.target);
    for (agent, team) in targets {
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
    status: &GhMonitorStatus,
    pr_url: Option<&str>,
    merge_state_status: &str,
    run_conclusion: Option<&str>,
    config_cwd: Option<&str>,
) {
    let expected_repo = pr_url.and_then(super::gh_monitor::extract_repo_slug_from_url);
    let (from_agent, targets) =
        resolve_ci_alert_routing(home, &status.team, config_cwd, expected_repo.as_deref());
    let target_kind = match status.target_kind {
        GhMonitorTargetKind::Pr => "pr",
        GhMonitorTargetKind::Workflow => "workflow",
        GhMonitorTargetKind::Run => "run",
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

    for (agent, team) in targets {
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
            return (
                "gh-monitor".to_string(),
                vec![("team-lead".to_string(), team.to_string())],
            );
        }
    };

    let plugin_table = config.plugin_config("gh_monitor");
    let Some(plugin_table) = plugin_table else {
        return (
            "gh-monitor".to_string(),
            vec![("team-lead".to_string(), team.to_string())],
        );
    };

    let parsed = match CiMonitorConfig::from_toml(plugin_table) {
        Ok(cfg) => cfg,
        Err(_) => {
            return (
                "gh-monitor".to_string(),
                vec![("team-lead".to_string(), team.to_string())],
            );
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

    let targets = if parsed.notify_target.is_empty() {
        vec![("team-lead".to_string(), parsed.team.clone())]
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

#[cfg(unix)]
pub(crate) fn emit_gh_monitor_health_transition(
    home: &std::path::Path,
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
