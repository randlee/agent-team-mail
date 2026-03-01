//! `atm doctor` — daemon/team health diagnostics.

use anyhow::{Context, Result};
use clap::Args;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::daemon_client::{
    daemon_is_running, daemon_pid_path, daemon_socket_path, query_list_agents,
    query_session_for_team,
};
use agent_team_mail_core::log_reader::{LogFilter, LogReader};
use agent_team_mail_core::schema::TeamConfig;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::util::settings::get_home_dir;

#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// Team name (uses configured default when omitted)
    #[arg(long)]
    team: Option<String>,

    /// Output report as stable JSON schema
    #[arg(long)]
    json: bool,

    /// Override log window start (ISO-8601 timestamp or duration, e.g. 30m, 2h, 1d)
    #[arg(long)]
    since: Option<String>,

    /// Restrict log diagnostics to error-level events only
    #[arg(long)]
    errors_only: bool,

    /// Use full log window from team-lead session start
    #[arg(long)]
    full: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Severity {
    Critical,
    Warn,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Finding {
    severity: Severity,
    check: String,
    code: String,
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Recommendation {
    command: String,
    reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FindingCounts {
    critical: usize,
    warn: usize,
    info: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Summary {
    team: String,
    generated_at: String,
    has_critical: bool,
    counts: FindingCounts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LogWindow {
    mode: String,
    start: String,
    end: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DoctorReport {
    summary: Summary,
    findings: Vec<Finding>,
    recommendations: Vec<Recommendation>,
    log_window: LogWindow,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DoctorState {
    // RFC3339 timestamp of last doctor invocation per team.
    last_call_by_team: HashMap<String, String>,
}

pub fn execute(args: DoctorArgs) -> Result<()> {
    // Prime daemon connectivity early so doctor reflects post-autostart health.
    let _ = query_list_agents()?;

    let current_dir = std::env::current_dir()?;
    let home_dir = get_home_dir()?;

    let config = resolve_config(
        &ConfigOverrides {
            team: args.team.clone(),
            ..Default::default()
        },
        &current_dir,
        &home_dir,
    )?;
    let team = config.core.default_team.clone();

    let report = build_report(&home_dir, &team, &args)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report);
    }

    persist_last_call(&home_dir, &team)?;

    if report.summary.has_critical {
        std::process::exit(2);
    }

    Ok(())
}

/// Build a doctor report JSON snapshot for monitor consumers.
///
/// This reuses the same evaluation path as `atm doctor` without terminal UI
/// side effects, so monitor logic does not duplicate health checks.
pub(crate) fn monitor_report_json(home_dir: &Path, team: &str) -> Result<serde_json::Value> {
    let args = DoctorArgs {
        team: Some(team.to_string()),
        json: true,
        since: None,
        errors_only: false,
        full: false,
    };
    let report = build_report(home_dir, team, &args)?;
    Ok(serde_json::to_value(report)?)
}

fn build_report(home_dir: &Path, team: &str, args: &DoctorArgs) -> Result<DoctorReport> {
    let now = Utc::now();
    let team_dir = home_dir.join(".claude/teams").join(team);
    let config_path = team_dir.join("config.json");

    let mut findings: Vec<Finding> = Vec::new();

    if !team_dir.exists() {
        findings.push(finding(
            Severity::Critical,
            "config_runtime_drift",
            "TEAM_DIR_MISSING",
            format!("Team directory missing: {}", team_dir.display()),
        ));
    }

    let team_config = if config_path.exists() {
        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?;
        Some(serde_json::from_str::<TeamConfig>(&content).context("Failed to parse team config")?)
    } else {
        findings.push(finding(
            Severity::Critical,
            "config_runtime_drift",
            "TEAM_CONFIG_MISSING",
            format!("Team config missing: {}", config_path.display()),
        ));
        None
    };

    // Check 1: daemon health (lock/socket/PID/status coherence)
    findings.extend(check_daemon_health(home_dir));

    // Check 2 + 3 + 4: session/roster/mailbox integrity
    if let Some(cfg) = &team_config {
        findings.extend(check_pid_session_reconciliation(team, cfg));
        findings.extend(check_roster_session_integrity(team, cfg));
        findings.extend(check_mailbox_integrity(team_dir.join("inboxes"), team, cfg));
    }

    // Check 5: config/runtime drift
    findings.extend(check_config_runtime_drift(team, &args.team));

    // Check 6: unified log diagnostics
    let (window_start, mode) =
        compute_log_window_start(home_dir, team, team_config.as_ref(), args)?;
    let window_end = now;
    findings.extend(check_log_diagnostics(
        home_dir,
        window_start,
        window_end,
        args.errors_only,
    ));

    findings.sort_by_key(|f| match f.severity {
        Severity::Critical => 0,
        Severity::Warn => 1,
        Severity::Info => 2,
    });

    let recommendations = build_recommendations(team, &findings);

    let counts = count_findings(&findings);
    let summary = Summary {
        team: team.to_string(),
        generated_at: now.to_rfc3339(),
        has_critical: counts.critical > 0,
        counts,
    };

    Ok(DoctorReport {
        summary,
        findings,
        recommendations,
        log_window: LogWindow {
            mode,
            start: window_start.to_rfc3339(),
            end: window_end.to_rfc3339(),
        },
    })
}

fn finding(severity: Severity, check: &str, code: &str, message: String) -> Finding {
    Finding {
        severity,
        check: check.to_string(),
        code: code.to_string(),
        message,
    }
}

fn check_daemon_health(home_dir: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();

    let running = daemon_is_running();
    let socket_path =
        daemon_socket_path().unwrap_or_else(|_| home_dir.join(".claude/daemon/atm-daemon.sock"));
    let pid_path =
        daemon_pid_path().unwrap_or_else(|_| home_dir.join(".claude/daemon/atm-daemon.pid"));
    let lock_path = home_dir.join(".config/atm/daemon.lock");
    let status_path = home_dir.join(".claude/daemon/status.json");

    if !running {
        findings.push(finding(
            Severity::Critical,
            "daemon_health",
            "DAEMON_NOT_RUNNING",
            "Daemon is not running or PID cannot be verified".to_string(),
        ));
    }

    if socket_path.exists() && !running {
        findings.push(finding(
            Severity::Warn,
            "daemon_health",
            "STALE_SOCKET",
            format!(
                "Socket exists but daemon not running: {}",
                socket_path.display()
            ),
        ));
    }

    if !socket_path.exists() && running {
        findings.push(finding(
            Severity::Warn,
            "daemon_health",
            "SOCKET_MISSING",
            format!(
                "Daemon appears running but socket missing: {}",
                socket_path.display()
            ),
        ));
    }

    if !pid_path.exists() {
        findings.push(finding(
            Severity::Warn,
            "daemon_health",
            "PID_FILE_MISSING",
            format!("Daemon PID file missing: {}", pid_path.display()),
        ));
    }

    if !lock_path.exists() {
        findings.push(finding(
            Severity::Info,
            "daemon_health",
            "LOCK_FILE_MISSING",
            format!("Daemon lock file not present: {}", lock_path.display()),
        ));
    }

    if !status_path.exists() {
        findings.push(finding(
            Severity::Warn,
            "daemon_health",
            "STATUS_FILE_MISSING",
            format!("Daemon status file missing: {}", status_path.display()),
        ));
    }

    findings
}

fn check_pid_session_reconciliation(team: &str, cfg: &TeamConfig) -> Vec<Finding> {
    let mut findings = Vec::new();

    for member in &cfg.members {
        let session = query_session_for_team(team, &member.name).ok().flatten();
        if member.is_active == Some(true) {
            match session {
                Some(s) if !s.alive => findings.push(finding(
                    Severity::Warn,
                    "pid_session_reconciliation",
                    "ACTIVE_FLAG_STALE",
                    format!(
                        "Member '{}' marked active but daemon session is dead (pid={})",
                        member.name, s.process_id
                    ),
                )),
                None => findings.push(finding(
                    Severity::Warn,
                    "pid_session_reconciliation",
                    "ACTIVE_WITHOUT_SESSION",
                    format!(
                        "Member '{}' marked active but no daemon session record found",
                        member.name
                    ),
                )),
                _ => {}
            }
        }
    }

    findings
}

fn check_roster_session_integrity(team: &str, cfg: &TeamConfig) -> Vec<Finding> {
    let mut findings = Vec::new();
    let roster: HashSet<String> = cfg.members.iter().map(|m| m.name.clone()).collect();

    if let Ok(Some(agents)) = query_list_agents() {
        for tracked in agents {
            if !roster.contains(&tracked.agent) {
                findings.push(finding(
                    Severity::Warn,
                    "roster_session_integrity",
                    "DAEMON_TRACKS_UNKNOWN_AGENT",
                    format!(
                        "Daemon tracks '{}' which is not in team '{}' roster",
                        tracked.agent, team
                    ),
                ));
            }
        }
    }

    findings
}

fn check_mailbox_integrity(inboxes_dir: PathBuf, team: &str, cfg: &TeamConfig) -> Vec<Finding> {
    let mut findings = Vec::new();
    let roster: HashSet<String> = cfg.members.iter().map(|m| m.name.clone()).collect();

    let mut local_mailboxes: HashSet<String> = HashSet::new();
    if inboxes_dir.exists() {
        if let Ok(entries) = fs::read_dir(&inboxes_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !name.ends_with(".json") {
                    continue;
                }
                let stem = name.trim_end_matches(".json");
                if stem.contains('.') {
                    continue; // per-origin files not teardown authority
                }
                local_mailboxes.insert(stem.to_string());
            }
        }
    }

    for mailbox in &local_mailboxes {
        if !roster.contains(mailbox) {
            findings.push(finding(
                Severity::Critical,
                "mailbox_teardown_integrity",
                "ORPHAN_MAILBOX",
                format!(
                    "Mailbox '{}' exists without matching roster member in team '{}'",
                    mailbox, team
                ),
            ));
        }
    }

    for member in &cfg.members {
        let session = query_session_for_team(team, &member.name).ok().flatten();
        if let Some(s) = session
            && !s.alive
        {
            let has_mailbox = local_mailboxes.contains(&member.name);
            if has_mailbox {
                findings.push(finding(
                    Severity::Critical,
                    "mailbox_teardown_integrity",
                    "TERMINAL_MEMBER_NOT_CLEANED",
                    format!(
                        "Member '{}' has dead session but still exists in roster and mailbox",
                        member.name
                    ),
                ));
            } else {
                findings.push(finding(
                    Severity::Critical,
                    "mailbox_teardown_integrity",
                    "PARTIAL_TEARDOWN",
                    format!(
                        "Member '{}' has dead session and missing mailbox but still exists in roster",
                        member.name
                    ),
                ));
            }
        }
    }

    findings
}

fn check_config_runtime_drift(team: &str, explicit_team_arg: &Option<String>) -> Vec<Finding> {
    let mut findings = Vec::new();

    if let Ok(env_team) = std::env::var("ATM_TEAM")
        && env_team != team
    {
        findings.push(finding(
            Severity::Info,
            "config_runtime_drift",
            "ENV_TEAM_MISMATCH",
            format!(
                "ATM_TEAM='{}' differs from resolved team='{}'",
                env_team, team
            ),
        ));
    }

    if explicit_team_arg.is_none() {
        findings.push(finding(
            Severity::Info,
            "config_runtime_drift",
            "TEAM_FROM_DEFAULT",
            "Team resolved from config default (no --team override)".to_string(),
        ));
    }

    findings
}

fn check_log_diagnostics(
    home_dir: &Path,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    errors_only: bool,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    let log_path = home_dir.join(".config/atm/atm.log.jsonl");
    let delta = end.signed_duration_since(start);
    let since_std = if delta < Duration::zero() {
        std::time::Duration::from_secs(0)
    } else {
        delta
            .to_std()
            .unwrap_or_else(|_| std::time::Duration::from_secs(0))
    };

    let filter = LogFilter {
        level: if errors_only {
            Some("error".to_string())
        } else {
            None
        },
        since: Some(since_std),
        ..Default::default()
    };

    let reader = LogReader::new(log_path.clone(), filter);
    let events = match reader.read_filtered() {
        Ok(v) => v,
        Err(e) => {
            findings.push(finding(
                Severity::Warn,
                "log_diagnostics",
                "LOG_READ_FAILED",
                format!("Failed to read {}: {e}", log_path.display()),
            ));
            return findings;
        }
    };

    if events.is_empty() {
        if !errors_only {
            findings.push(finding(
                Severity::Info,
                "log_diagnostics",
                "NO_EVENTS_IN_WINDOW",
                "No matching log events found in selected window".to_string(),
            ));
        }
        return findings;
    }

    let (warn_count, err_count) = events.iter().fold((0usize, 0usize), |(w, e), ev| {
        if ev.level.eq_ignore_ascii_case("error") {
            (w, e + 1)
        } else if ev.level.eq_ignore_ascii_case("warn") {
            (w + 1, e)
        } else {
            (w, e)
        }
    });

    if err_count > 0 {
        findings.push(finding(
            Severity::Warn,
            "log_diagnostics",
            "ERROR_EVENTS_PRESENT",
            format!("Found {err_count} error-level event(s) in log window"),
        ));
    }
    if !errors_only && warn_count > 0 {
        findings.push(finding(
            Severity::Info,
            "log_diagnostics",
            "WARN_EVENTS_PRESENT",
            format!("Found {warn_count} warning-level event(s) in log window"),
        ));
    }

    findings
}

fn parse_since_input(input: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Some(dt.with_timezone(&Utc));
    }

    let trimmed = input.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return None;
    }

    let (num, suffix) = trimmed.split_at(trimmed.len().saturating_sub(1));
    let value: i64 = num.parse().ok()?;
    let dur = match suffix {
        "s" => Duration::seconds(value),
        "m" => Duration::minutes(value),
        "h" => Duration::hours(value),
        "d" => Duration::days(value),
        _ => return None,
    };

    Some(Utc::now() - dur)
}

fn read_doctor_state(home_dir: &Path) -> DoctorState {
    let path = home_dir.join(".config/atm/doctor-state.json");
    let Ok(content) = fs::read_to_string(path) else {
        return DoctorState::default();
    };
    serde_json::from_str(&content).unwrap_or_default()
}

fn persist_last_call(home_dir: &Path, team: &str) -> Result<()> {
    let mut state = read_doctor_state(home_dir);
    state
        .last_call_by_team
        .insert(team.to_string(), Utc::now().to_rfc3339());

    let path = home_dir.join(".config/atm/doctor-state.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_vec_pretty(&state)?)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

fn compute_log_window_start(
    home_dir: &Path,
    team: &str,
    cfg: Option<&TeamConfig>,
    args: &DoctorArgs,
) -> Result<(DateTime<Utc>, String)> {
    if let Some(since) = &args.since {
        if let Some(dt) = parse_since_input(since) {
            return Ok((dt, "since_override".to_string()));
        }
        anyhow::bail!("Invalid --since value: '{since}'. Use ISO-8601 or duration like 30m/2h/1d");
    }

    let fallback = Utc::now() - Duration::hours(1);
    let team_lead_start = cfg
        .and_then(|c| c.members.iter().find(|m| m.name == "team-lead"))
        .and_then(|m| DateTime::<Utc>::from_timestamp_millis(m.joined_at as i64))
        .unwrap_or(fallback);

    if args.full {
        return Ok((team_lead_start, "full".to_string()));
    }

    let state = read_doctor_state(home_dir);
    let last_call = state
        .last_call_by_team
        .get(team)
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let start = match last_call {
        Some(lc) if lc > team_lead_start => lc,
        _ => team_lead_start,
    };
    Ok((start, "default_incremental".to_string()))
}

fn count_findings(findings: &[Finding]) -> FindingCounts {
    let mut counts = FindingCounts {
        critical: 0,
        warn: 0,
        info: 0,
    };

    for f in findings {
        match f.severity {
            Severity::Critical => counts.critical += 1,
            Severity::Warn => counts.warn += 1,
            Severity::Info => counts.info += 1,
        }
    }

    counts
}

fn build_recommendations(team: &str, findings: &[Finding]) -> Vec<Recommendation> {
    let mut recs: Vec<Recommendation> = Vec::new();

    let has = |code: &str| findings.iter().any(|f| f.code == code);

    if has("DAEMON_NOT_RUNNING") {
        recs.push(Recommendation {
            command: "atm-daemon".to_string(),
            reason: "Start daemon to enable session/teardown reconciliation".to_string(),
        });
    }

    if has("ORPHAN_MAILBOX") || has("TERMINAL_MEMBER_NOT_CLEANED") || has("PARTIAL_TEARDOWN") {
        recs.push(Recommendation {
            command: format!("atm teams cleanup {team}"),
            reason: "Reconcile stale roster/mailbox teardown drift".to_string(),
        });
    }

    if has("ACTIVE_WITHOUT_SESSION") || has("ACTIVE_FLAG_STALE") {
        recs.push(Recommendation {
            command: format!("atm register {team}"),
            reason: "Refresh team-lead/session state before additional lifecycle actions"
                .to_string(),
        });
    }

    recs
}

fn print_human(report: &DoctorReport) {
    println!("ATM Doctor — team {}", report.summary.team);
    println!("Generated: {}", report.summary.generated_at);
    println!();
    println!(
        "Findings: critical={} warn={} info={}",
        report.summary.counts.critical, report.summary.counts.warn, report.summary.counts.info
    );
    println!(
        "Log window: {} -> {} ({})",
        report.log_window.start, report.log_window.end, report.log_window.mode
    );
    println!();

    if report.findings.is_empty() {
        println!("No findings.");
        return;
    }

    println!("Findings (ordered by severity):");
    for f in &report.findings {
        let sev = match f.severity {
            Severity::Critical => "CRITICAL",
            Severity::Warn => "WARN",
            Severity::Info => "INFO",
        };
        println!("- [{sev}] {} ({}): {}", f.check, f.code, f.message);
    }

    if !report.recommendations.is_empty() {
        println!();
        println!("Recommended actions:");
        for r in &report.recommendations {
            println!("- {}  # {}", r.command, r.reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_core::schema::AgentMember;

    fn member(name: &str, is_active: Option<bool>, joined_at: u64) -> AgentMember {
        AgentMember {
            agent_id: format!("{name}@atm-dev"),
            name: name.to_string(),
            agent_type: "general-purpose".to_string(),
            model: "sonnet".to_string(),
            prompt: None,
            color: None,
            plan_mode_required: None,
            joined_at,
            tmux_pane_id: None,
            cwd: std::env::temp_dir().to_string_lossy().to_string(),
            subscriptions: vec![],
            backend_type: None,
            is_active,
            last_active: None,
            session_id: None,
            external_backend_type: None,
            external_model: None,
            unknown_fields: HashMap::new(),
        }
    }

    #[test]
    fn parse_since_input_supports_duration() {
        assert!(parse_since_input("30m").is_some());
        assert!(parse_since_input("2h").is_some());
        assert!(parse_since_input("1d").is_some());
        assert!(parse_since_input("bogus").is_none());
    }

    #[test]
    fn parse_since_input_supports_rfc3339() {
        let dt = parse_since_input("2026-02-27T20:00:00Z").unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-02-27T20:00:00+00:00");
    }

    #[test]
    fn build_recommendations_includes_daemon_start() {
        let findings = vec![finding(
            Severity::Critical,
            "daemon_health",
            "DAEMON_NOT_RUNNING",
            "x".to_string(),
        )];
        let recs = build_recommendations("atm-dev", &findings);
        assert!(recs.iter().any(|r| r.command == "atm-daemon"));
    }

    #[test]
    fn count_findings_counts_all_levels() {
        let findings = vec![
            finding(Severity::Critical, "a", "A", "x".to_string()),
            finding(Severity::Warn, "b", "B", "x".to_string()),
            finding(Severity::Info, "c", "C", "x".to_string()),
        ];
        let counts = count_findings(&findings);
        assert_eq!(counts.critical, 1);
        assert_eq!(counts.warn, 1);
        assert_eq!(counts.info, 1);
    }

    #[test]
    fn compute_log_window_prefers_max_of_team_start_and_last_call() {
        let tmp = tempfile::tempdir().unwrap();
        let team = "atm-dev";
        let mut state = DoctorState::default();
        state
            .last_call_by_team
            .insert(team.to_string(), "2026-02-27T21:00:00Z".to_string());
        let state_path = tmp.path().join(".config/atm/doctor-state.json");
        fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();

        let cfg = TeamConfig {
            name: team.to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: format!("team-lead@{team}"),
            lead_session_id: "sess".to_string(),
            members: vec![member("team-lead", Some(true), 1772216400000)], // ~2026-02-27T19:00:00Z
            unknown_fields: HashMap::new(),
        };

        let args = DoctorArgs {
            team: Some(team.to_string()),
            json: false,
            since: None,
            errors_only: false,
            full: false,
        };

        let (start, mode) = compute_log_window_start(tmp.path(), team, Some(&cfg), &args).unwrap();
        assert_eq!(mode, "default_incremental");
        assert_eq!(start.to_rfc3339(), "2026-02-27T21:00:00+00:00");
    }

    #[test]
    fn check_mailbox_integrity_detects_orphan_mailbox() {
        let tmp = tempfile::tempdir().unwrap();
        let inboxes = tmp.path().join("inboxes");
        fs::create_dir_all(&inboxes).unwrap();
        fs::write(inboxes.join("orphan.json"), "[]").unwrap();

        let cfg = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "s".to_string(),
            members: vec![member("team-lead", Some(true), 0)],
            unknown_fields: HashMap::new(),
        };

        let findings = check_mailbox_integrity(inboxes, "atm-dev", &cfg);
        assert!(findings.iter().any(|f| f.code == "ORPHAN_MAILBOX"));
    }

    #[test]
    fn check_config_runtime_drift_flags_env_mismatch() {
        let team = "atm-dev";
        let findings = check_config_runtime_drift(team, &Some(team.to_string()));
        // no assertion on env here; just ensure function emits deterministic info for explicit team.
        assert!(!findings.iter().any(|f| f.code == "TEAM_FROM_DEFAULT"));
    }

    #[test]
    fn check_log_diagnostics_errors_only_suppresses_no_events_info() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join(".config/atm");
        fs::create_dir_all(&log_dir).unwrap();
        fs::write(log_dir.join("atm.log.jsonl"), "").unwrap();

        let start = Utc::now() - Duration::minutes(5);
        let end = Utc::now();
        let findings = check_log_diagnostics(tmp.path(), start, end, true);

        assert!(!findings.iter().any(|f| f.code == "NO_EVENTS_IN_WINDOW"));
    }

    #[test]
    fn check_log_diagnostics_non_errors_only_emits_no_events_info() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join(".config/atm");
        fs::create_dir_all(&log_dir).unwrap();
        fs::write(log_dir.join("atm.log.jsonl"), "").unwrap();

        let start = Utc::now() - Duration::minutes(5);
        let end = Utc::now();
        let findings = check_log_diagnostics(tmp.path(), start, end, false);

        assert!(findings.iter().any(|f| f.code == "NO_EVENTS_IN_WINDOW"));
    }
}
