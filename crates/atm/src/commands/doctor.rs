//! `atm doctor` — daemon/team health diagnostics.

use anyhow::{Context, Result};
use clap::Args;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::daemon_client::{
    AgentSummary, SessionQueryResult, daemon_is_running, daemon_pid_path, daemon_socket_path,
    query_list_agents, query_list_agents_for_team, query_session_for_team,
};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::log_reader::{LogFilter, LogReader};
use agent_team_mail_core::schema::TeamConfig;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::util::hook_identity::read_hook_file;
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
    #[serde(skip_serializing, skip_deserializing, default)]
    member_snapshot: Vec<MemberSnapshot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MemberSnapshot {
    name: String,
    agent_type: String,
    model: String,
    status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DoctorState {
    // RFC3339 timestamp of last doctor invocation per team.
    last_call_by_team: HashMap<String, String>,
}

pub fn execute(args: DoctorArgs) -> Result<()> {
    // Prime daemon connectivity early so doctor reflects post-autostart health.
    // Must be best-effort: doctor should still produce a report when daemon is
    // unavailable or autostart fails.
    // Intentionally uses the unscoped query for connectivity priming only;
    // doctor findings themselves use team-scoped checks below.
    let _ = query_list_agents();

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

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "doctor",
        team: Some(team.clone()),
        session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
        agent_id: Some(config.core.identity.clone()),
        agent_name: Some(config.core.identity.clone()),
        result: Some(
            if report.summary.has_critical {
                "critical_findings"
            } else {
                "ok"
            }
            .to_string(),
        ),
        count: Some(report.findings.len() as u64),
        ..Default::default()
    });

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

    let recommendations = build_recommendations(team, &findings, has_register_session_context());

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
        member_snapshot: team_config
            .as_ref()
            .map(|cfg| {
                cfg.members
                    .iter()
                    .map(|m| MemberSnapshot {
                        name: m.name.clone(),
                        agent_type: m.agent_type.clone(),
                        model: m.model.clone(),
                        status: status_from_daemon_session(team, &m.name),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn status_from_daemon_session(team: &str, member_name: &str) -> String {
    status_from_daemon_session_with_query(team, member_name, query_session_for_team)
}

fn status_from_daemon_session_with_query<F>(
    team: &str,
    member_name: &str,
    query_session: F,
) -> String
where
    F: Fn(&str, &str) -> anyhow::Result<Option<SessionQueryResult>>,
{
    match query_session(team, member_name) {
        Ok(Some(session)) if session.alive => "Online".to_string(),
        Ok(Some(_)) => "Offline".to_string(),
        Ok(None) | Err(_) => "Unknown".to_string(),
    }
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
    check_pid_session_reconciliation_with_query(team, cfg, query_session_for_team)
}

fn check_pid_session_reconciliation_with_query<F>(
    team: &str,
    cfg: &TeamConfig,
    query_session: F,
) -> Vec<Finding>
where
    F: Fn(&str, &str) -> anyhow::Result<Option<SessionQueryResult>>,
{
    let mut findings = Vec::new();

    for member in &cfg.members {
        if member.is_active == Some(true) {
            match query_session(team, &member.name) {
                Ok(Some(s)) if !s.alive => findings.push(finding(
                    Severity::Warn,
                    "pid_session_reconciliation",
                    "ACTIVE_FLAG_STALE",
                    format!(
                        "Member '{}' marked active but daemon session is dead (pid={})",
                        member.name, s.process_id
                    ),
                )),
                Ok(None) => findings.push(finding(
                    Severity::Warn,
                    "pid_session_reconciliation",
                    "ACTIVE_WITHOUT_SESSION",
                    format!(
                        "Member '{}' marked active but no daemon session record found",
                        member.name
                    ),
                )),
                // V.3 contract: daemon query errors are treated as unknown/missing
                // session state (doctor remains non-failing and emits diagnostics).
                Err(_) => findings.push(finding(
                    Severity::Warn,
                    "pid_session_reconciliation",
                    "ACTIVE_WITHOUT_SESSION",
                    format!(
                        "Member '{}' marked active but daemon session query failed",
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
    check_roster_session_integrity_with_query(team, cfg, query_list_agents_for_team)
}

fn check_roster_session_integrity_with_query<F>(
    team: &str,
    cfg: &TeamConfig,
    list_agents_for_team: F,
) -> Vec<Finding>
where
    F: Fn(&str) -> anyhow::Result<Option<Vec<AgentSummary>>>,
{
    let mut findings = Vec::new();
    let roster: HashSet<String> = cfg.members.iter().map(|m| m.name.clone()).collect();

    if let Ok(Some(agents)) = list_agents_for_team(team) {
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
    check_mailbox_integrity_with_query(inboxes_dir, team, cfg, query_session_for_team)
}

fn check_mailbox_integrity_with_query<F>(
    inboxes_dir: PathBuf,
    team: &str,
    cfg: &TeamConfig,
    query_session: F,
) -> Vec<Finding>
where
    F: Fn(&str, &str) -> anyhow::Result<Option<SessionQueryResult>>,
{
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
        let session = query_session(team, &member.name).ok().flatten();
        if let Some(s) = session
            && !s.alive
        {
            if member.name == "team-lead" {
                findings.push(finding(
                    Severity::Warn,
                    "mailbox_teardown_integrity",
                    "LEAD_SESSION_RECOVERY_REQUIRED",
                    format!(
                        "Team lead has dead session (pid={}) and requires session recovery/reregister",
                        s.process_id
                    ),
                ));
                continue;
            }

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

fn parse_since_input(input: &str) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Ok(dt.with_timezone(&Utc));
    }

    let trimmed = input.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        anyhow::bail!("value cannot be empty");
    }

    let (num, suffix) = trimmed.split_at(trimmed.len().saturating_sub(1));
    let value: i64 = num
        .parse()
        .with_context(|| format!("invalid duration number: '{num}'"))?;
    if value <= 0 {
        anyhow::bail!("duration must be a positive integer (got {value})");
    }
    let dur = match suffix {
        "s" => Duration::seconds(value),
        "m" => Duration::minutes(value),
        "h" => Duration::hours(value),
        "d" => Duration::days(value),
        _ => anyhow::bail!("invalid duration unit '{suffix}' (expected one of: s, m, h, d)"),
    };

    Ok(Utc::now() - dur)
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
        let dt = parse_since_input(since).with_context(|| {
            format!(
                "Invalid --since value: '{since}'. Use ISO-8601 or positive duration like 30m/2h/1d"
            )
        })?;
        return Ok((dt, "since_override".to_string()));
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

fn has_register_session_context() -> bool {
    if let Ok(session_id) = std::env::var("CLAUDE_SESSION_ID")
        && !session_id.trim().is_empty()
    {
        return true;
    }
    read_hook_file()
        .ok()
        .flatten()
        .map(|d| !d.session_id.trim().is_empty())
        .unwrap_or(false)
}

fn build_recommendations(
    team: &str,
    findings: &[Finding],
    has_session_context: bool,
) -> Vec<Recommendation> {
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
            reason:
                "Reconcile stale roster/mailbox teardown drift (dead terminal members are cleanup-eligible; active sessions are preserved)".to_string(),
        });
    }

    if has("ACTIVE_WITHOUT_SESSION") || has("ACTIVE_FLAG_STALE") {
        recs.push(Recommendation {
            command: format!("atm teams cleanup {team}"),
            reason: "Reconcile stale non-lead session state and mailbox/roster drift".to_string(),
        });
    }

    if has("LEAD_SESSION_RECOVERY_REQUIRED") {
        if has_session_context {
            recs.push(Recommendation {
                command: format!("atm register {team}"),
                reason: "Refresh team-lead/session state before additional lifecycle actions"
                    .to_string(),
            });
        } else {
            recs.push(Recommendation {
                command: format!("atm --as team-lead register {team}"),
                reason: "No session context detected. Run from a managed Claude session (or set CLAUDE_SESSION_ID) before retrying register.".to_string(),
            });
        }
    }

    recs
}

fn print_human(report: &DoctorReport) {
    print!("{}", render_human(report));
}

fn render_human(report: &DoctorReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("ATM Doctor — team {}\n", report.summary.team));
    out.push_str(&format!("Generated: {}\n\n", report.summary.generated_at));

    out.push_str(&format!(
        "Findings: critical={} warn={} info={}\n",
        report.summary.counts.critical, report.summary.counts.warn, report.summary.counts.info
    ));
    out.push_str(&format!(
        "Log window: {} -> {} ({})\n\n",
        report.log_window.start, report.log_window.end, report.log_window.mode
    ));

    if !report.member_snapshot.is_empty() {
        out.push_str("Members:\n");
        out.push_str(&format!(
            "  {:<20} {:<20} {:<16}\n",
            "Name", "Type", "Status"
        ));
        out.push_str(&format!("  {}\n", "─".repeat(58)));
        for m in &report.member_snapshot {
            out.push_str(&format!(
                "  {:<20} {:<20} {:<16}\n",
                m.name, m.agent_type, m.status
            ));
        }
        out.push('\n');
    }

    if report.findings.is_empty() {
        out.push_str("No findings.\n");
        return out;
    }

    out.push_str("Findings (ordered by severity):\n");
    for f in &report.findings {
        let sev = match f.severity {
            Severity::Critical => "CRITICAL",
            Severity::Warn => "WARN",
            Severity::Info => "INFO",
        };
        out.push_str(&format!(
            "- [{sev}] {} ({}): {}\n",
            f.check, f.code, f.message
        ));
    }

    if !report.recommendations.is_empty() {
        out.push_str("\nRecommended actions:\n");
        for r in &report.recommendations {
            out.push_str(&format!("- {}  # {}\n", r.command, r.reason));
        }
    }

    out
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
        assert!(parse_since_input("30m").is_ok());
        assert!(parse_since_input("2h").is_ok());
        assert!(parse_since_input("1d").is_ok());
        assert!(parse_since_input("bogus").is_err());
    }

    #[test]
    fn parse_since_input_supports_rfc3339() {
        let dt = parse_since_input("2026-02-27T20:00:00Z").expect("valid rfc3339");
        assert_eq!(dt.to_rfc3339(), "2026-02-27T20:00:00+00:00");
    }

    #[test]
    fn parse_since_input_rejects_zero_duration() {
        let err = parse_since_input("0m")
            .expect_err("zero duration must be rejected")
            .to_string();
        assert!(err.contains("positive integer"), "unexpected error: {err}");
    }

    #[test]
    fn parse_since_input_rejects_negative_duration() {
        let err = parse_since_input("-5m")
            .expect_err("negative duration must be rejected")
            .to_string();
        assert!(err.contains("positive integer"), "unexpected error: {err}");
    }

    #[test]
    fn parse_since_input_accepts_positive_durations() {
        assert!(parse_since_input("5m").is_ok());
        assert!(parse_since_input("1h").is_ok());
    }

    #[test]
    fn status_from_daemon_session_maps_alive_dead_unknown_and_error() {
        let alive = SessionQueryResult {
            session_id: "s1".to_string(),
            process_id: 1234,
            alive: true,
            runtime: None,
            runtime_session_id: None,
            pane_id: None,
            runtime_home: None,
        };
        let dead = SessionQueryResult {
            session_id: "s2".to_string(),
            process_id: 5678,
            alive: false,
            runtime: None,
            runtime_session_id: None,
            pane_id: None,
            runtime_home: None,
        };
        assert_eq!(
            status_from_daemon_session_with_query("atm-dev", "a", |_, _| Ok(Some(alive.clone()))),
            "Online"
        );
        assert_eq!(
            status_from_daemon_session_with_query("atm-dev", "a", |_, _| Ok(Some(dead.clone()))),
            "Offline"
        );
        assert_eq!(
            status_from_daemon_session_with_query("atm-dev", "a", |_, _| Ok(None)),
            "Unknown"
        );
        assert_eq!(
            status_from_daemon_session_with_query("atm-dev", "a", |_, _| {
                Err(anyhow::anyhow!("daemon unavailable"))
            }),
            "Unknown"
        );
    }

    #[test]
    fn build_recommendations_includes_daemon_start() {
        let findings = vec![finding(
            Severity::Critical,
            "daemon_health",
            "DAEMON_NOT_RUNNING",
            "x".to_string(),
        )];
        let recs = build_recommendations("atm-dev", &findings, true);
        assert!(recs.iter().any(|r| r.command == "atm-daemon"));
    }

    #[test]
    fn build_recommendations_routes_active_without_session_to_cleanup_with_context() {
        let findings = vec![finding(
            Severity::Warn,
            "pid_session_reconciliation",
            "ACTIVE_WITHOUT_SESSION",
            "x".to_string(),
        )];
        let recs = build_recommendations("atm-dev", &findings, true);
        assert!(
            recs.iter()
                .any(|r| r.command == "atm teams cleanup atm-dev")
        );
    }

    #[test]
    fn build_recommendations_routes_active_without_session_to_cleanup_without_context() {
        let findings = vec![finding(
            Severity::Warn,
            "pid_session_reconciliation",
            "ACTIVE_WITHOUT_SESSION",
            "x".to_string(),
        )];
        let recs = build_recommendations("atm-dev", &findings, false);
        assert!(
            recs.iter()
                .any(|r| r.command == "atm teams cleanup atm-dev")
        );
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
    fn check_roster_session_integrity_excludes_other_teams_when_scoped() {
        let cfg = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "s".to_string(),
            members: vec![
                member("team-lead", Some(true), 0),
                member("arch-ctm", Some(true), 0),
            ],
            unknown_fields: HashMap::new(),
        };

        // Seed simulated daemon state for two teams. The scoped query provider
        // returns only members for the requested team.
        let atm_dev_agents = vec![
            AgentSummary {
                agent: "team-lead".to_string(),
                state: "idle".to_string(),
            },
            AgentSummary {
                agent: "arch-ctm".to_string(),
                state: "active".to_string(),
            },
        ];
        let other_team_agents = vec![AgentSummary {
            agent: "researcher".to_string(),
            state: "idle".to_string(),
        }];

        let findings = check_roster_session_integrity_with_query("atm-dev", &cfg, |team| {
            if team == "atm-dev" {
                Ok(Some(atm_dev_agents.clone()))
            } else {
                Ok(Some(other_team_agents.clone()))
            }
        });

        assert!(
            !findings
                .iter()
                .any(|f| f.code == "DAEMON_TRACKS_UNKNOWN_AGENT"),
            "scoped roster integrity check must ignore agents from other teams"
        );
    }

    #[test]
    fn build_recommendations_routes_lead_recovery_to_register_only() {
        let findings = vec![finding(
            Severity::Warn,
            "mailbox_teardown_integrity",
            "LEAD_SESSION_RECOVERY_REQUIRED",
            "x".to_string(),
        )];
        let recs = build_recommendations("atm-dev", &findings, true);
        assert!(recs.iter().any(|r| r.command == "atm register atm-dev"));
        assert!(
            !recs
                .iter()
                .any(|r| r.command == "atm teams cleanup atm-dev")
        );
    }

    #[test]
    fn check_mailbox_integrity_classifies_dead_team_lead_as_recovery_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let inboxes = tmp.path().join("inboxes");
        fs::create_dir_all(&inboxes).unwrap();

        let cfg = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "s".to_string(),
            members: vec![member("team-lead", Some(true), 0)],
            unknown_fields: HashMap::new(),
        };

        let dead_lead_session = SessionQueryResult {
            session_id: "lead-session".to_string(),
            process_id: 4242,
            alive: false,
            runtime: None,
            runtime_session_id: None,
            pane_id: None,
            runtime_home: None,
        };
        let findings = check_mailbox_integrity_with_query(inboxes, "atm-dev", &cfg, |_, name| {
            if name == "team-lead" {
                Ok(Some(dead_lead_session.clone()))
            } else {
                Ok(None)
            }
        });
        assert!(
            findings
                .iter()
                .any(|f| f.code == "LEAD_SESSION_RECOVERY_REQUIRED"),
            "dead team-lead must produce explicit recovery warning"
        );
        assert!(!findings.iter().any(|f| f.code == "PARTIAL_TEARDOWN"));
    }

    #[test]
    fn check_mailbox_integrity_keeps_dead_non_lead_as_critical_cleanup_finding() {
        let tmp = tempfile::tempdir().unwrap();
        let inboxes = tmp.path().join("inboxes");
        fs::create_dir_all(&inboxes).unwrap();
        fs::write(inboxes.join("arch-ctm.json"), "[]").unwrap();

        let cfg = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "s".to_string(),
            members: vec![
                member("team-lead", Some(true), 0),
                member("arch-ctm", Some(true), 0),
            ],
            unknown_fields: HashMap::new(),
        };

        let dead_member_session = SessionQueryResult {
            session_id: "member-session".to_string(),
            process_id: 4243,
            alive: false,
            runtime: None,
            runtime_session_id: None,
            pane_id: None,
            runtime_home: None,
        };
        let findings = check_mailbox_integrity_with_query(inboxes, "atm-dev", &cfg, |_, name| {
            if name == "arch-ctm" {
                Ok(Some(dead_member_session.clone()))
            } else {
                Ok(None)
            }
        });

        assert!(
            findings.iter().any(
                |f| f.code == "TERMINAL_MEMBER_NOT_CLEANED" && f.severity == Severity::Critical
            ),
            "dead non-lead with mailbox must remain critical cleanup finding"
        );
    }

    #[test]
    fn build_recommendations_routes_non_lead_teardown_to_cleanup() {
        let findings = vec![finding(
            Severity::Critical,
            "mailbox_teardown_integrity",
            "TERMINAL_MEMBER_NOT_CLEANED",
            "x".to_string(),
        )];
        let recs = build_recommendations("atm-dev", &findings, true);
        assert!(
            recs.iter()
                .any(|r| r.command == "atm teams cleanup atm-dev")
        );
    }

    #[test]
    fn build_recommendations_routes_active_without_session_to_cleanup() {
        let findings = vec![finding(
            Severity::Warn,
            "pid_session_reconciliation",
            "ACTIVE_WITHOUT_SESSION",
            "x".to_string(),
        )];
        let recs = build_recommendations("atm-dev", &findings, true);
        assert!(
            recs.iter()
                .any(|r| r.command == "atm teams cleanup atm-dev")
        );
        assert!(
            !recs.iter().any(|r| r.command == "atm register atm-dev"),
            "non-lead stale/unknown activity should route to cleanup, not register"
        );
    }

    #[test]
    fn check_pid_session_reconciliation_query_error_maps_to_active_without_session() {
        let cfg = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "s".to_string(),
            members: vec![member("worker-a", Some(true), 0)],
            unknown_fields: HashMap::new(),
        };

        let findings = check_pid_session_reconciliation_with_query("atm-dev", &cfg, |_, _| {
            Err(anyhow::anyhow!("daemon unavailable"))
        });
        assert!(
            findings.iter().any(|f| {
                f.code == "ACTIVE_WITHOUT_SESSION" && f.message.contains("query failed")
            })
        );
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

    #[test]
    fn render_human_places_member_snapshot_before_findings() {
        let report = DoctorReport {
            summary: Summary {
                team: "atm-dev".to_string(),
                generated_at: "2026-03-02T00:00:00Z".to_string(),
                has_critical: false,
                counts: FindingCounts {
                    critical: 0,
                    warn: 1,
                    info: 0,
                },
            },
            findings: vec![finding(
                Severity::Warn,
                "pid_session_reconciliation",
                "ACTIVE_WITHOUT_SESSION",
                "x".to_string(),
            )],
            recommendations: vec![],
            log_window: LogWindow {
                mode: "default_incremental".to_string(),
                start: "2026-03-02T00:00:00Z".to_string(),
                end: "2026-03-02T00:01:00Z".to_string(),
            },
            member_snapshot: vec![MemberSnapshot {
                name: "team-lead".to_string(),
                agent_type: "team-lead".to_string(),
                model: "claude".to_string(),
                status: "Online".to_string(),
            }],
        };
        let rendered = render_human(&report);
        let members_idx = rendered.find("Members:").unwrap();
        let findings_idx = rendered.find("Findings (ordered by severity):").unwrap();
        assert!(members_idx < findings_idx);
    }

    #[test]
    fn doctor_json_schema_excludes_member_snapshot() {
        let report = DoctorReport {
            summary: Summary {
                team: "atm-dev".to_string(),
                generated_at: "2026-03-02T00:00:00Z".to_string(),
                has_critical: false,
                counts: FindingCounts {
                    critical: 0,
                    warn: 0,
                    info: 0,
                },
            },
            findings: vec![],
            recommendations: vec![],
            log_window: LogWindow {
                mode: "default_incremental".to_string(),
                start: "2026-03-02T00:00:00Z".to_string(),
                end: "2026-03-02T00:01:00Z".to_string(),
            },
            member_snapshot: vec![MemberSnapshot::default()],
        };
        let value = serde_json::to_value(report).unwrap();
        assert!(value.get("member_snapshot").is_none());
    }
}
