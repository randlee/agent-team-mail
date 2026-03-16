//! `atm doctor` — daemon/team health diagnostics.

use anyhow::{Context, Result};
use clap::Args;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::daemon_client::{
    AgentSummary, CanonicalMemberState, DaemonTouchSnapshot, SessionQueryResult, daemon_is_running,
    daemon_lock_path, daemon_pid_path, daemon_socket_path, daemon_status_path_for,
    daemon_touch_path_for, query_list_agents, query_list_agents_for_team, query_session_for_team,
    query_team_member_states, read_daemon_lock_metadata,
};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::gh_monitor_observability::{
    GhCliObserverContext, RateLimitUpdate, emit_gh_info_denied, emit_gh_info_live_refresh,
    emit_gh_info_requested, emit_gh_info_served_from_cache, gh_repo_state_cache_age_secs,
    new_gh_execution_call_id, new_gh_info_request_id, read_gh_repo_state,
    run_attributed_gh_command_with_ids, update_gh_repo_state_rate_limit,
};
use agent_team_mail_core::log_reader::{LogFilter, LogReader};
use agent_team_mail_core::pid::is_pid_alive;
use agent_team_mail_core::schema::TeamConfig;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::commands::init::{
    catch_all_hook_command_present, notification_idle_prompt_cmd, permission_request_cmd,
    post_tool_use_bash_cmd, pre_tool_use_bash_cmd, pre_tool_use_task_cmd, runtime_detected,
    session_end_cmd, session_start_cmd, stop_cmd,
};
use crate::commands::logging_health::{
    LoggingHealthContract, LoggingHealthSnapshot, build_logging_health_contract,
    logging_remediation,
};
use crate::util::caller_identity::resolve_caller_session_id_optional;
use crate::util::member_labels::UNREGISTERED_MARKER;
use crate::util::settings::{claude_root_dir_for, get_home_dir, teams_root_dir_for};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    uptime_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    install_milestone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LogWindow {
    mode: String,
    start: String,
    end: String,
    elapsed_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct EnvOverrideValue {
    source: String,
    value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EnvOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    atm_home: Option<EnvOverrideValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    atm_team: Option<EnvOverrideValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    atm_identity: Option<EnvOverrideValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DoctorReport {
    summary: Summary,
    findings: Vec<Finding>,
    recommendations: Vec<Recommendation>,
    log_window: LogWindow,
    env_overrides: EnvOverrides,
    #[serde(skip_serializing_if = "Option::is_none")]
    gh_rate_limit_audit: Option<GhRateLimitAudit>,
    #[serde(default)]
    logging_health: LoggingHealthContract,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    members: Vec<MemberSnapshot>,
    #[serde(skip_serializing, skip_deserializing, default)]
    member_snapshot: Vec<MemberSnapshot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MemberSnapshot {
    name: String,
    agent_type: String,
    model: String,
    status: String,
    activity: String,
    session_id: Option<String>,
    process_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DoctorState {
    // RFC3339 timestamp of last doctor invocation per team.
    last_call_by_team: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GhRateLimitAudit {
    live_remaining: u64,
    live_limit: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    live_reset_at: Option<String>,
    cached_used_in_window: u64,
    repos_observed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    cached_rate_limit_remaining: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cached_rate_limit_limit: Option<u64>,
    delta_consumed_vs_cached: i64,
}

#[derive(Debug, Deserialize)]
struct GhRateLimitResponse {
    resources: GhRateLimitResources,
}

#[derive(Debug, Deserialize)]
struct GhRateLimitResources {
    core: GhCoreRateLimit,
}

#[derive(Debug, Deserialize)]
struct GhCoreRateLimit {
    limit: u64,
    remaining: u64,
    #[serde(default)]
    reset: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct DaemonStatusSnapshot {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    plugins: Vec<PluginStatusSnapshot>,
    #[serde(default)]
    logging: LoggingHealthSnapshot,
}

#[derive(Debug, Clone, Deserialize)]
struct PluginStatusSnapshot {
    name: String,
    status: String,
    #[serde(default)]
    last_error: Option<String>,
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
    let caller_session_id =
        resolve_caller_session_id_optional(Some(&team), Some(&config.core.identity))
            .ok()
            .flatten();

    let mut report = build_report(&home_dir, &team, &args)?;
    report.gh_rate_limit_audit = build_gh_rate_limit_audit(&home_dir, &team).ok().flatten();

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "doctor",
        team: Some(team.clone()),
        session_id: caller_session_id,
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
    let team_dir = teams_root_dir_for(home_dir).join(team);
    let config_path = team_dir.join("config.json");

    let mut findings: Vec<Finding> = Vec::new();
    let mut daemon_states_by_agent: HashMap<String, CanonicalMemberState> = HashMap::new();

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
    findings.extend(check_daemon_ownership_mismatch(home_dir));
    findings.extend(check_plugin_init_failures(home_dir));

    // Check 2 + 3 + 4: session/roster/mailbox integrity
    if let Some(cfg) = &team_config {
        let (pid_findings, daemon_states) =
            check_pid_session_reconciliation_with_query(team, cfg, query_team_member_states);
        daemon_states_by_agent = daemon_states;
        findings.extend(pid_findings);
        findings.extend(check_roster_session_integrity(team, cfg));
        findings.extend(check_mailbox_integrity(team_dir.join("inboxes"), team, cfg));
    }

    // Check 5: config/runtime drift
    findings.extend(check_config_runtime_drift(team, &args.team));

    // Check 5b: hook installation / config audit
    findings.extend(check_hook_audit(home_dir, &std::env::current_dir()?));

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
        uptime_secs: read_daemon_status_uptime_secs(home_dir),
        daemon_version: None,
        install_milestone: read_active_install_milestone(),
    };
    let daemon_status = read_daemon_status(home_dir);
    let summary = Summary {
        daemon_version: daemon_status.version.clone(),
        ..summary
    };
    let logging = daemon_status.logging;
    let logging_health = build_logging_health_contract(&logging, home_dir);

    let member_snapshot = build_member_snapshot(team_config.as_ref(), &daemon_states_by_agent);
    Ok(DoctorReport {
        summary,
        findings,
        recommendations,
        log_window: LogWindow {
            mode,
            start: window_start.to_rfc3339(),
            end: window_end.to_rfc3339(),
            elapsed_secs: window_end
                .signed_duration_since(window_start)
                .num_seconds()
                .max(0) as u64,
        },
        env_overrides: active_env_overrides(),
        gh_rate_limit_audit: None,
        logging_health,
        members: member_snapshot.clone(),
        member_snapshot,
    })
}

fn build_gh_rate_limit_audit(home_dir: &Path, team: &str) -> Result<Option<GhRateLimitAudit>> {
    let state = read_gh_repo_state(home_dir)?;
    let team_records: Vec<_> = state
        .records
        .into_iter()
        .filter(|record| record.team == team)
        .collect();
    if team_records.is_empty() {
        return Ok(None);
    }

    let repo_scope = &team_records[0].repo;
    if !repo_scope.contains('/') {
        anyhow::bail!("invalid owner/repo scope in gh repo-state: {repo_scope}");
    }
    let observer_ctx = GhCliObserverContext {
        home: home_dir.to_path_buf(),
        team: team.to_string(),
        repo: repo_scope.to_string(),
        runtime: "atm".to_string(),
    };
    let request_id = new_gh_info_request_id();
    let call_id = new_gh_execution_call_id();
    emit_gh_info_requested(&observer_ctx, &request_id, "gh_api_rate_limit");
    if let Some(cached_rate_limit) = team_records
        .iter()
        .filter_map(|record| record.rate_limit.as_ref().map(|_| record))
        .max_by_key(|record| record.updated_at.clone())
    {
        emit_gh_info_served_from_cache(
            &observer_ctx,
            &request_id,
            "gh_api_rate_limit",
            gh_repo_state_cache_age_secs(cached_rate_limit),
        );
    }
    let output = match run_attributed_gh_command_with_ids(
        &observer_ctx,
        "gh_api_rate_limit",
        &["api", "rate_limit"],
        None,
        None,
        request_id.clone(),
        call_id.clone(),
    ) {
        Ok(output) => {
            emit_gh_info_live_refresh(&observer_ctx, &request_id, "gh_api_rate_limit", &call_id);
            output
        }
        Err(err) => {
            emit_gh_info_denied(
                &observer_ctx,
                &request_id,
                "gh_api_rate_limit",
                &err.to_string(),
            );
            return Err(err).context("gh api rate_limit failed via attributed provider path");
        }
    };

    let live: GhRateLimitResponse =
        serde_json::from_str(&output).context("failed to parse gh api rate_limit response")?;
    let live_reset_at = live
        .resources
        .core
        .reset
        .and_then(|epoch| DateTime::<Utc>::from_timestamp(epoch, 0))
        .map(|ts| ts.to_rfc3339());
    let _ = update_gh_repo_state_rate_limit(
        home_dir,
        team,
        repo_scope,
        RateLimitUpdate {
            runtime: "atm".to_string(),
            remaining: live.resources.core.remaining,
            limit: live.resources.core.limit,
            reset_at: live_reset_at.clone(),
            source: "atm_doctor",
        },
    );
    let cached_used_in_window: u64 = team_records
        .iter()
        .map(|record| record.budget_used_in_window)
        .sum();
    let cached_rate_limit = team_records
        .iter()
        .filter_map(|record| record.rate_limit.as_ref())
        .max_by_key(|rate| rate.updated_at.clone());
    let consumed_live = live
        .resources
        .core
        .limit
        .saturating_sub(live.resources.core.remaining);
    Ok(Some(GhRateLimitAudit {
        live_remaining: live.resources.core.remaining,
        live_limit: live.resources.core.limit,
        live_reset_at,
        cached_used_in_window,
        repos_observed: team_records.len(),
        cached_rate_limit_remaining: cached_rate_limit.map(|rate| rate.remaining),
        cached_rate_limit_limit: cached_rate_limit.map(|rate| rate.limit),
        delta_consumed_vs_cached: consumed_live as i64 - cached_used_in_window as i64,
    }))
}

fn build_member_snapshot(
    team_config: Option<&TeamConfig>,
    daemon_states_by_agent: &HashMap<String, CanonicalMemberState>,
) -> Vec<MemberSnapshot> {
    let mut names = BTreeSet::new();
    let mut config_members: HashMap<&str, &agent_team_mail_core::schema::AgentMember> =
        HashMap::new();

    if let Some(cfg) = team_config {
        for member in &cfg.members {
            names.insert(member.name.clone());
            config_members.insert(member.name.as_str(), member);
        }
    }
    for state in daemon_states_by_agent.values() {
        names.insert(state.agent.clone());
    }

    names
        .into_iter()
        .map(|name| {
            if let Some(member) = config_members.get(name.as_str()) {
                MemberSnapshot {
                    name: name.clone(),
                    agent_type: member.agent_type.clone(),
                    model: member.model.clone(),
                    status: snapshot_status_from_canonical_state(
                        daemon_states_by_agent.get(name.as_str()),
                    ),
                    activity: snapshot_activity_from_canonical_state(
                        daemon_states_by_agent.get(name.as_str()),
                    ),
                    session_id: daemon_states_by_agent
                        .get(name.as_str())
                        .and_then(|s| s.session_id.clone()),
                    process_id: daemon_states_by_agent
                        .get(name.as_str())
                        .and_then(|s| s.process_id),
                }
            } else {
                MemberSnapshot {
                    name: format!("{name} {UNREGISTERED_MARKER}"),
                    agent_type: UNREGISTERED_MARKER.to_string(),
                    model: UNREGISTERED_MARKER.to_string(),
                    status: snapshot_status_from_canonical_state(
                        daemon_states_by_agent.get(name.as_str()),
                    ),
                    activity: snapshot_activity_from_canonical_state(
                        daemon_states_by_agent.get(name.as_str()),
                    ),
                    session_id: daemon_states_by_agent
                        .get(name.as_str())
                        .and_then(|s| s.session_id.clone()),
                    process_id: daemon_states_by_agent
                        .get(name.as_str())
                        .and_then(|s| s.process_id),
                }
            }
        })
        .collect()
}

fn snapshot_status_from_canonical_state(state: Option<&CanonicalMemberState>) -> String {
    match state.map(|s| s.state.as_str()) {
        Some("active") | Some("idle") => "Online".to_string(),
        Some("offline") | Some("dead") => "Offline".to_string(),
        _ => "Unknown".to_string(),
    }
}

fn snapshot_activity_from_canonical_state(state: Option<&CanonicalMemberState>) -> String {
    match state.map(|s| s.activity.as_str()) {
        Some("busy") => "Busy".to_string(),
        Some("idle") => "Idle".to_string(),
        _ => "Unknown".to_string(),
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

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_override(name: &str) -> Option<EnvOverrideValue> {
    nonempty_env(name).map(|value| EnvOverrideValue {
        source: "env".to_string(),
        value,
    })
}

fn active_env_overrides() -> EnvOverrides {
    EnvOverrides {
        atm_home: env_override("ATM_HOME"),
        atm_team: env_override("ATM_TEAM"),
        atm_identity: env_override("ATM_IDENTITY"),
    }
}

fn count_nested_hook_command_matches(array: &[serde_json::Value], cmd: &str) -> usize {
    array
        .iter()
        .map(|entry| {
            if entry
                .get("command")
                .and_then(|value| value.as_str())
                .is_some_and(|value| value == cmd)
            {
                1
            } else {
                entry
                    .get("hooks")
                    .and_then(|hooks| hooks.as_array())
                    .map(|hooks| {
                        hooks
                            .iter()
                            .filter(|hook| {
                                hook.get("command")
                                    .and_then(|value| value.as_str())
                                    .is_some_and(|value| value == cmd)
                            })
                            .count()
                    })
                    .unwrap_or(0)
            }
        })
        .sum()
}

fn hook_script_findings(scripts_dir: &Path, scripts: &[&str]) -> Vec<Finding> {
    scripts
        .iter()
        .filter_map(|script| {
            let path = scripts_dir.join(script);
            (!path.is_file()).then(|| {
                finding(
                    Severity::Warn,
                    "hook_audit",
                    "HOOK_SCRIPT_MISSING",
                    format!("Expected hook script missing: {}", path.display()),
                )
            })
        })
        .collect()
}

fn audit_claude_command(
    findings: &mut Vec<Finding>,
    hooks: &serde_json::Map<String, serde_json::Value>,
    category: &str,
    label: &str,
    command: &str,
) {
    let Some(array) = hooks.get(category).and_then(|value| value.as_array()) else {
        findings.push(finding(
            Severity::Warn,
            "hook_audit",
            "HOOK_COMMAND_MISSING",
            format!("Claude hook '{}' missing from hooks.{}", label, category),
        ));
        return;
    };

    let matches = if matches!(
        category,
        "SessionStart" | "SessionEnd" | "PermissionRequest" | "Stop"
    ) {
        if catch_all_hook_command_present(array, command) {
            count_nested_hook_command_matches(array, command)
        } else {
            0
        }
    } else {
        count_nested_hook_command_matches(array, command)
    };

    if matches == 0 {
        findings.push(finding(
            Severity::Warn,
            "hook_audit",
            "HOOK_COMMAND_MISSING",
            format!(
                "Claude hook '{}' is not installed with expected command '{}' in hooks.{}",
                label, command, category
            ),
        ));
    } else if matches > 1 {
        findings.push(finding(
            Severity::Warn,
            "hook_audit",
            "HOOK_COMMAND_DUPLICATED",
            format!(
                "Claude hook '{}' appears {} times in hooks.{}; expected exactly 1",
                label, matches, category
            ),
        ));
    }
}

fn audit_gemini_command(
    findings: &mut Vec<Finding>,
    hooks: &serde_json::Map<String, serde_json::Value>,
    category: &str,
    label: &str,
    command: &str,
) {
    let Some(array) = hooks.get(category).and_then(|value| value.as_array()) else {
        findings.push(finding(
            Severity::Warn,
            "hook_audit",
            "HOOK_COMMAND_MISSING",
            format!("Gemini hook '{}' missing from hooks.{}", label, category),
        ));
        return;
    };

    let matches = count_nested_hook_command_matches(array, command);
    if matches == 0 {
        findings.push(finding(
            Severity::Warn,
            "hook_audit",
            "HOOK_COMMAND_MISSING",
            format!(
                "Gemini hook '{}' is not installed with expected command '{}' in hooks.{}",
                label, command, category
            ),
        ));
    } else if matches > 1 {
        findings.push(finding(
            Severity::Warn,
            "hook_audit",
            "HOOK_COMMAND_DUPLICATED",
            format!(
                "Gemini hook '{}' appears {} times in hooks.{}; expected exactly 1",
                label, matches, category
            ),
        ));
    }
}

fn selected_claude_hook_root(home_dir: &Path, current_dir: &Path) -> (PathBuf, bool) {
    let local_root = current_dir.join(".claude");
    let global_root = claude_root_dir_for(home_dir);
    if local_root != global_root && local_root.join("settings.json").exists() {
        (local_root, true)
    } else {
        (global_root, false)
    }
}

fn check_hook_audit(home_dir: &Path, current_dir: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let (claude_root, local_claude_install) = selected_claude_hook_root(home_dir, current_dir);
    let claude_scripts_dir = claude_root.join("scripts");
    let claude_command_scripts_dir: Option<&Path> = if local_claude_install {
        None
    } else {
        Some(claude_scripts_dir.as_path())
    };

    findings.extend(hook_script_findings(
        &claude_scripts_dir,
        &[
            "session-start.py",
            "session-end.py",
            "permission-request-relay.py",
            "stop-relay.py",
            "notification-idle-relay.py",
            "atm-identity-write.py",
            "gate-agent-spawns.py",
            "atm-identity-cleanup.py",
            "atm-hook-relay.py",
            "atm_hook_lib.py",
            "teammate-idle-relay.py",
        ],
    ));

    let claude_settings_path = claude_root.join("settings.json");
    match fs::read_to_string(&claude_settings_path) {
        Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(settings) => {
                if let Some(hooks) = settings.get("hooks").and_then(|value| value.as_object()) {
                    audit_claude_command(
                        &mut findings,
                        hooks,
                        "SessionStart",
                        "SessionStart",
                        &session_start_cmd(claude_command_scripts_dir),
                    );
                    audit_claude_command(
                        &mut findings,
                        hooks,
                        "SessionEnd",
                        "SessionEnd",
                        &session_end_cmd(claude_command_scripts_dir),
                    );
                    audit_claude_command(
                        &mut findings,
                        hooks,
                        "PermissionRequest",
                        "PermissionRequest",
                        &permission_request_cmd(claude_command_scripts_dir),
                    );
                    audit_claude_command(
                        &mut findings,
                        hooks,
                        "Stop",
                        "Stop",
                        &stop_cmd(claude_command_scripts_dir),
                    );
                    audit_claude_command(
                        &mut findings,
                        hooks,
                        "Notification",
                        "Notification(idle_prompt)",
                        &notification_idle_prompt_cmd(claude_command_scripts_dir),
                    );
                    audit_claude_command(
                        &mut findings,
                        hooks,
                        "PreToolUse",
                        "PreToolUse(Bash)",
                        &pre_tool_use_bash_cmd(claude_command_scripts_dir),
                    );
                    audit_claude_command(
                        &mut findings,
                        hooks,
                        "PreToolUse",
                        "PreToolUse(Task)",
                        &pre_tool_use_task_cmd(claude_command_scripts_dir),
                    );
                    audit_claude_command(
                        &mut findings,
                        hooks,
                        "PostToolUse",
                        "PostToolUse(Bash)",
                        &post_tool_use_bash_cmd(claude_command_scripts_dir),
                    );
                } else {
                    findings.push(finding(
                        Severity::Warn,
                        "hook_audit",
                        "HOOK_CONFIG_INVALID",
                        format!(
                            "Claude hook settings missing JSON object at {}",
                            claude_settings_path.display()
                        ),
                    ));
                }
            }
            Err(err) => findings.push(finding(
                Severity::Warn,
                "hook_audit",
                "HOOK_CONFIG_INVALID",
                format!(
                    "Failed to parse Claude settings {}: {err}",
                    claude_settings_path.display()
                ),
            )),
        },
        Err(err) => findings.push(finding(
            Severity::Warn,
            "hook_audit",
            "HOOK_CONFIG_MISSING",
            format!(
                "Claude settings missing or unreadable at {}: {err}",
                claude_settings_path.display()
            ),
        )),
    }

    let codex_config_path = home_dir.join(".codex/config.toml");
    if runtime_detected("codex", &codex_config_path) {
        let expected_notify_script = claude_scripts_dir
            .join("atm-hook-relay.py")
            .to_string_lossy()
            .replace('\\', "/");
        match fs::read_to_string(&codex_config_path) {
            Ok(raw) => match raw.parse::<toml::Table>() {
                Ok(table) => match table.get("notify").and_then(|value| value.as_array()) {
                    Some(array)
                        if codex_notify_matches_expected(array, &expected_notify_script) => {}
                    Some(_) => findings.push(finding(
                        Severity::Warn,
                        "hook_audit",
                        "HOOK_COMMAND_MISMATCH",
                        format!(
                            "Codex notify config in {} does not match expected ATM relay",
                            codex_config_path.display()
                        ),
                    )),
                    None => findings.push(finding(
                        Severity::Warn,
                        "hook_audit",
                        "HOOK_COMMAND_MISSING",
                        format!(
                            "Codex notify hook missing in {}",
                            codex_config_path.display()
                        ),
                    )),
                },
                Err(err) => findings.push(finding(
                    Severity::Warn,
                    "hook_audit",
                    "HOOK_CONFIG_INVALID",
                    format!(
                        "Failed to parse Codex config {}: {err}",
                        codex_config_path.display()
                    ),
                )),
            },
            Err(err) => findings.push(finding(
                Severity::Warn,
                "hook_audit",
                "HOOK_CONFIG_MISSING",
                format!(
                    "Codex config missing or unreadable at {}: {err}",
                    codex_config_path.display()
                ),
            )),
        }
    }

    let gemini_root = home_dir.join(".gemini");
    let gemini_settings_path = gemini_root.join("settings.json");
    if runtime_detected("gemini", &gemini_root) {
        match fs::read_to_string(&gemini_settings_path) {
            Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(settings) => {
                    if let Some(hooks) = settings.get("hooks").and_then(|value| value.as_object()) {
                        let session_start = format!(
                            "python3 \"{}\"",
                            claude_scripts_dir
                                .join("session-start.py")
                                .to_string_lossy()
                        );
                        let session_end = format!(
                            "python3 \"{}\"",
                            claude_scripts_dir.join("session-end.py").to_string_lossy()
                        );
                        let after_agent = format!(
                            "python3 \"{}\"",
                            claude_scripts_dir
                                .join("teammate-idle-relay.py")
                                .to_string_lossy()
                        );
                        audit_gemini_command(
                            &mut findings,
                            hooks,
                            "SessionStart",
                            "SessionStart",
                            &session_start,
                        );
                        audit_gemini_command(
                            &mut findings,
                            hooks,
                            "SessionEnd",
                            "SessionEnd",
                            &session_end,
                        );
                        audit_gemini_command(
                            &mut findings,
                            hooks,
                            "AfterAgent",
                            "AfterAgent",
                            &after_agent,
                        );
                    } else {
                        findings.push(finding(
                            Severity::Warn,
                            "hook_audit",
                            "HOOK_CONFIG_INVALID",
                            format!(
                                "Gemini hook settings missing JSON object at {}",
                                gemini_settings_path.display()
                            ),
                        ));
                    }
                }
                Err(err) => findings.push(finding(
                    Severity::Warn,
                    "hook_audit",
                    "HOOK_CONFIG_INVALID",
                    format!(
                        "Failed to parse Gemini settings {}: {err}",
                        gemini_settings_path.display()
                    ),
                )),
            },
            Err(err) => findings.push(finding(
                Severity::Warn,
                "hook_audit",
                "HOOK_CONFIG_MISSING",
                format!(
                    "Gemini settings missing or unreadable at {}: {err}",
                    gemini_settings_path.display()
                ),
            )),
        }
    }

    findings
}

fn codex_notify_matches_expected(array: &[toml::Value], expected_script: &str) -> bool {
    if array.len() != 2 {
        return false;
    }

    let python = array[0].as_str().map(str::trim).unwrap_or_default();
    let script = array[1].as_str().map(str::trim).unwrap_or_default();
    let python_name_matches =
        Path::new(python).file_name().and_then(|name| name.to_str()) == Some("python3");

    python_name_matches && script.replace('\\', "/") == expected_script
}

fn check_daemon_health(home_dir: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();

    let running = daemon_is_running();
    let socket_path =
        daemon_socket_path().unwrap_or_else(|_| home_dir.join(".atm/daemon/atm-daemon.sock"));
    let pid_path =
        daemon_pid_path().unwrap_or_else(|_| home_dir.join(".atm/daemon/atm-daemon.pid"));
    let lock_path = daemon_lock_path().unwrap_or_else(|_| home_dir.join(".atm/daemon/daemon.lock"));
    let status_path = daemon_status_path_for(home_dir);

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

    findings.extend(check_competing_daemon_touch(home_dir, &pid_path));

    findings
}

fn check_competing_daemon_touch(home_dir: &Path, pid_path: &Path) -> Vec<Finding> {
    let touch_path = daemon_touch_path_for(home_dir);
    let Ok(raw) = fs::read_to_string(&touch_path) else {
        return Vec::new();
    };
    let Ok(snapshot) = serde_json::from_str::<DaemonTouchSnapshot>(&raw) else {
        return Vec::new();
    };
    let current_pid = fs::read_to_string(pid_path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .or_else(|| read_daemon_lock_metadata(home_dir).map(|meta| meta.pid));

    snapshot
        .into_iter()
        .filter_map(|(team, entry)| {
            if current_pid.is_some_and(|pid| pid == entry.pid) || !is_pid_alive(entry.pid) {
                return None;
            }
            Some(finding(
                Severity::Critical,
                "daemon_health",
                "COMPETING_DAEMON_DETECTED",
                format!(
                    "Team '{}' daemon-touch sidecar points at live foreign pid={} (started_at={}, binary={})",
                    team, entry.pid, entry.started_at, entry.binary
                ),
            ))
        })
        .collect()
}

fn check_plugin_init_failures(home_dir: &Path) -> Vec<Finding> {
    let snapshot = read_daemon_status(home_dir);

    let mut findings = Vec::new();
    for plugin in snapshot.plugins {
        if plugin.status != "disabled_init_error" {
            continue;
        }
        findings.push(finding(
            Severity::Warn,
            "daemon_health",
            "PLUGIN_INIT_FAILED",
            format!(
                "Plugin '{}' failed initialization and is disabled: {}",
                plugin.name,
                plugin
                    .last_error
                    .unwrap_or_else(|| "plugin init failed".to_string())
            ),
        ));
    }
    findings
}

fn check_daemon_ownership_mismatch(home_dir: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let Some(metadata) = read_daemon_lock_metadata(home_dir) else {
        return findings;
    };

    let expected_home = fs::canonicalize(home_dir)
        .unwrap_or_else(|_| home_dir.to_path_buf())
        .to_string_lossy()
        .to_string();
    if !metadata.owner.home_scope.trim().is_empty() && metadata.owner.home_scope != expected_home {
        findings.push(finding(
            Severity::Warn,
            "daemon_health",
            "DAEMON_OWNERSHIP_MISMATCH",
            format!(
                "Daemon ownership mismatch: lock metadata home_scope='{}' expected='{}'",
                metadata.owner.home_scope, expected_home
            ),
        ));
    }

    let pid_path = home_dir.join(".atm/daemon/atm-daemon.pid");
    if let Ok(raw_pid) = fs::read_to_string(&pid_path)
        && let Ok(pid_from_file) = raw_pid.trim().parse::<u32>()
        && pid_from_file != metadata.pid
    {
        findings.push(finding(
            Severity::Warn,
            "daemon_health",
            "DAEMON_OWNERSHIP_MISMATCH",
            format!(
                "Daemon ownership mismatch: pid file ({pid_from_file}) != lock metadata ({})",
                metadata.pid
            ),
        ));
    }

    findings
}

fn read_daemon_status(home_dir: &Path) -> DaemonStatusSnapshot {
    let status_path = daemon_status_path_for(home_dir);
    let Ok(raw) = fs::read_to_string(&status_path) else {
        return DaemonStatusSnapshot {
            version: None,
            plugins: Vec::new(),
            logging: LoggingHealthSnapshot::default(),
        };
    };
    serde_json::from_str(&raw).unwrap_or(DaemonStatusSnapshot {
        version: None,
        plugins: Vec::new(),
        logging: LoggingHealthSnapshot::default(),
    })
}

fn read_daemon_status_uptime_secs(home_dir: &Path) -> Option<u64> {
    let status_path = daemon_status_path_for(home_dir);
    let raw = fs::read_to_string(status_path).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
    value.get("uptime_secs").and_then(serde_json::Value::as_u64)
}

fn read_active_install_milestone() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let bin_dir = exe.parent()?;
    if bin_dir.file_name()?.to_str()? != "bin" {
        return None;
    }

    let manifest_path = bin_dir.parent()?.join("manifest.json");
    let raw = fs::read_to_string(manifest_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("milestone_version")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
}

fn check_pid_session_reconciliation_with_query<F>(
    team: &str,
    cfg: &TeamConfig,
    query_states: F,
) -> (Vec<Finding>, HashMap<String, CanonicalMemberState>)
where
    F: Fn(&str) -> anyhow::Result<Option<Vec<CanonicalMemberState>>>,
{
    let mut findings = Vec::new();
    let (daemon_states, unreachable_reason): (
        HashMap<String, CanonicalMemberState>,
        Option<String>,
    ) = match query_states(team) {
        Ok(Some(states)) => (
            states
                .into_iter()
                .map(|s| (s.agent.clone(), s))
                .collect::<HashMap<_, _>>(),
            None,
        ),
        Ok(None) => (
            HashMap::new(),
            Some(format!(
                "Daemon team-scoped state query unavailable for team '{team}'"
            )),
        ),
        Err(err) => (
            HashMap::new(),
            Some(format!(
                "Daemon team-scoped state query failed for team '{team}': {err}"
            )),
        ),
    };

    if let Some(reason) = unreachable_reason {
        findings.push(finding(
            Severity::Warn,
            "daemon_health",
            "DAEMON_UNREACHABLE",
            reason,
        ));
    }

    for state in daemon_states.values() {
        if state.source != "pid_backend_validation" {
            continue;
        }
        if let Some(details) = parse_pid_backend_mismatch_reason(&state.reason) {
            findings.push(finding(
                Severity::Warn,
                "pid_session_reconciliation",
                "PID_PROCESS_MISMATCH",
                format!(
                    "Member '{}' failed daemon PID/backend validation: backend='{}' expected='{}' actual='{}' pid={}{}",
                    state.agent,
                    details.backend,
                    details.expected,
                    details.actual,
                    details.pid,
                    if state.in_config {
                        String::new()
                    } else {
                        " (daemon-only session)".to_string()
                    }
                ),
            ));
        } else {
            findings.push(finding(
                Severity::Warn,
                "pid_session_reconciliation",
                "PID_PROCESS_MISMATCH",
                format!(
                    "Member '{}' failed daemon PID/backend validation: {}{}",
                    state.agent,
                    state.reason,
                    if state.in_config {
                        String::new()
                    } else {
                        " (daemon-only session)".to_string()
                    }
                ),
            ));
        }
    }

    for member in &cfg.members {
        let daemon_state = daemon_states.get(&member.name);
        match daemon_state.map(|s| s.state.as_str()) {
            Some("offline") | Some("dead") if member.is_active == Some(true) => {
                findings.push(finding(
                    Severity::Warn,
                    "pid_session_reconciliation",
                    "ACTIVE_FLAG_STALE",
                    format!(
                        "Member '{}' has activity hint isActive=true but daemon state is dead (pid={})",
                        member.name,
                        daemon_state
                            .and_then(|s| s.process_id)
                            .map(|p| p.to_string())
                            .unwrap_or_else(|| "unknown".to_string())
                    ),
                ))
            }
            _ => {}
        }
    }

    (findings, daemon_states)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PidMismatchDetails {
    backend: String,
    expected: String,
    actual: String,
    pid: u32,
}

fn parse_pid_backend_mismatch_reason(reason: &str) -> Option<PidMismatchDetails> {
    let rest = reason.strip_prefix("pid/backend mismatch: backend='")?;
    let (backend, rest) = rest.split_once("' expected='")?;
    let (expected, rest) = rest.split_once("' actual='")?;
    let (actual, pid_part) = rest.rsplit_once("' pid=")?;
    let pid = pid_part.parse::<u32>().ok()?;
    Some(PidMismatchDetails {
        backend: backend.to_string(),
        expected: expected.to_string(),
        actual: actual.to_string(),
        pid,
    })
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
        let since_mode = if DateTime::parse_from_rfc3339(since.trim()).is_ok() {
            "since_timestamp"
        } else {
            "since_duration"
        };
        let dt = parse_since_input(since).with_context(|| {
            format!(
                "Invalid --since value: '{since}'. Use ISO-8601 or positive duration like 30m/2h/1d"
            )
        })?;
        return Ok((dt, since_mode.to_string()));
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
    resolve_caller_session_id_optional(None, None)
        .ok()
        .flatten()
        .is_some()
}

fn build_recommendations(
    team: &str,
    findings: &[Finding],
    has_session_context: bool,
) -> Vec<Recommendation> {
    let mut recs: Vec<Recommendation> = Vec::new();

    let has = |code: &str| findings.iter().any(|f| f.code == code);

    if has("DAEMON_NOT_RUNNING") || has("DAEMON_UNREACHABLE") {
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
                reason: "No session context detected. Run from a managed session (or set ATM_SESSION_ID) before retrying register.".to_string(),
            });
        }
    }

    if findings.iter().any(|f| f.check == "hook_audit") {
        recs.push(Recommendation {
            command: format!("atm init {team}"),
            reason: "Install or repair expected runtime hook wiring and scripts".to_string(),
        });
    }

    recs
}

fn print_human(report: &DoctorReport) {
    print!("{}", render_human(report));
}

fn format_session_short(session_id: Option<&str>) -> String {
    let Some(session) = session_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return "-".to_string();
    };
    session.chars().take(8).collect()
}

fn render_human(report: &DoctorReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("ATM Doctor — team {}\n", report.summary.team));
    out.push_str(&format!("Generated: {}\n\n", report.summary.generated_at));

    out.push_str(&format!(
        "Findings: critical={} warn={} info={}\n",
        report.summary.counts.critical, report.summary.counts.warn, report.summary.counts.info
    ));
    if let Some(version) = &report.summary.daemon_version {
        out.push_str(&format!("Daemon version: {version}\n"));
    }
    if let Some(uptime_secs) = report.summary.uptime_secs {
        out.push_str(&format!("Daemon uptime: {uptime_secs}s\n"));
    }
    if let Some(milestone) = &report.summary.install_milestone {
        out.push_str(&format!("Install milestone: {milestone}\n"));
    }
    out.push_str(&format!(
        "Log window: {}\n\n",
        render_log_window_human(&report.log_window)
    ));
    out.push_str("Logging health:\n");
    out.push_str(&format!(
        "  schema_version: {}\n",
        report.logging_health.schema_version
    ));
    out.push_str(&format!("  state: {}\n", report.logging_health.state));
    out.push_str(&format!("  log_root: {}\n", report.logging_health.log_root));
    out.push_str(&format!(
        "  canonical_log_path: {}\n",
        report.logging_health.canonical_log_path
    ));
    out.push_str(&format!(
        "  spool_path: {}\n",
        report.logging_health.spool_path
    ));
    out.push_str(&format!(
        "  dropped_events_total: {}\n",
        report.logging_health.dropped_events_total
    ));
    out.push_str(&format!(
        "  spool_file_count: {}\n",
        report.logging_health.spool_file_count
    ));
    out.push_str(&format!(
        "  oldest_spool_age_seconds: {}\n",
        report
            .logging_health
            .oldest_spool_age_seconds
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string())
    ));
    if let Some(code) = &report.logging_health.last_error.code {
        out.push_str(&format!("  last_error.code: {code}\n"));
    }
    if let Some(message) = &report.logging_health.last_error.message {
        out.push_str(&format!("  last_error.message: {message}\n"));
    }
    if let Some(at) = &report.logging_health.last_error.at {
        out.push_str(&format!("  last_error.at: {at}\n"));
    }
    if let Some(remediation) = logging_remediation(&report.logging_health.state) {
        out.push_str(&format!("  remediation: {remediation}\n"));
    }
    out.push('\n');

    if report.env_overrides.atm_home.is_some()
        || report.env_overrides.atm_team.is_some()
        || report.env_overrides.atm_identity.is_some()
    {
        out.push_str("Active env overrides:\n");
        if let Some(v) = &report.env_overrides.atm_home {
            out.push_str(&format!("  ATM_HOME={} (source={})\n", v.value, v.source));
        }
        if let Some(v) = &report.env_overrides.atm_team {
            out.push_str(&format!("  ATM_TEAM={} (source={})\n", v.value, v.source));
        }
        if let Some(v) = &report.env_overrides.atm_identity {
            out.push_str(&format!(
                "  ATM_IDENTITY={} (source={})\n",
                v.value, v.source
            ));
        }
        out.push('\n');
    }

    if let Some(audit) = &report.gh_rate_limit_audit {
        out.push_str("GitHub rate audit:\n");
        out.push_str(&format!(
            "  live_remaining: {}/{}\n",
            audit.live_remaining, audit.live_limit
        ));
        out.push_str(&format!(
            "  cached_used_in_window: {}\n",
            audit.cached_used_in_window
        ));
        out.push_str(&format!("  repos_observed: {}\n", audit.repos_observed));
        if let (Some(remaining), Some(limit)) = (
            audit.cached_rate_limit_remaining,
            audit.cached_rate_limit_limit,
        ) {
            out.push_str(&format!("  cached_rate_limit: {remaining}/{limit}\n"));
        }
        if let Some(reset_at) = &audit.live_reset_at {
            out.push_str(&format!("  live_reset_at: {reset_at}\n"));
        }
        out.push_str(&format!(
            "  delta_consumed_vs_cached: {}\n\n",
            audit.delta_consumed_vs_cached
        ));
    }

    if !report.member_snapshot.is_empty() {
        out.push_str("Members:\n");
        out.push_str(&format!(
            "  {:<20} {:<20} {:<24} {:<10} {:<10} {:<8} {}\n",
            "Name", "Type", "Model", "Status", "Activity", "PID", "Session ID"
        ));
        out.push_str(&format!("  {}\n", "─".repeat(120)));
        for m in &report.member_snapshot {
            let pid = m
                .process_id
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string());
            let session = format_session_short(m.session_id.as_deref());
            out.push_str(&format!(
                "  {:<20} {:<20} {:<24} {:<10} {:<10} {:<8} {}\n",
                m.name, m.agent_type, m.model, m.status, m.activity, pid, session
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

fn render_log_window_human(window: &LogWindow) -> String {
    let elapsed = human_duration(window.elapsed_secs);
    match window.mode.as_str() {
        "default_incremental" | "since_duration" => format!("last {elapsed}"),
        "since_timestamp" => parse_utc_timestamp(&window.start)
            .map(|dt| format!("since {} ({elapsed})", dt.format("%Y-%m-%d %H:%M:%S UTC")))
            .unwrap_or_else(|| fallback_log_window_human(window)),
        "full" => format!("since session start ({elapsed})"),
        _ => fallback_log_window_human(window),
    }
}

fn fallback_log_window_human(window: &LogWindow) -> String {
    format!("{} -> {} ({})", window.start, window.end, window.mode)
}

fn parse_utc_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn human_duration(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3_600 {
        return format!("{}m", seconds / 60);
    }
    if seconds < 86_400 {
        return format!("{}h", seconds / 3_600);
    }
    format!("{}d", seconds / 86_400)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_core::schema::AgentMember;
    use serial_test::serial;

    struct EnvGuard {
        vars: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn isolate(keys: &'static [&'static str]) -> Self {
            let mut vars = Vec::with_capacity(keys.len());
            unsafe {
                for key in keys {
                    vars.push((*key, std::env::var(key).ok()));
                    std::env::remove_var(key);
                }
            }
            Self { vars }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                for (key, original) in &self.vars {
                    match original {
                        Some(v) => std::env::set_var(key, v),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    const OVERRIDE_ENV_KEYS: &[&str] = &["ATM_HOME", "ATM_TEAM", "ATM_IDENTITY"];

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
    fn format_session_short_always_uses_8_char_prefix() {
        assert_eq!(
            format_session_short(Some("123e4567-e89b-12d3-a456-426614174000")),
            "123e4567"
        );
        assert_eq!(
            format_session_short(Some("codex-thread-abc1234def567890")),
            "codex-th"
        );
        assert_eq!(format_session_short(Some("sess-123456789")), "sess-123");
        assert_eq!(format_session_short(Some("sess-1")), "sess-1");
        assert_eq!(format_session_short(None), "-");
        assert_eq!(format_session_short(Some("   ")), "-");
    }

    #[test]
    fn snapshot_status_from_canonical_state_maps_active_offline_unknown() {
        let active = CanonicalMemberState {
            agent: "a".to_string(),
            state: "active".to_string(),
            activity: "busy".to_string(),
            session_id: Some("s1".to_string()),
            process_id: Some(1234),
            last_alive_at: None,
            reason: "x".to_string(),
            source: "session_registry".to_string(),
            in_config: true,
        };
        let dead = CanonicalMemberState {
            state: "offline".to_string(),
            ..active.clone()
        };
        let unknown = CanonicalMemberState {
            state: "unknown".to_string(),
            ..active.clone()
        };
        assert_eq!(
            snapshot_status_from_canonical_state(Some(&active)),
            "Online".to_string()
        );
        assert_eq!(
            snapshot_status_from_canonical_state(Some(&dead)),
            "Offline".to_string()
        );
        assert_eq!(
            snapshot_status_from_canonical_state(Some(&unknown)),
            "Unknown".to_string()
        );
        assert_eq!(snapshot_status_from_canonical_state(None), "Unknown");
    }

    #[test]
    fn build_member_snapshot_includes_daemon_only_members_as_unregistered() {
        let cfg = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "sess-0".to_string(),
            members: vec![member("team-lead", Some(false), 0)],
            unknown_fields: HashMap::new(),
        };
        let mut daemon_states = HashMap::new();
        daemon_states.insert(
            "arch-ctm".to_string(),
            CanonicalMemberState {
                agent: "arch-ctm".to_string(),
                state: "active".to_string(),
                activity: "busy".to_string(),
                session_id: Some("sess-1".to_string()),
                process_id: Some(4242),
                last_alive_at: None,
                reason: "session active".to_string(),
                source: "session_registry".to_string(),
                in_config: false,
            },
        );

        let snapshot = build_member_snapshot(Some(&cfg), &daemon_states);
        let ghost = snapshot
            .iter()
            .find(|m| m.name.contains("arch-ctm"))
            .expect("daemon-only member missing from snapshot");
        assert!(
            ghost.name.contains(UNREGISTERED_MARKER),
            "daemon-only member name should include marker"
        );
        assert_eq!(ghost.agent_type, UNREGISTERED_MARKER);
        assert_eq!(ghost.model, UNREGISTERED_MARKER);
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
    fn build_recommendations_includes_daemon_start_for_unreachable() {
        let findings = vec![finding(
            Severity::Warn,
            "daemon_health",
            "DAEMON_UNREACHABLE",
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
    fn compute_log_window_since_duration_sets_duration_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let args = DoctorArgs {
            team: Some("atm-dev".to_string()),
            json: false,
            since: Some("30m".to_string()),
            errors_only: false,
            full: false,
        };
        let (_start, mode) = compute_log_window_start(tmp.path(), "atm-dev", None, &args).unwrap();
        assert_eq!(mode, "since_duration");
    }

    #[test]
    fn compute_log_window_since_timestamp_sets_timestamp_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let args = DoctorArgs {
            team: Some("atm-dev".to_string()),
            json: false,
            since: Some("2026-03-03T10:00:00Z".to_string()),
            errors_only: false,
            full: false,
        };
        let (start, mode) = compute_log_window_start(tmp.path(), "atm-dev", None, &args).unwrap();
        assert_eq!(mode, "since_timestamp");
        assert_eq!(start.to_rfc3339(), "2026-03-03T10:00:00+00:00");
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
    fn check_roster_session_integrity_ignores_foreign_team_name_collision() {
        let cfg = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "s".to_string(),
            members: vec![
                member("team-lead", Some(true), 0),
                member("shared-agent", None, 0),
            ],
            unknown_fields: HashMap::new(),
        };

        let atm_dev_agents = vec![
            AgentSummary {
                agent: "team-lead".to_string(),
                state: "idle".to_string(),
            },
            AgentSummary {
                agent: "shared-agent".to_string(),
                state: "idle".to_string(),
            },
        ];
        let other_team_agents = vec![AgentSummary {
            agent: "shared-agent".to_string(),
            state: "active".to_string(),
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
            "foreign-team same-name agents must not bleed into this team's roster checks"
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
            last_seen_at: None,
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
            last_seen_at: None,
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
    fn build_recommendations_includes_atm_init_for_hook_audit() {
        let findings = vec![finding(
            Severity::Warn,
            "hook_audit",
            "HOOK_COMMAND_MISSING",
            "x".to_string(),
        )];
        let recs = build_recommendations("atm-dev", &findings, true);
        assert!(recs.iter().any(|r| r.command == "atm init atm-dev"));
    }

    #[test]
    fn check_pid_session_reconciliation_query_error_only_reports_daemon_unreachable() {
        let cfg = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "s".to_string(),
            members: vec![member("worker-a", Some(true), 0)],
            unknown_fields: HashMap::new(),
        };

        let (findings, _) = check_pid_session_reconciliation_with_query("atm-dev", &cfg, |_| {
            Err(anyhow::anyhow!("daemon unavailable"))
        });
        assert!(findings.iter().any(|f| f.code == "DAEMON_UNREACHABLE"));
        assert!(!findings.iter().any(|f| f.code == "ACTIVE_WITHOUT_SESSION"));
    }

    #[test]
    fn check_pid_session_reconciliation_query_none_only_reports_daemon_unreachable() {
        let cfg = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "s".to_string(),
            members: vec![member("worker-a", Some(true), 0)],
            unknown_fields: HashMap::new(),
        };

        let (findings, _) =
            check_pid_session_reconciliation_with_query("atm-dev", &cfg, |_| Ok(None));
        assert!(
            findings
                .iter()
                .any(|f| f.code == "DAEMON_UNREACHABLE" && f.message.contains("unavailable"))
        );
        assert!(!findings.iter().any(|f| f.code == "ACTIVE_WITHOUT_SESSION"));
    }

    #[test]
    fn check_pid_session_reconciliation_ignores_foreign_state_entries() {
        let cfg = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "s".to_string(),
            members: vec![member("team-lead", Some(false), 0)],
            unknown_fields: HashMap::new(),
        };

        let (findings, _) = check_pid_session_reconciliation_with_query("atm-dev", &cfg, |_| {
            Ok(Some(vec![CanonicalMemberState {
                agent: "foreign-agent".to_string(),
                state: "offline".to_string(),
                activity: "unknown".to_string(),
                session_id: Some("foreign-sess".to_string()),
                process_id: Some(9),
                last_alive_at: None,
                reason: "foreign team state".to_string(),
                source: "session_registry".to_string(),
                in_config: false,
            }]))
        });
        assert!(
            findings.is_empty(),
            "foreign-team daemon states must not create findings for this team's inactive members"
        );
    }

    #[test]
    fn check_pid_session_reconciliation_allows_live_daemon_state_without_activity_hint() {
        let cfg_none = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "s".to_string(),
            members: vec![member("worker-a", None, 0)],
            unknown_fields: HashMap::new(),
        };

        let (findings_none, _) =
            check_pid_session_reconciliation_with_query("atm-dev", &cfg_none, |_| {
                Ok(Some(vec![CanonicalMemberState {
                    agent: "worker-a".to_string(),
                    state: "active".to_string(),
                    activity: "busy".to_string(),
                    session_id: Some("sess-1".to_string()),
                    process_id: Some(4321),
                    last_alive_at: None,
                    reason: "session active".to_string(),
                    source: "session_registry".to_string(),
                    in_config: true,
                }]))
            });
        assert!(
            findings_none.is_empty(),
            "live daemon state with no activity hint must not be treated as a ghost session"
        );

        let cfg_false = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "s".to_string(),
            members: vec![member("worker-a", Some(false), 0)],
            unknown_fields: HashMap::new(),
        };

        let (findings_false, _) =
            check_pid_session_reconciliation_with_query("atm-dev", &cfg_false, |_| {
                Ok(Some(vec![CanonicalMemberState {
                    agent: "worker-a".to_string(),
                    state: "active".to_string(),
                    activity: "busy".to_string(),
                    session_id: Some("sess-1".to_string()),
                    process_id: Some(4321),
                    last_alive_at: None,
                    reason: "session active".to_string(),
                    source: "session_registry".to_string(),
                    in_config: true,
                }]))
            });
        assert!(
            findings_false.is_empty(),
            "live daemon state with isActive=false must not be treated as a ghost session"
        );

        let cfg_true = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "s".to_string(),
            members: vec![member("worker-a", Some(true), 0)],
            unknown_fields: HashMap::new(),
        };

        let (findings_true, _) =
            check_pid_session_reconciliation_with_query("atm-dev", &cfg_true, |_| {
                Ok(Some(vec![CanonicalMemberState {
                    agent: "worker-a".to_string(),
                    state: "active".to_string(),
                    activity: "busy".to_string(),
                    session_id: Some("sess-1".to_string()),
                    process_id: Some(4321),
                    last_alive_at: None,
                    reason: "session active".to_string(),
                    source: "session_registry".to_string(),
                    in_config: true,
                }]))
            });
        assert!(
            findings_true.is_empty(),
            "live daemon state with isActive=true should remain the clean happy path"
        );

        let (findings_idle_none, _) =
            check_pid_session_reconciliation_with_query("atm-dev", &cfg_none, |_| {
                Ok(Some(vec![CanonicalMemberState {
                    agent: "worker-a".to_string(),
                    state: "idle".to_string(),
                    activity: "idle".to_string(),
                    session_id: Some("sess-1".to_string()),
                    process_id: Some(4321),
                    last_alive_at: None,
                    reason: "session idle".to_string(),
                    source: "session_registry".to_string(),
                    in_config: true,
                }]))
            });
        assert!(
            findings_idle_none.is_empty(),
            "idle daemon state with no activity hint must not be treated as a ghost session"
        );

        let (findings_idle_false, _) =
            check_pid_session_reconciliation_with_query("atm-dev", &cfg_false, |_| {
                Ok(Some(vec![CanonicalMemberState {
                    agent: "worker-a".to_string(),
                    state: "idle".to_string(),
                    activity: "idle".to_string(),
                    session_id: Some("sess-1".to_string()),
                    process_id: Some(4321),
                    last_alive_at: None,
                    reason: "session idle".to_string(),
                    source: "session_registry".to_string(),
                    in_config: true,
                }]))
            });
        assert!(
            findings_idle_false.is_empty(),
            "idle daemon state with isActive=false must not be treated as a ghost session"
        );
    }

    #[test]
    fn parse_pid_backend_mismatch_reason_extracts_expected_fields() {
        let parsed = parse_pid_backend_mismatch_reason(
            "pid/backend mismatch: backend='codex' expected='comm=codex' actual='zsh' pid=4242",
        )
        .expect("valid mismatch reason");
        assert_eq!(parsed.backend, "codex");
        assert_eq!(parsed.expected, "comm=codex");
        assert_eq!(parsed.actual, "zsh");
        assert_eq!(parsed.pid, 4242);
    }

    #[test]
    fn check_pid_session_reconciliation_reports_pid_mismatch_for_daemon_only_state() {
        let cfg = TeamConfig {
            name: "atm-dev".to_string(),
            description: None,
            created_at: 0,
            lead_agent_id: "team-lead@atm-dev".to_string(),
            lead_session_id: "s".to_string(),
            members: vec![member("team-lead", Some(false), 0)],
            unknown_fields: HashMap::new(),
        };

        let (findings, _) = check_pid_session_reconciliation_with_query("atm-dev", &cfg, |_| {
            Ok(Some(vec![CanonicalMemberState {
                agent: "arch-ctm".to_string(),
                state: "offline".to_string(),
                activity: "unknown".to_string(),
                session_id: Some("sess-ghost".to_string()),
                process_id: Some(9999),
                last_alive_at: None,
                reason: "pid/backend mismatch: backend='codex' expected='comm=codex' actual='zsh' pid=9999"
                    .to_string(),
                source: "pid_backend_validation".to_string(),
                in_config: false,
            }]))
        });

        let mismatch = findings
            .iter()
            .find(|f| f.code == "PID_PROCESS_MISMATCH")
            .expect("expected pid mismatch finding");
        assert!(mismatch.message.contains("backend='codex'"));
        assert!(mismatch.message.contains("expected='comm=codex'"));
        assert!(mismatch.message.contains("actual='zsh'"));
        assert!(mismatch.message.contains("pid=9999"));
        assert!(mismatch.message.contains("daemon-only session"));
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
    #[serial]
    fn check_plugin_init_failures_reports_disabled_init_error() {
        let _guard = EnvGuard::isolate(OVERRIDE_ENV_KEYS);
        let tmp = tempfile::tempdir().unwrap();
        let daemon_dir = tmp.path().join(".atm/daemon");
        fs::create_dir_all(&daemon_dir).unwrap();
        fs::write(
            daemon_dir.join("status.json"),
            r#"{
  "plugins": [
    {"name": "gh_monitor", "status": "running"},
    {"name": "issues", "status": "disabled_init_error", "last_error": "plugin init failed: bad token"}
  ]
}"#,
        )
        .unwrap();
        unsafe { std::env::set_var("ATM_HOME", tmp.path()) };

        let findings = check_plugin_init_failures(tmp.path());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "PLUGIN_INIT_FAILED");
        assert!(findings[0].message.contains("issues"));
        assert!(findings[0].message.contains("bad token"));
    }

    #[test]
    fn read_daemon_status_uptime_secs_reads_existing_status_field() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon_dir = tmp.path().join(".atm/daemon");
        fs::create_dir_all(&daemon_dir).unwrap();
        fs::write(
            daemon_dir.join("status.json"),
            r#"{"timestamp":"2026-03-02T00:00:00Z","pid":42,"version":"0.44.1","uptime_secs":123,"plugins":[],"teams":[]}"#,
        )
        .unwrap();

        assert_eq!(read_daemon_status_uptime_secs(tmp.path()), Some(123));
    }

    #[test]
    fn check_daemon_ownership_mismatch_reports_home_scope_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon_dir = tmp.path().join(".atm/daemon");
        fs::create_dir_all(&daemon_dir).unwrap();
        fs::write(
            daemon_dir.join("daemon.lock.meta.json"),
            r#"{
  "pid": 42,
  "runtime_kind": "dev",
  "build_profile": "release",
  "executable_path": "/tmp/atm-daemon",
  "home_scope": "/tmp/other-home",
  "version": "0.44.1",
  "written_at": "2026-03-02T00:00:00Z"
}"#,
        )
        .unwrap();

        let findings = check_daemon_ownership_mismatch(tmp.path());
        assert!(
            findings.iter().any(|f| {
                f.code == "DAEMON_OWNERSHIP_MISMATCH" && f.message.contains("/tmp/other-home")
            }),
            "expected home-scope mismatch finding, got: {findings:?}"
        );
    }

    #[test]
    #[serial]
    fn check_daemon_health_reports_competing_live_pid_from_touch_sidecar() {
        let _guard = EnvGuard::isolate(OVERRIDE_ENV_KEYS);
        let tmp = tempfile::tempdir().unwrap();
        let daemon_dir = tmp.path().join(".atm/daemon");
        fs::create_dir_all(&daemon_dir).unwrap();
        fs::write(daemon_dir.join("atm-daemon.pid"), "999999\n").unwrap();
        fs::write(
            daemon_dir.join("daemon-touch.json"),
            format!(
                r#"{{
  "atm-dev": {{
    "pid": {},
    "started_at": "2026-03-16T00:00:00Z",
    "binary": "/tmp/foreign-atm-daemon"
  }}
}}"#,
                std::process::id()
            ),
        )
        .unwrap();
        unsafe { std::env::set_var("ATM_HOME", tmp.path()) };

        let findings = check_daemon_health(tmp.path());
        assert!(
            findings
                .iter()
                .any(|finding| finding.code == "COMPETING_DAEMON_DETECTED")
        );
    }

    #[test]
    #[serial]
    fn check_hook_audit_reports_missing_claude_settings_and_scripts() {
        let _guard = EnvGuard::isolate(&["ATM_HOME", "ATM_TEAM", "ATM_IDENTITY", "PATH"]);
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PATH", "") };

        let findings = check_hook_audit(tmp.path(), tmp.path());
        assert!(findings.iter().any(|f| f.code == "HOOK_SCRIPT_MISSING"));
        assert!(findings.iter().any(|f| f.code == "HOOK_CONFIG_MISSING"));
    }

    #[test]
    #[serial]
    fn check_hook_audit_accepts_installed_claude_hooks() {
        let _guard = EnvGuard::isolate(&["ATM_HOME", "ATM_TEAM", "ATM_IDENTITY", "PATH"]);
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PATH", "") };

        let claude_root = claude_root_dir_for(tmp.path());
        let scripts_dir = claude_root.join("scripts");
        fs::create_dir_all(&scripts_dir).unwrap();
        for script in [
            "session-start.py",
            "session-end.py",
            "permission-request-relay.py",
            "stop-relay.py",
            "notification-idle-relay.py",
            "atm-identity-write.py",
            "gate-agent-spawns.py",
            "atm-identity-cleanup.py",
            "atm-hook-relay.py",
            "atm_hook_lib.py",
            "teammate-idle-relay.py",
        ] {
            fs::write(scripts_dir.join(script), "#!/usr/bin/env python3\n").unwrap();
        }

        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": session_start_cmd(Some(&scripts_dir))}]
                }],
                "SessionEnd": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": session_end_cmd(Some(&scripts_dir))}]
                }],
                "PermissionRequest": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": permission_request_cmd(Some(&scripts_dir))}]
                }],
                "Stop": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": stop_cmd(Some(&scripts_dir))}]
                }],
                "Notification": [{
                    "matcher": "idle_prompt",
                    "hooks": [{"type": "command", "command": notification_idle_prompt_cmd(Some(&scripts_dir))}]
                }],
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": pre_tool_use_bash_cmd(Some(&scripts_dir))}]},
                    {"matcher": "Task", "hooks": [{"type": "command", "command": pre_tool_use_task_cmd(Some(&scripts_dir))}]}
                ],
                "PostToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": post_tool_use_bash_cmd(Some(&scripts_dir))}]}
                ]
            }
        });
        fs::create_dir_all(&claude_root).unwrap();
        fs::write(
            claude_root.join("settings.json"),
            serde_json::to_vec_pretty(&settings).unwrap(),
        )
        .unwrap();

        let findings = check_hook_audit(tmp.path(), tmp.path());
        assert!(
            findings.is_empty(),
            "expected clean hook audit, got: {:?}",
            findings
        );
    }

    #[test]
    #[serial]
    fn check_hook_audit_accepts_project_local_claude_hooks_under_redirected_home() {
        let _guard = EnvGuard::isolate(&["ATM_HOME", "ATM_TEAM", "ATM_IDENTITY", "PATH"]);
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PATH", "") };

        let claude_root = project.path().join(".claude");
        let scripts_dir = claude_root.join("scripts");
        fs::create_dir_all(&scripts_dir).unwrap();
        for script in [
            "session-start.py",
            "session-end.py",
            "permission-request-relay.py",
            "stop-relay.py",
            "notification-idle-relay.py",
            "atm-identity-write.py",
            "gate-agent-spawns.py",
            "atm-identity-cleanup.py",
            "atm-hook-relay.py",
            "atm_hook_lib.py",
            "teammate-idle-relay.py",
        ] {
            fs::write(scripts_dir.join(script), "#!/usr/bin/env python3\n").unwrap();
        }

        let settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": session_start_cmd(None)}]
                }],
                "SessionEnd": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": session_end_cmd(None)}]
                }],
                "PermissionRequest": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": permission_request_cmd(None)}]
                }],
                "Stop": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": stop_cmd(None)}]
                }],
                "Notification": [{
                    "matcher": "idle_prompt",
                    "hooks": [{"type": "command", "command": notification_idle_prompt_cmd(None)}]
                }],
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": pre_tool_use_bash_cmd(None)}]},
                    {"matcher": "Task", "hooks": [{"type": "command", "command": pre_tool_use_task_cmd(None)}]}
                ],
                "PostToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": post_tool_use_bash_cmd(None)}]}
                ]
            }
        });
        fs::write(
            claude_root.join("settings.json"),
            serde_json::to_vec_pretty(&settings).unwrap(),
        )
        .unwrap();

        let codex_root = home.path().join(".codex");
        fs::create_dir_all(&codex_root).unwrap();
        let relay_path_toml = scripts_dir
            .join("atm-hook-relay.py")
            .display()
            .to_string()
            .replace('\\', "/");
        fs::write(
            codex_root.join("config.toml"),
            format!("notify = [\"python3\", \"{}\"]\n", relay_path_toml),
        )
        .unwrap();

        let gemini_root = home.path().join(".gemini");
        fs::create_dir_all(&gemini_root).unwrap();
        let gemini_settings = serde_json::json!({
            "hooks": {
                "SessionStart": [{"command": format!("python3 \"{}\"", scripts_dir.join("session-start.py").display())}],
                "SessionEnd": [{"command": format!("python3 \"{}\"", scripts_dir.join("session-end.py").display())}],
                "AfterAgent": [{"command": format!("python3 \"{}\"", scripts_dir.join("teammate-idle-relay.py").display())}]
            }
        });
        fs::write(
            gemini_root.join("settings.json"),
            serde_json::to_vec_pretty(&gemini_settings).unwrap(),
        )
        .unwrap();

        let findings = check_hook_audit(home.path(), project.path());
        assert!(
            findings.is_empty(),
            "expected clean local hook audit, got: {:?}",
            findings
        );
    }

    #[test]
    fn logging_health_contract_matches_canonical_schema() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let temp_root = std::env::temp_dir();
        let spool_path = temp_root.join("spool").to_string_lossy().to_string();
        let canonical_log_path = temp_root
            .join("atm.log.jsonl")
            .to_string_lossy()
            .to_string();
        let value = serde_json::to_value(build_logging_health_contract(
            &LoggingHealthSnapshot {
                state: "degraded_spooling".to_string(),
                dropped_counter: 0,
                spool_path: spool_path.clone(),
                last_error: Some("events are queued in spool awaiting merge".to_string()),
                canonical_log_path: canonical_log_path.clone(),
                spool_count: 1,
                oldest_spool_age: Some(5),
            },
            tmp.path(),
        ))
        .expect("serialize logging_health");

        assert_eq!(value["schema_version"], "v1");
        assert_eq!(value["state"], "degraded_spooling");
        assert_eq!(value["canonical_log_path"], canonical_log_path);
        assert_eq!(value["spool_path"], spool_path);
        assert_eq!(value["dropped_events_total"], 0);
        assert_eq!(value["spool_file_count"], 1);
        assert_eq!(value["oldest_spool_age_seconds"], 5);
        assert!(value["log_root"].is_string());
        assert_eq!(value["last_error"]["code"], "DEGRADED_SPOOLING");
        assert_eq!(
            value["last_error"]["message"],
            "events are queued in spool awaiting merge"
        );
        assert!(value["last_error"]["at"].is_string());
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
                uptime_secs: Some(42),
                daemon_version: Some("0.44.1".to_string()),
                install_milestone: Some("0.44.2".to_string()),
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
                elapsed_secs: 60,
            },
            env_overrides: EnvOverrides::default(),
            gh_rate_limit_audit: None,
            logging_health: LoggingHealthContract::default(),
            members: vec![MemberSnapshot {
                name: "team-lead".to_string(),
                agent_type: "team-lead".to_string(),
                model: "claude".to_string(),
                status: "Online".to_string(),
                activity: "Busy".to_string(),
                session_id: Some("sess-1".to_string()),
                process_id: Some(4242),
            }],
            member_snapshot: vec![MemberSnapshot {
                name: "team-lead".to_string(),
                agent_type: "team-lead".to_string(),
                model: "claude".to_string(),
                status: "Online".to_string(),
                activity: "Busy".to_string(),
                session_id: Some("sess-1".to_string()),
                process_id: Some(4242),
            }],
        };
        let rendered = render_human(&report);
        assert!(rendered.contains("Model"));
        assert!(rendered.contains("Daemon uptime: 42s"));
        let members_idx = rendered.find("Members:").unwrap();
        let findings_idx = rendered.find("Findings (ordered by severity):").unwrap();
        assert!(members_idx < findings_idx);
        assert!(
            rendered.contains("remediation: logging unavailable"),
            "human output should include logging remediation for unavailable state"
        );
    }

    #[test]
    fn render_log_window_human_uses_elapsed_labels() {
        let incremental = LogWindow {
            mode: "default_incremental".to_string(),
            start: "2026-03-02T00:00:00Z".to_string(),
            end: "2026-03-02T00:01:00Z".to_string(),
            elapsed_secs: 60,
        };
        assert_eq!(render_log_window_human(&incremental), "last 1m");

        let full = LogWindow {
            mode: "full".to_string(),
            start: "2026-03-02T00:00:00Z".to_string(),
            end: "2026-03-02T02:00:00Z".to_string(),
            elapsed_secs: 7_200,
        };
        assert_eq!(render_log_window_human(&full), "since session start (2h)");

        let since_timestamp = LogWindow {
            mode: "since_timestamp".to_string(),
            start: "2026-03-02T00:00:00Z".to_string(),
            end: "2026-03-02T00:05:00Z".to_string(),
            elapsed_secs: 300,
        };
        assert_eq!(
            render_log_window_human(&since_timestamp),
            "since 2026-03-02 00:00:00 UTC (5m)"
        );
    }

    #[test]
    fn doctor_json_schema_includes_members_and_excludes_member_snapshot() {
        let atm_home = std::env::temp_dir()
            .join("atm-home")
            .to_string_lossy()
            .into_owned();
        let full_session = "123e4567-e89b-12d3-a456-426614174000".to_string();
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
                uptime_secs: Some(42),
                daemon_version: Some("0.44.1".to_string()),
                install_milestone: Some("0.44.2".to_string()),
            },
            findings: vec![],
            recommendations: vec![],
            log_window: LogWindow {
                mode: "default_incremental".to_string(),
                start: "2026-03-02T00:00:00Z".to_string(),
                end: "2026-03-02T00:01:00Z".to_string(),
                elapsed_secs: 60,
            },
            env_overrides: EnvOverrides {
                atm_home: Some(EnvOverrideValue {
                    source: "env".to_string(),
                    value: atm_home.clone(),
                }),
                atm_team: Some(EnvOverrideValue {
                    source: "env".to_string(),
                    value: "atm-dev".to_string(),
                }),
                atm_identity: Some(EnvOverrideValue {
                    source: "env".to_string(),
                    value: "arch-ctm".to_string(),
                }),
            },
            gh_rate_limit_audit: None,
            logging_health: LoggingHealthContract::default(),
            members: vec![MemberSnapshot {
                name: "arch-ctm".to_string(),
                agent_type: "codex".to_string(),
                model: "custom:codex".to_string(),
                status: "Online".to_string(),
                activity: "Busy".to_string(),
                session_id: Some(full_session.clone()),
                process_id: Some(4242),
            }],
            member_snapshot: vec![MemberSnapshot::default()],
        };
        let value = serde_json::to_value(report).unwrap();
        assert!(value.get("member_snapshot").is_none());
        assert_eq!(
            value["members"][0]["session_id"],
            serde_json::Value::String(full_session)
        );
        assert_eq!(
            value["env_overrides"]["atm_home"]["source"],
            serde_json::Value::String("env".to_string())
        );
        assert_eq!(
            value["env_overrides"]["atm_home"]["value"],
            serde_json::Value::String(atm_home)
        );
        assert_eq!(
            value["env_overrides"]["atm_team"]["value"],
            serde_json::Value::String("atm-dev".to_string())
        );
        assert_eq!(
            value["env_overrides"]["atm_identity"]["value"],
            serde_json::Value::String("arch-ctm".to_string())
        );
        assert_eq!(
            value["log_window"]["elapsed_secs"],
            serde_json::Value::Number(60u64.into())
        );
        assert_eq!(
            value["summary"]["uptime_secs"],
            serde_json::Value::Number(42u64.into())
        );
        assert!(
            value.get("logging").is_none(),
            "legacy logging key must not be serialized"
        );
        assert_eq!(
            value["logging_health"]["schema_version"],
            serde_json::Value::String("v1".to_string())
        );
        assert_eq!(
            value["logging_health"]["state"],
            serde_json::Value::String("unavailable".to_string())
        );
        assert!(value["logging_health"]["log_root"].is_string());
        assert!(value["logging_health"]["canonical_log_path"].is_string());
        assert!(value["logging_health"]["spool_path"].is_string());
        assert_eq!(
            value["logging_health"]["dropped_events_total"],
            serde_json::Value::Number(0u64.into())
        );
        assert!(value["logging_health"]["spool_file_count"].is_u64());
        assert!(value["logging_health"]["oldest_spool_age_seconds"].is_null());
        assert!(value["logging_health"]["last_error"]["code"].is_null());
        assert!(value["logging_health"]["last_error"]["message"].is_null());
        assert!(value["logging_health"]["last_error"]["at"].is_null());
    }

    #[test]
    #[serial]
    fn active_env_overrides_ignores_empty_values() {
        let _env_guard = EnvGuard::isolate(OVERRIDE_ENV_KEYS);
        unsafe {
            std::env::set_var("ATM_HOME", "   ");
            std::env::set_var("ATM_TEAM", "atm-dev");
            std::env::set_var("ATM_IDENTITY", "");
        }

        let overrides = active_env_overrides();
        assert_eq!(overrides.atm_home, None);
        assert_eq!(
            overrides.atm_team.as_ref().map(|v| v.value.as_str()),
            Some("atm-dev")
        );
        assert_eq!(
            overrides.atm_team.as_ref().map(|v| v.source.as_str()),
            Some("env")
        );
        assert_eq!(overrides.atm_identity, None);
    }

    #[test]
    fn render_human_includes_active_env_overrides() {
        let home = std::env::temp_dir()
            .join("home")
            .to_string_lossy()
            .into_owned();
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
                uptime_secs: None,
                daemon_version: Some("0.44.1".to_string()),
                install_milestone: Some("0.44.2".to_string()),
            },
            findings: vec![],
            recommendations: vec![],
            log_window: LogWindow {
                mode: "default_incremental".to_string(),
                start: "2026-03-02T00:00:00Z".to_string(),
                end: "2026-03-02T00:01:00Z".to_string(),
                elapsed_secs: 60,
            },
            env_overrides: EnvOverrides {
                atm_home: Some(EnvOverrideValue {
                    source: "env".to_string(),
                    value: home.clone(),
                }),
                atm_team: Some(EnvOverrideValue {
                    source: "env".to_string(),
                    value: "atm-dev".to_string(),
                }),
                atm_identity: Some(EnvOverrideValue {
                    source: "env".to_string(),
                    value: "arch-ctm".to_string(),
                }),
            },
            gh_rate_limit_audit: None,
            logging_health: LoggingHealthContract::default(),
            members: vec![],
            member_snapshot: vec![],
        };

        let rendered = render_human(&report);
        assert!(rendered.contains("Active env overrides:"));
        assert!(rendered.contains(&format!("ATM_HOME={home} (source=env)")));
        assert!(rendered.contains("ATM_TEAM=atm-dev"));
        assert!(rendered.contains("ATM_IDENTITY=arch-ctm"));
    }

    #[test]
    fn render_human_members_use_short_session_ids_while_snapshot_keeps_full() {
        let full_session = "123e4567-e89b-12d3-a456-426614174000";
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
                uptime_secs: None,
                daemon_version: Some("0.44.1".to_string()),
                install_milestone: Some("0.44.2".to_string()),
            },
            findings: vec![],
            recommendations: vec![],
            log_window: LogWindow {
                mode: "default_incremental".to_string(),
                start: "2026-03-02T00:00:00Z".to_string(),
                end: "2026-03-02T00:01:00Z".to_string(),
                elapsed_secs: 60,
            },
            env_overrides: EnvOverrides::default(),
            gh_rate_limit_audit: None,
            logging_health: LoggingHealthContract::default(),
            members: vec![MemberSnapshot {
                name: "arch-ctm".to_string(),
                agent_type: "codex".to_string(),
                model: "custom:codex".to_string(),
                status: "Online".to_string(),
                activity: "Busy".to_string(),
                session_id: Some(full_session.to_string()),
                process_id: Some(1234),
            }],
            member_snapshot: vec![MemberSnapshot {
                name: "arch-ctm".to_string(),
                agent_type: "codex".to_string(),
                model: "custom:codex".to_string(),
                status: "Online".to_string(),
                activity: "Busy".to_string(),
                session_id: Some(full_session.to_string()),
                process_id: Some(1234),
            }],
        };

        let rendered = render_human(&report);
        assert!(rendered.contains("123e4567"));
        assert!(!rendered.contains(full_session));

        let json_value = serde_json::to_value(report).unwrap();
        assert!(json_value.get("member_snapshot").is_none());
        assert_eq!(
            json_value["members"][0]["session_id"],
            serde_json::Value::String(full_session.to_string())
        );
    }

    #[test]
    fn render_human_members_mixed_session_formats_display_consistent_short_values() {
        let full_session = "123e4567-e89b-12d3-a456-426614174000";
        let short_session = "abcd1234";
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
                uptime_secs: None,
                daemon_version: Some("0.44.5".to_string()),
                install_milestone: Some("0.44.5-dev.1".to_string()),
            },
            findings: vec![],
            recommendations: vec![],
            log_window: LogWindow {
                mode: "default_incremental".to_string(),
                start: "2026-03-02T00:00:00Z".to_string(),
                end: "2026-03-02T00:01:00Z".to_string(),
                elapsed_secs: 60,
            },
            env_overrides: EnvOverrides::default(),
            gh_rate_limit_audit: None,
            logging_health: LoggingHealthContract::default(),
            members: vec![
                MemberSnapshot {
                    name: "arch-ctm".to_string(),
                    agent_type: "codex".to_string(),
                    model: "custom:codex".to_string(),
                    status: "Online".to_string(),
                    activity: "Busy".to_string(),
                    session_id: Some(full_session.to_string()),
                    process_id: Some(1234),
                },
                MemberSnapshot {
                    name: "atm-monitor".to_string(),
                    agent_type: "claude".to_string(),
                    model: "claude-sonnet-4-6".to_string(),
                    status: "Online".to_string(),
                    activity: "Idle".to_string(),
                    session_id: Some(short_session.to_string()),
                    process_id: Some(2222),
                },
            ],
            member_snapshot: vec![
                MemberSnapshot {
                    name: "arch-ctm".to_string(),
                    agent_type: "codex".to_string(),
                    model: "custom:codex".to_string(),
                    status: "Online".to_string(),
                    activity: "Busy".to_string(),
                    session_id: Some(full_session.to_string()),
                    process_id: Some(1234),
                },
                MemberSnapshot {
                    name: "atm-monitor".to_string(),
                    agent_type: "claude".to_string(),
                    model: "claude-sonnet-4-6".to_string(),
                    status: "Online".to_string(),
                    activity: "Idle".to_string(),
                    session_id: Some(short_session.to_string()),
                    process_id: Some(2222),
                },
            ],
        };

        let rendered = render_human(&report);
        assert!(rendered.contains("arch-ctm"));
        assert!(rendered.contains("atm-monitor"));
        assert!(rendered.contains("123e4567"));
        assert!(
            !rendered.contains(full_session),
            "full UUID must not appear in rendered output"
        );
        assert!(rendered.contains("abcd1234"));
    }

    #[test]
    fn format_session_short_returns_unchanged_when_shorter_than_8_chars() {
        // Session IDs shorter than 8 chars must be returned as-is (no padding/truncation).
        assert_eq!(format_session_short(Some("abc123")), "abc123");
        assert_eq!(format_session_short(Some("xy")), "xy");
        assert_eq!(format_session_short(Some("1234567")), "1234567");
        // Exactly 8 chars is returned in full.
        assert_eq!(format_session_short(Some("abcd1234")), "abcd1234");
    }
}
