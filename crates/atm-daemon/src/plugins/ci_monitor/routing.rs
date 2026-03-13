//! Routing and notification policy for CI monitor alerts.

#[cfg(unix)]
use super::alerts;
#[cfg(unix)]
use agent_team_mail_core::daemon_client::GhMonitorStatus;

#[cfg(unix)]
pub(crate) use super::alerts::repo_scope_matches;

#[cfg(unix)]
pub(crate) fn resolve_ci_alert_routing(
    home: &std::path::Path,
    team: &str,
    config_cwd: Option<&str>,
    expected_repo_slug: Option<&str>,
) -> (String, Vec<(String, String)>) {
    alerts::resolve_ci_alert_routing(home, team, config_cwd, expected_repo_slug)
}

#[cfg(unix)]
pub(crate) fn notify_ci_not_started(
    home: &std::path::Path,
    status: &GhMonitorStatus,
    config_cwd: Option<&str>,
) {
    alerts::emit_ci_not_started_alert(home, status, config_cwd);
}

#[cfg(unix)]
pub(crate) fn notify_merge_conflict(
    home: &std::path::Path,
    status: &GhMonitorStatus,
    pr_url: Option<&str>,
    merge_state_status: &str,
    run_conclusion: Option<&str>,
    config_cwd: Option<&str>,
) {
    alerts::emit_merge_conflict_alert(
        home,
        status,
        pr_url,
        merge_state_status,
        run_conclusion,
        config_cwd,
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
    alerts::emit_gh_monitor_health_transition(home, team, config_cwd, old_state, new_state, reason);
}

#[cfg(all(test, unix))]
mod tests {
    use super::{notify_merge_conflict, resolve_ci_alert_routing};
    use agent_team_mail_core::daemon_client::{GhMonitorStatus, GhMonitorTargetKind};
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

        let status = GhMonitorStatus {
            team: "atm-dev".to_string(),
            configured: true,
            enabled: true,
            config_source: Some("repo".to_string()),
            config_path: Some(repo_dir.join(".atm.toml").to_string_lossy().to_string()),
            target_kind: GhMonitorTargetKind::Pr,
            target: "123".to_string(),
            state: "merge_conflict".to_string(),
            run_id: Some(99),
            reference: None,
            updated_at: chrono::Utc::now().to_rfc3339(),
            message: Some("preflight dirty".to_string()),
        };

        notify_merge_conflict(
            temp.path(),
            &status,
            Some("https://github.com/randlee/agent-team-mail/pull/123"),
            "DIRTY",
            Some("failure"),
            Some(repo_dir.to_string_lossy().as_ref()),
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
