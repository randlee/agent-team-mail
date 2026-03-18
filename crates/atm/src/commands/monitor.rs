//! `atm monitor` — continuous operational health monitor.

use anyhow::{Context, Result};
use clap::Args;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::io::inbox::inbox_append;
use agent_team_mail_core::schema::InboxMessage;

use crate::commands::doctor::monitor_report_json;
use crate::util::settings::{get_home_dir, teams_root_dir_for};

#[derive(Args, Debug)]
pub struct MonitorArgs {
    /// Team name (uses configured default when omitted)
    #[arg(long)]
    team: Option<String>,

    /// Poll interval in seconds
    #[arg(long, default_value_t = 60)]
    interval_secs: u64,

    /// Cooldown window in seconds for duplicate alerts
    #[arg(long, default_value_t = 300)]
    cooldown_secs: u64,

    /// Comma-separated recipient agent names (default: team-lead)
    #[arg(long, default_value = "team-lead")]
    notify: String,

    /// Run exactly one poll cycle and exit
    #[arg(long)]
    once: bool,

    /// Maximum poll cycles before exit (test helper)
    #[arg(long, hide = true)]
    max_iterations: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FindingKey {
    severity: String,
    code: String,
}

#[derive(Debug, Clone)]
struct MonitorFinding {
    key: FindingKey,
    check: String,
    message: String,
    remediation: Option<String>,
}

#[derive(Default)]
struct AlertTracker {
    active: HashSet<FindingKey>,
    last_sent: HashMap<FindingKey, Instant>,
}

impl AlertTracker {
    fn should_emit(&self, key: &FindingKey, cooldown: Duration, now: Instant) -> bool {
        if !self.active.contains(key) {
            return true;
        }
        match self.last_sent.get(key) {
            Some(last) => now.saturating_duration_since(*last) >= cooldown,
            None => true,
        }
    }
}

pub fn execute(args: MonitorArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;
    let config = resolve_config(
        &ConfigOverrides {
            team: args.team.clone(),
            ..Default::default()
        },
        &current_dir,
        &home_dir,
    )?;
    let team = config.core.default_team;
    let recipients: Vec<String> = args
        .notify
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();
    let cooldown = Duration::from_secs(args.cooldown_secs.max(1));
    let interval = Duration::from_secs(args.interval_secs.max(1));

    let mut tracker = AlertTracker::default();
    let mut iterations = 0u64;
    loop {
        iterations += 1;

        let report_json = monitor_report_json(&home_dir, &team);
        let critical_findings = extract_critical_findings(&report_json);
        let now = Instant::now();
        let current_keys: HashSet<FindingKey> =
            critical_findings.iter().map(|f| f.key.clone()).collect();

        for finding in &critical_findings {
            if tracker.should_emit(&finding.key, cooldown, now) {
                send_alerts(&home_dir, &team, &recipients, finding)?;
                tracker.last_sent.insert(finding.key.clone(), now);
            }
        }
        tracker.active = current_keys;

        if args.once {
            break;
        }
        if let Some(max) = args.max_iterations
            && iterations >= max
        {
            break;
        }
        std::thread::sleep(interval);
    }

    Ok(())
}

fn extract_critical_findings(report_json: &Result<serde_json::Value>) -> Vec<MonitorFinding> {
    let Ok(report) = report_json else {
        return vec![MonitorFinding {
            key: FindingKey {
                severity: "critical".to_string(),
                code: "MONITOR_REPORT_ERROR".to_string(),
            },
            check: "monitor_runtime".to_string(),
            message: "failed to evaluate doctor report".to_string(),
            remediation: Some(
                "Run `atm doctor --json` and inspect daemon availability.".to_string(),
            ),
        }];
    };

    let remediation = report
        .get("recommendations")
        .and_then(|r| r.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("command"))
        .and_then(|v| v.as_str())
        .map(ToString::to_string);

    report
        .get("findings")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|f| {
            let severity = f.get("severity")?.as_str()?.to_string();
            if severity != "critical" {
                return None;
            }
            let code = f.get("code")?.as_str()?.to_string();
            let check = f.get("check")?.as_str()?.to_string();
            let message = f.get("message")?.as_str()?.to_string();
            Some(MonitorFinding {
                key: FindingKey { severity, code },
                check,
                message,
                remediation: remediation.clone(),
            })
        })
        .collect()
}

fn send_alerts(
    home_dir: &std::path::Path,
    team: &str,
    recipients: &[String],
    finding: &MonitorFinding,
) -> Result<()> {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let payload = serde_json::json!({
        "type": "atm_monitor_alert",
        "severity": finding.key.severity,
        "code": finding.key.code,
        "check": finding.check,
        "message": finding.message,
        "remediation": finding.remediation,
        "timestamp": timestamp,
    });
    let human = format!(
        "[atm-monitor] {sev} {code}\ncheck: {check}\nmessage: {msg}\nremediation: {rem}\njson: {json}",
        sev = finding.key.severity.to_uppercase(),
        code = finding.key.code,
        check = finding.check,
        msg = finding.message,
        rem = finding
            .remediation
            .clone()
            .unwrap_or_else(|| "Run `atm doctor --json` for recommendations.".to_string()),
        json = payload
    );

    for recipient in recipients {
        let inbox = teams_root_dir_for(home_dir)
            .join(team)
            .join("inboxes")
            .join(format!("{recipient}.json"));
        if !inbox.exists() {
            continue;
        }
        let msg = InboxMessage {
            from: "atm-monitor".to_string(),
            source_team: None,
            text: human.clone(),
            timestamp: timestamp.clone(),
            read: false,
            summary: Some(format!("{} {}", finding.key.severity, finding.key.code)),
            message_id: Some(uuid::Uuid::new_v4().to_string()),
            unknown_fields: std::collections::HashMap::new(),
        };
        inbox_append(&inbox, &msg, team, "atm-monitor")
            .with_context(|| format!("failed to send alert to {recipient}@{team}"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_critical_findings_filters_non_critical() {
        let report = serde_json::json!({
            "findings": [
                {"severity":"warn","code":"W1","check":"c","message":"warn"},
                {"severity":"critical","code":"C1","check":"c","message":"critical"}
            ],
            "recommendations": [{"command":"atm doctor --json","reason":"test"}]
        });
        let findings = extract_critical_findings(&Ok(report));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].key.code, "C1");
        assert_eq!(
            findings[0].remediation.as_deref(),
            Some("atm doctor --json")
        );
    }

    #[test]
    fn test_alert_tracker_dedup_window() {
        let mut tracker = AlertTracker::default();
        let key = FindingKey {
            severity: "critical".to_string(),
            code: "C1".to_string(),
        };
        let now = Instant::now();
        tracker.active.insert(key.clone());
        tracker.last_sent.insert(key.clone(), now);
        assert!(!tracker.should_emit(&key, Duration::from_secs(60), now));
        assert!(tracker.should_emit(&key, Duration::from_secs(60), now + Duration::from_secs(61)));
    }

    #[test]
    fn test_alert_tracker_reintroduced_finding_emits_immediately() {
        let mut tracker = AlertTracker::default();
        let key = FindingKey {
            severity: "critical".to_string(),
            code: "C1".to_string(),
        };
        let now = Instant::now();
        tracker.last_sent.insert(key.clone(), now);
        // Not active anymore -> treated as reintroduced/new.
        assert!(tracker.should_emit(&key, Duration::from_secs(300), now + Duration::from_secs(5)));
    }
}
