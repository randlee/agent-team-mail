//! GitHub command routing helpers owned by the CI-monitor plugin layer.

use super::run_attributed_gh_command_with_ids;
use agent_team_mail_ci_monitor::{
    GhCliObserverContext, emit_gh_info_denied, emit_gh_info_live_refresh, emit_gh_info_requested,
    new_gh_execution_call_id, new_gh_info_request_id,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct GhPrListRow {
    number: u64,
    title: String,
    url: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "reviewDecision", default)]
    review_decision: Option<String>,
    #[serde(rename = "mergeStateStatus", default)]
    merge_state_status: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct GhPrReportRow {
    number: u64,
    title: String,
    url: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "reviewDecision", default)]
    review_decision: Option<String>,
    #[serde(rename = "mergeStateStatus", default)]
    merge_state_status: Option<String>,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Vec<serde_json::Value>,
    #[serde(default)]
    reviews: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GhPrListSummary {
    pub team: String,
    pub repo: String,
    pub generated_at: String,
    pub total_open_prs: usize,
    pub items: Vec<GhMonitorListItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GhMonitorListItem {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub draft: bool,
    pub ci: GhCiRollup,
    pub merge: String,
    pub review: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GhCiRollup {
    pub state: String,
    pub total: u64,
    pub pass: u64,
    pub fail: u64,
    pub pending: u64,
    pub skip: u64,
    pub neutral: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GhPrReportSummary {
    pub schema_version: String,
    pub team: String,
    pub repo: String,
    pub generated_at: String,
    pub pr: GhMonitorReportPr,
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct GhMergeReport {
    pub mergeable: String,
    pub merge_state_status: String,
    pub status: String,
    pub blocking_reasons: Vec<String>,
    pub advisory_reasons: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GhPrMergeProbe {
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(rename = "mergeStateStatus", default)]
    merge_state_status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct GhMonitorReviewReport {
    pub reviewer: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<String>,
}

pub const GH_MONITOR_REPORT_SCHEMA_VERSION: &str = "1.0.0";
const GH_MONITOR_MERGE_RETRY_ATTEMPTS: u8 = 3;
const GH_MONITOR_MERGE_RETRY_DELAY_MS: u64 = 250;

pub fn build_pr_list_summary(
    team: &str,
    home_dir: &Path,
    repo: &str,
    limit: u32,
) -> Result<GhPrListSummary> {
    let request_limit = limit.clamp(1, 200);
    let gh_json_fields =
        "number,title,url,isDraft,reviewDecision,mergeStateStatus,statusCheckRollup";
    let limit_arg = request_limit.to_string();
    let args = vec![
        "-R".to_string(),
        repo.to_string(),
        "pr".to_string(),
        "list".to_string(),
        "--state".to_string(),
        "open".to_string(),
        "--limit".to_string(),
        limit_arg,
        "--json".to_string(),
        gh_json_fields.to_string(),
    ];
    let output = run_repo_scoped_gh_command(team, home_dir, repo, "gh_pr_list", &args, None)
        .with_context(|| format!("failed to invoke `gh pr list` for repository {repo}"))?;

    let rows: Vec<GhPrListRow> = serde_json::from_str(&output)
        .with_context(|| "failed to parse `gh pr list` JSON output")?;

    let mut items: Vec<GhMonitorListItem> = rows
        .iter()
        .map(|row| GhMonitorListItem {
            number: row.number,
            title: row.title.clone(),
            url: row.url.clone(),
            draft: row.is_draft,
            ci: summarize_ci_rollup(&row.status_check_rollup),
            merge: normalize_merge_status(row.merge_state_status.as_deref()),
            review: normalize_review_status(row.review_decision.as_deref()),
        })
        .collect();
    items.sort_by(|a, b| a.number.cmp(&b.number));

    Ok(GhPrListSummary {
        team: team.to_string(),
        repo: repo.to_string(),
        generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        total_open_prs: items.len(),
        items,
    })
}

pub fn build_pr_report_summary(
    team: &str,
    home_dir: &Path,
    repo: &str,
    pr_number: u64,
) -> Result<GhPrReportSummary> {
    let gh_json_fields = "number,title,url,isDraft,reviewDecision,mergeStateStatus,mergeable,statusCheckRollup,reviews";
    let pr_number_arg = pr_number.to_string();
    let args = vec![
        "-R".to_string(),
        repo.to_string(),
        "pr".to_string(),
        "view".to_string(),
        pr_number_arg.clone(),
        "--json".to_string(),
        gh_json_fields.to_string(),
    ];
    let output = run_repo_scoped_gh_command(
        team,
        home_dir,
        repo,
        "gh_pr_view",
        &args,
        Some(pr_number_arg.as_str()),
    )
    .with_context(|| format!("failed to invoke `gh pr view` for repository {repo}"))?;

    let row: GhPrReportRow = serde_json::from_str(&output)
        .with_context(|| "failed to parse `gh pr view` JSON output")?;
    let checks = extract_check_reports(&row.status_check_rollup);
    let reviews = extract_review_reports(&row.reviews);
    let ci = summarize_ci_rollup(&row.status_check_rollup);
    let review_decision =
        normalize_report_review_decision(row.review_decision.as_deref(), &reviews);
    let (mergeable, merge_state_status) = resolve_merge_snapshot_with_retry(
        team,
        home_dir,
        repo,
        pr_number,
        row.mergeable.clone(),
        row.merge_state_status.clone(),
    );
    let merge = build_merge_report(
        mergeable.as_deref(),
        merge_state_status.as_deref(),
        row.is_draft,
        &ci,
        &review_decision,
    );

    Ok(GhPrReportSummary {
        schema_version: GH_MONITOR_REPORT_SCHEMA_VERSION.to_string(),
        team: team.to_string(),
        repo: repo.to_string(),
        generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        pr: GhMonitorReportPr {
            number: row.number,
            title: row.title,
            url: row.url,
            draft: row.is_draft,
            ci,
            review_decision,
            merge,
            checks,
            reviews,
        },
    })
}

fn resolve_merge_snapshot_with_retry(
    team: &str,
    home_dir: &Path,
    repo: &str,
    pr_number: u64,
    initial_mergeable: Option<String>,
    initial_merge_state_status: Option<String>,
) -> (Option<String>, Option<String>) {
    let mut mergeable = initial_mergeable;
    let mut merge_state_status = initial_merge_state_status;

    if !should_retry_mergeability(mergeable.as_deref(), merge_state_status.as_deref()) {
        return (mergeable, merge_state_status);
    }

    for _ in 0..GH_MONITOR_MERGE_RETRY_ATTEMPTS {
        thread::sleep(Duration::from_millis(GH_MONITOR_MERGE_RETRY_DELAY_MS));
        let Ok(snapshot) = query_merge_snapshot(team, home_dir, repo, pr_number) else {
            break;
        };

        mergeable = snapshot.mergeable;
        merge_state_status = snapshot.merge_state_status;

        if !should_retry_mergeability(mergeable.as_deref(), merge_state_status.as_deref()) {
            break;
        }
    }

    (mergeable, merge_state_status)
}

fn should_retry_mergeability(mergeable: Option<&str>, merge_state_status: Option<&str>) -> bool {
    let mergeable_normalized = normalize_mergeable(mergeable);
    let merge_state_status_normalized = normalize_merge_status(merge_state_status);
    mergeable_normalized == "unknown"
        || matches!(
            merge_state_status_normalized.as_str(),
            "unknown" | "pending"
        )
}

fn query_merge_snapshot(
    team: &str,
    home_dir: &Path,
    repo: &str,
    pr_number: u64,
) -> Result<GhPrMergeProbe> {
    let pr_number_arg = pr_number.to_string();
    let args = vec![
        "-R".to_string(),
        repo.to_string(),
        "pr".to_string(),
        "view".to_string(),
        pr_number_arg.clone(),
        "--json".to_string(),
        "mergeStateStatus,mergeable".to_string(),
    ];
    let output = run_repo_scoped_gh_command(
        team,
        home_dir,
        repo,
        "gh_pr_view_merge_probe",
        &args,
        Some(pr_number_arg.as_str()),
    )
    .with_context(|| format!("failed to invoke `gh pr view` merge probe for {repo}"))?;

    serde_json::from_str(&output).with_context(|| "failed to parse merge probe JSON output")
}

fn run_repo_scoped_gh_command(
    team: &str,
    home_dir: &Path,
    repo: &str,
    action: &str,
    args: &[String],
    reference: Option<&str>,
) -> Result<String> {
    let observer_ctx = GhCliObserverContext::new(
        home_dir.to_path_buf(),
        team.to_string(),
        repo.to_string(),
        "atm".to_string(),
    );
    let request_id = new_gh_info_request_id();
    let call_id = new_gh_execution_call_id();
    emit_gh_info_requested(&observer_ctx, &request_id, action, None, reference);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match run_attributed_gh_command_with_ids(
        &observer_ctx,
        action,
        &arg_refs,
        None,
        reference,
        request_id.clone(),
        call_id.clone(),
    ) {
        Ok(output) => {
            emit_gh_info_live_refresh(
                &observer_ctx,
                &request_id,
                action,
                &call_id,
                None,
                reference,
            );
            Ok(output)
        }
        Err(err) => {
            emit_gh_info_denied(
                &observer_ctx,
                &request_id,
                action,
                &err.to_string(),
                None,
                reference,
            );
            Err(err)
        }
    }
}

pub fn validate_gh_cli_prerequisites() -> Result<()> {
    let version = Command::new("gh")
        .arg("--version")
        .output()
        .context("failed to invoke `gh --version`")?;
    if !version.status.success() {
        bail!(
            "GitHub CLI (`gh`) not found or not executable. Install from https://cli.github.com/"
        );
    }

    let auth = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .context("failed to invoke `gh auth status`")?;
    if !auth.status.success() {
        let stderr = String::from_utf8_lossy(&auth.stderr);
        bail!(
            "GitHub CLI is not authenticated. Run `gh auth login` first.\n{}",
            stderr.trim()
        );
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

pub fn normalize_merge_status(value: Option<&str>) -> String {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(raw) if raw.eq_ignore_ascii_case("unknown") => "pending".to_string(),
        Some(raw) => raw.to_ascii_lowercase(),
        None => "unknown".to_string(),
    }
}
