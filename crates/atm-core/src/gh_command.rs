//! Neutral GH namespace contracts shared by the CLI and daemon/plugin layers.

use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginCapabilityDescriptor {
    pub namespace: String,
    pub plugin_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhPrListRequest {
    pub team: String,
    pub repo: String,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhPrReportRequest {
    pub team: String,
    pub repo: String,
    pub pr_number: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhCliPrereqRequest {
    pub team: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhRateLimitAuditRequest {
    pub team: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhCliPrereqStatus {
    pub gh_installed: bool,
    pub gh_authenticated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhPrListSummary {
    pub team: String,
    pub repo: String,
    pub generated_at: String,
    pub total_open_prs: usize,
    pub items: Vec<GhMonitorListItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhMonitorListItem {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub draft: bool,
    pub ci: GhCiRollup,
    pub merge: String,
    pub review: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhCiRollup {
    pub state: String,
    pub total: u64,
    pub pass: u64,
    pub fail: u64,
    pub pending: u64,
    pub skip: u64,
    pub neutral: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhPrReportSummary {
    pub schema_version: String,
    pub team: String,
    pub repo: String,
    pub generated_at: String,
    pub pr: GhMonitorReportPr,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhMonitorReportPr {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub draft: bool,
    pub ci: GhCiRollup,
    pub review_decision: String,
    pub merge: GhMergeReport,
    pub checks: Vec<GhMonitorCheckReport>,
    pub reviews: Vec<GhMonitorReviewReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhMergeReport {
    pub mergeable: String,
    pub merge_state_status: String,
    pub status: String,
    pub blocking_reasons: Vec<String>,
    pub advisory_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhMonitorCheckReport {
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhMonitorReviewReport {
    pub reviewer: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhRateLimitAudit {
    pub live_remaining: u64,
    pub live_limit: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_reset_at: Option<String>,
    pub cached_used_in_window: u64,
    pub repos_observed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_rate_limit_remaining: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_rate_limit_limit: Option<u64>,
    pub delta_consumed_vs_cached: i64,
}

pub const GH_MONITOR_REPORT_SCHEMA_VERSION: &str = "1.0.0";

pub type CliTeardownHook = Arc<dyn Fn() + Send + Sync>;

fn cli_teardown_hook_slot() -> &'static Mutex<Option<CliTeardownHook>> {
    static CLI_TEARDOWN_HOOK: OnceLock<Mutex<Option<CliTeardownHook>>> = OnceLock::new();
    CLI_TEARDOWN_HOOK.get_or_init(|| Mutex::new(None))
}

pub fn install_cli_teardown_hook(hook: CliTeardownHook) {
    *cli_teardown_hook_slot()
        .lock()
        .expect("cli teardown hook lock poisoned") = Some(hook);
}

pub fn clear_cli_teardown_hook() {
    *cli_teardown_hook_slot()
        .lock()
        .expect("cli teardown hook lock poisoned") = None;
}

pub fn run_cli_teardown_hook() {
    let hook = cli_teardown_hook_slot()
        .lock()
        .expect("cli teardown hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        hook();
    }
}

#[derive(Debug, Deserialize)]
pub struct GhPrListRow {
    pub number: u64,
    pub title: String,
    pub url: String,
    #[serde(rename = "isDraft", default)]
    pub is_draft: bool,
    #[serde(rename = "reviewDecision", default)]
    pub review_decision: Option<String>,
    #[serde(rename = "mergeStateStatus", default)]
    pub merge_state_status: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    pub status_check_rollup: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct GhPrReportRow {
    pub number: u64,
    pub title: String,
    pub url: String,
    #[serde(rename = "isDraft", default)]
    pub is_draft: bool,
    #[serde(rename = "reviewDecision", default)]
    pub review_decision: Option<String>,
    #[serde(rename = "mergeStateStatus", default)]
    pub merge_state_status: Option<String>,
    #[serde(default)]
    pub mergeable: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    pub status_check_rollup: Vec<serde_json::Value>,
    #[serde(default)]
    pub reviews: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct GhPrMergeProbe {
    #[serde(default)]
    pub mergeable: Option<String>,
    #[serde(rename = "mergeStateStatus", default)]
    pub merge_state_status: Option<String>,
}

const GH_OBSERVABILITY_LEDGER_FILE: &str = ".atm/daemon/gh-observability.jsonl";

pub fn flush_local_gh_observability_records(home_dir: &std::path::Path) -> anyhow::Result<()> {
    let paths = [
        home_dir.join(GH_OBSERVABILITY_LEDGER_FILE),
        home_dir.join(".atm/daemon/gh-monitor-repo-state.json"),
    ];

    for path in paths {
        if !path.exists() {
            continue;
        }
        let file = OpenOptions::new().append(true).open(&path).map_err(|err| {
            anyhow::anyhow!("open gh observability file {}: {err}", path.display())
        })?;
        file.sync_all().map_err(|err| {
            anyhow::anyhow!("flush gh observability file {}: {err}", path.display())
        })?;
    }

    Ok(())
}

pub fn extract_check_reports(entries: &[serde_json::Value]) -> Vec<GhMonitorCheckReport> {
    let mut checks: Vec<GhMonitorCheckReport> = entries
        .iter()
        .map(|entry| GhMonitorCheckReport {
            name: extract_check_name(entry),
            status: extract_check_status(entry),
            conclusion: extract_string_field(entry, &["conclusion"]),
            started_at: extract_string_field(entry, &["startedAt", "started_at"]),
            completed_at: extract_string_field(entry, &["completedAt", "completed_at"]),
            run_url: extract_string_field(entry, &["detailsUrl", "targetUrl", "url", "htmlUrl"]),
        })
        .collect();
    checks.sort_by(|a, b| a.name.cmp(&b.name));
    checks
}

pub fn extract_review_reports(entries: &[serde_json::Value]) -> Vec<GhMonitorReviewReport> {
    let mut reviews: Vec<GhMonitorReviewReport> = entries
        .iter()
        .map(|entry| {
            let reviewer = entry
                .get("author")
                .and_then(|author| author.get("login"))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    entry
                        .get("authorLogin")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                })
                .unwrap_or("unknown-reviewer")
                .to_string();
            let state = extract_string_field(entry, &["state"])
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_else(|| "unknown".to_string());
            GhMonitorReviewReport {
                reviewer,
                state,
                submitted_at: extract_string_field(entry, &["submittedAt", "submitted_at"]),
            }
        })
        .collect();
    reviews.sort_by(|a, b| a.reviewer.cmp(&b.reviewer));
    reviews
}

pub fn build_merge_report(
    mergeable: Option<&str>,
    merge_state_status: Option<&str>,
    draft: bool,
    ci: &GhCiRollup,
    review_decision: &str,
) -> GhMergeReport {
    let mergeable_normalized = normalize_mergeable(mergeable);
    let merge_state_status_normalized = normalize_merge_status(merge_state_status);
    let mut blocking_reasons: Vec<String> = Vec::new();
    let mut advisory_reasons: Vec<String> = Vec::new();
    if draft {
        blocking_reasons.push("PR is draft".to_string());
    }
    if mergeable_normalized == "unknown" {
        advisory_reasons.push("mergeability is UNKNOWN (transient)".to_string());
    } else if mergeable_normalized == "conflicting" {
        blocking_reasons.push("mergeability is CONFLICTING".to_string());
    }
    if matches!(
        merge_state_status_normalized.as_str(),
        "dirty" | "blocked" | "behind"
    ) {
        blocking_reasons.push(format!(
            "mergeStateStatus={}",
            merge_state_status_normalized.to_ascii_uppercase()
        ));
    } else if matches!(
        merge_state_status_normalized.as_str(),
        "pending" | "unknown"
    ) {
        advisory_reasons.push(format!(
            "mergeStateStatus={}",
            merge_state_status_normalized.to_ascii_uppercase()
        ));
    }
    if ci.fail > 0 {
        blocking_reasons.push("CI has failing checks".to_string());
    } else if ci.pending > 0 {
        blocking_reasons.push("CI checks still pending".to_string());
    }
    if review_decision == "changes_requested" {
        blocking_reasons.push("review decision is CHANGES_REQUESTED".to_string());
    } else if review_decision == "review_required" {
        advisory_reasons.push("review approval still required".to_string());
    } else if review_decision == "unknown" {
        advisory_reasons.push("review decision unavailable".to_string());
    } else if review_decision == "none" {
        advisory_reasons.push("no explicit review decision".to_string());
    }
    let status = if !blocking_reasons.is_empty() {
        "blocked"
    } else if mergeable_normalized == "unknown" {
        "indeterminate"
    } else {
        "ready"
    };

    GhMergeReport {
        mergeable: mergeable_normalized,
        merge_state_status: merge_state_status_normalized,
        status: status.to_string(),
        blocking_reasons,
        advisory_reasons,
    }
}

pub fn summarize_ci_rollup(entries: &[serde_json::Value]) -> GhCiRollup {
    let mut total = 0_u64;
    let mut pass = 0_u64;
    let mut fail = 0_u64;
    let mut pending = 0_u64;
    let mut skip = 0_u64;
    let mut neutral = 0_u64;

    for entry in entries {
        if let Some(outcome) = classify_check_outcome(entry) {
            total += 1;
            match outcome {
                GhCheckOutcome::Pass => pass += 1,
                GhCheckOutcome::Fail => fail += 1,
                GhCheckOutcome::Pending => pending += 1,
                GhCheckOutcome::Skip => skip += 1,
                GhCheckOutcome::Neutral => neutral += 1,
            }
        }
    }
    let state = if total == 0 {
        "none"
    } else if fail > 0 {
        "fail"
    } else if pending > 0 {
        "pending"
    } else if pass + skip + neutral == total {
        "pass"
    } else {
        "mixed"
    };
    GhCiRollup {
        state: state.to_string(),
        total,
        pass,
        fail,
        pending,
        skip,
        neutral,
    }
}

pub fn normalize_review_status(value: Option<&str>) -> String {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(raw) => match raw.to_ascii_uppercase().as_str() {
            "APPROVED" => "approved".to_string(),
            "CHANGES_REQUESTED" => "changes_requested".to_string(),
            "REVIEW_REQUIRED" => "review_required".to_string(),
            _ => raw.to_ascii_lowercase(),
        },
        None => "unknown".to_string(),
    }
}

pub fn normalize_report_review_decision(
    value: Option<&str>,
    reviews: &[GhMonitorReviewReport],
) -> String {
    let decision = normalize_review_status(value);
    if decision == "unknown" && reviews.is_empty() {
        "none".to_string()
    } else {
        decision
    }
}

pub fn normalize_merge_status(value: Option<&str>) -> String {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(raw) if raw.eq_ignore_ascii_case("unknown") => "pending".to_string(),
        Some(raw) => raw.to_ascii_lowercase(),
        None => "unknown".to_string(),
    }
}

fn extract_check_name(entry: &serde_json::Value) -> String {
    extract_string_field(entry, &["name", "context"])
        .or_else(|| extract_string_field(entry, &["displayTitle"]))
        .unwrap_or_else(|| "unknown-check".to_string())
}

fn extract_check_status(entry: &serde_json::Value) -> String {
    extract_string_field(entry, &["status", "state"])
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| {
            if extract_string_field(entry, &["conclusion"]).is_some() {
                "completed".to_string()
            } else {
                "unknown".to_string()
            }
        })
}

fn extract_string_field(entry: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| entry.get(*key))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn normalize_mergeable(value: Option<&str>) -> String {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(raw) => match raw.to_ascii_uppercase().as_str() {
            "MERGEABLE" => "mergeable".to_string(),
            "CONFLICTING" => "conflicting".to_string(),
            "UNKNOWN" => "unknown".to_string(),
            _ => raw.to_ascii_lowercase(),
        },
        None => "unknown".to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GhCheckOutcome {
    Pass,
    Fail,
    Pending,
    Skip,
    Neutral,
}

fn classify_check_outcome(entry: &serde_json::Value) -> Option<GhCheckOutcome> {
    let conclusion = entry
        .get("conclusion")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    if let Some(conclusion) = conclusion.as_deref() {
        return Some(match conclusion {
            "success" => GhCheckOutcome::Pass,
            "failure" | "timed_out" | "startup_failure" | "action_required" => GhCheckOutcome::Fail,
            "skipped" => GhCheckOutcome::Skip,
            "cancelled" | "neutral" => GhCheckOutcome::Neutral,
            _ => GhCheckOutcome::Neutral,
        });
    }
    let status = entry
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            entry
                .get("state")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
        });
    status.as_deref().map(|status| match status {
        "success" => GhCheckOutcome::Pass,
        "failure" | "error" | "timed_out" | "startup_failure" | "action_required" => {
            GhCheckOutcome::Fail
        }
        "queued" | "in_progress" | "pending" | "requested" | "waiting" => GhCheckOutcome::Pending,
        "skipped" => GhCheckOutcome::Skip,
        "completed" => GhCheckOutcome::Neutral,
        "cancelled" | "neutral" => GhCheckOutcome::Neutral,
        _ => GhCheckOutcome::Neutral,
    })
}
