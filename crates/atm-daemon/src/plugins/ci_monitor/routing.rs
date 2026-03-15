//! Routing and notification policy for CI monitor alerts.

#[cfg(unix)]
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
#[cfg(unix)]
use agent_team_mail_core::schema::InboxMessage;
#[cfg(unix)]
use tracing::warn;

#[cfg(unix)]
use super::types::{CiMonitorStatus, GhAlertTargets};

#[cfg(unix)]
pub(crate) fn resolve_ci_alert_routing(
    home: &std::path::Path,
    team: &str,
    config_cwd: Option<&str>,
    expected_repo_slug: Option<&str>,
    alert_targets: GhAlertTargets<'_>,
) -> (String, Vec<(String, String)>) {
    super::gh_alerts::resolve_ci_alert_routing(
        home,
        team,
        config_cwd,
        expected_repo_slug,
        alert_targets,
    )
}

#[cfg(unix)]
pub(crate) fn notify_ci_not_started(
    home: &std::path::Path,
    status: &CiMonitorStatus,
    config_cwd: Option<&str>,
    repo_scope: Option<&str>,
    alert_targets: GhAlertTargets<'_>,
) {
    super::gh_alerts::emit_ci_not_started_alert(
        home,
        status,
        config_cwd,
        repo_scope,
        alert_targets,
    );
}

#[cfg(unix)]
pub(crate) fn notify_merge_conflict(
    home: &std::path::Path,
    status: &CiMonitorStatus,
    pr_url: Option<&str>,
    merge_state_status: &str,
    run_conclusion: Option<&str>,
    config_cwd: Option<&str>,
    alert_targets: GhAlertTargets<'_>,
) {
    super::gh_alerts::emit_merge_conflict_alert(
        home,
        status,
        pr_url,
        merge_state_status,
        run_conclusion,
        config_cwd,
        alert_targets,
    );
}

#[cfg(unix)]
#[allow(dead_code)]
pub(crate) fn notify_gh_monitor_health_transition(
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

    let (from_agent, targets) =
        resolve_ci_alert_routing(home, team, config_cwd, None, GhAlertTargets::default());
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
        if let Some(parent) = inbox_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if !inbox_path.exists() {
            let _ = std::fs::write(&inbox_path, "[]");
        }
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

#[cfg(all(test, unix))]
mod tests {
    use super::{notify_merge_conflict, resolve_ci_alert_routing};
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
    fn gh_multi_fr_3_and_4_route_to_caller_with_cc_recipients() {
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
notify_target = "team-lead"
"#,
        )
        .unwrap();

        let (_, targets) = resolve_ci_alert_routing(
            temp.path(),
            "atm-dev",
            Some(repo_dir.to_string_lossy().as_ref()),
            Some("randlee/agent-team-mail"),
            GhAlertTargets {
                caller_agent: Some("arch-ctm"),
                cc: &["qa-bot".to_string(), "ops@ops-team".to_string()],
            },
        );

        assert_eq!(
            targets,
            vec![
                ("arch-ctm".to_string(), "atm-dev".to_string()),
                ("qa-bot".to_string(), "atm-dev".to_string()),
                ("ops".to_string(), "ops-team".to_string()),
            ]
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

        notify_merge_conflict(
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
