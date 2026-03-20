//! Neutral GH namespace contracts shared by the CLI and daemon/plugin layers.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
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

#[derive(Debug, Deserialize)]
struct GhPrMergeProbe {
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(rename = "mergeStateStatus", default)]
    merge_state_status: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct LocalGhRepoStateFile {
    #[serde(default)]
    records: Vec<LocalGhRepoStateRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LocalGhRepoStateRecord {
    team: String,
    repo: String,
    updated_at: String,
    cache_expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_refresh_at: Option<String>,
    budget_limit_per_hour: u64,
    budget_used_in_window: u64,
    budget_window_started_at: String,
    budget_warning_threshold: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    warning_emitted_at: Option<String>,
    #[serde(default)]
    blocked: bool,
    #[serde(default)]
    in_flight: u64,
    idle_poll_interval_secs: u64,
    active_poll_interval_secs: u64,
    #[serde(default)]
    branch_ref_counts: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_call: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rate_limit: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<serde_json::Value>,
}

const GH_MONITOR_MERGE_RETRY_ATTEMPTS: u8 = 3;
const GH_MONITOR_MERGE_RETRY_DELAY_MS: u64 = 250;
const GH_OBSERVABILITY_LEDGER_FILE: &str = ".atm/daemon/gh-observability.jsonl";

pub fn build_pr_list_summary(
    team: &str,
    home_dir: &std::path::Path,
    repo: &str,
    limit: u32,
) -> anyhow::Result<GhPrListSummary> {
    let request_id = new_local_gh_id("gh-info");
    emit_local_gh_ledger_record(
        home_dir,
        "gh_info_requested",
        serde_json::json!({
            "kind": "freshness",
            "request_id": request_id,
            "team": team,
            "repo": repo,
            "caller": "gh_pr_list",
            "info_type": "gh_pr_list",
        }),
    );
    let request_limit = limit.clamp(1, 200);
    let gh_json_fields =
        "number,title,url,isDraft,reviewDecision,mergeStateStatus,statusCheckRollup";
    let output = run_repo_scoped_gh_command(
        team,
        home_dir,
        repo,
        "gh_pr_list",
        &[
            "-R",
            repo,
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            &request_limit.to_string(),
            "--json",
            gh_json_fields,
        ],
    )
    .map_err(|err| anyhow::anyhow!("failed to invoke `gh pr list` for repository {repo}: {err}"))?;
    emit_local_gh_ledger_record(
        home_dir,
        "gh_info_live_refresh",
        serde_json::json!({
            "kind": "freshness",
            "request_id": request_id,
            "team": team,
            "repo": repo,
            "caller": "gh_pr_list",
            "info_type": "gh_pr_list",
            "cache_hit": false,
            "result": "live_refresh",
        }),
    );

    let rows: Vec<GhPrListRow> = serde_json::from_str(&output)
        .map_err(|err| anyhow::anyhow!("failed to parse `gh pr list` JSON output: {err}"))?;

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
    home_dir: &std::path::Path,
    repo: &str,
    pr_number: u64,
) -> anyhow::Result<GhPrReportSummary> {
    let request_id = new_local_gh_id("gh-info");
    emit_local_gh_ledger_record(
        home_dir,
        "gh_info_requested",
        serde_json::json!({
            "kind": "freshness",
            "request_id": request_id,
            "team": team,
            "repo": repo,
            "caller": "gh_pr_report",
            "info_type": "gh_pr_report",
        }),
    );
    let pr_number_arg = pr_number.to_string();
    let gh_json_fields = "number,title,url,isDraft,reviewDecision,mergeStateStatus,mergeable,statusCheckRollup,reviews";
    let output = run_repo_scoped_gh_command(
        team,
        home_dir,
        repo,
        "gh_pr_view",
        &[
            "-R",
            repo,
            "pr",
            "view",
            &pr_number_arg,
            "--json",
            gh_json_fields,
        ],
    )
    .map_err(|err| anyhow::anyhow!("failed to invoke `gh pr view` for repository {repo}: {err}"))?;
    emit_local_gh_ledger_record(
        home_dir,
        "gh_info_live_refresh",
        serde_json::json!({
            "kind": "freshness",
            "request_id": request_id,
            "team": team,
            "repo": repo,
            "caller": "gh_pr_report",
            "info_type": "gh_pr_report",
            "cache_hit": false,
            "result": "live_refresh",
        }),
    );

    let row: GhPrReportRow = serde_json::from_str(&output)
        .map_err(|err| anyhow::anyhow!("failed to parse `gh pr view` JSON output: {err}"))?;
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

pub fn validate_gh_cli_prerequisites() -> anyhow::Result<()> {
    let version = std::process::Command::new("gh")
        .arg("--version")
        .output()
        .map_err(|err| anyhow::anyhow!("failed to invoke `gh --version`: {err}"))?;
    if !version.status.success() {
        anyhow::bail!(
            "GitHub CLI (`gh`) not found or not executable. Install from https://cli.github.com/"
        );
    }

    let auth = std::process::Command::new("gh")
        .args(["auth", "status"])
        .output()
        .map_err(|err| anyhow::anyhow!("failed to invoke `gh auth status`: {err}"))?;
    if !auth.status.success() {
        let stderr = String::from_utf8_lossy(&auth.stderr);
        anyhow::bail!(
            "GitHub CLI is not authenticated. Run `gh auth login` first.\n{}",
            stderr.trim()
        );
    }

    Ok(())
}

fn resolve_merge_snapshot_with_retry(
    team: &str,
    home_dir: &std::path::Path,
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
        std::thread::sleep(std::time::Duration::from_millis(
            GH_MONITOR_MERGE_RETRY_DELAY_MS,
        ));
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
    home_dir: &std::path::Path,
    repo: &str,
    pr_number: u64,
) -> anyhow::Result<GhPrMergeProbe> {
    let pr_number_arg = pr_number.to_string();
    let output = run_repo_scoped_gh_command(
        team,
        home_dir,
        repo,
        "gh_pr_view_merge_probe",
        &[
            "-R",
            repo,
            "pr",
            "view",
            &pr_number_arg,
            "--json",
            "mergeStateStatus,mergeable",
        ],
    )
    .map_err(|err| {
        anyhow::anyhow!("failed to invoke `gh pr view` merge probe for {repo}: {err}")
    })?;

    serde_json::from_str(&output)
        .map_err(|err| anyhow::anyhow!("failed to parse merge probe JSON output: {err}"))
}

fn run_repo_scoped_gh_command(
    team: &str,
    home_dir: &std::path::Path,
    repo: &str,
    action: &str,
    args: &[&str],
) -> anyhow::Result<String> {
    let call_id = new_local_gh_id("gh-call");
    emit_local_gh_ledger_record(
        home_dir,
        "gh_call_started",
        serde_json::json!({
            "kind": "execution",
            "call_id": call_id,
            "team": team,
            "repo": repo,
            "argv": args,
            "caller": action,
        }),
    );
    let output = std::process::Command::new("gh")
        .args(args)
        .output()
        .map_err(|err| anyhow::anyhow!("failed to invoke gh for {action}: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        anyhow::bail!(
            "gh command failed for {action}: {}",
            if detail.is_empty() {
                "unknown gh error"
            } else {
                detail
            }
        );
    }
    emit_local_gh_ledger_record(
        home_dir,
        "gh_call_finished",
        serde_json::json!({
            "kind": "execution",
            "call_id": call_id,
            "team": team,
            "repo": repo,
            "argv": args,
            "caller": action,
            "result": "success",
        }),
    );
    record_local_repo_state(home_dir, team, repo, action)?;
    String::from_utf8(output.stdout)
        .map_err(|err| anyhow::anyhow!("gh output was not valid UTF-8: {err}"))
}

fn emit_local_gh_ledger_record(
    home_dir: &std::path::Path,
    action: &str,
    mut record: serde_json::Value,
) {
    if let serde_json::Value::Object(fields) = &mut record {
        fields.insert(
            "action".to_string(),
            serde_json::Value::String(action.to_string()),
        );
        fields.insert(
            "at".to_string(),
            serde_json::Value::String(
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ),
        );
    }
    let path = home_dir.join(GH_OBSERVABILITY_LEDGER_FILE);
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let Ok(line) = serde_json::to_string(&record) else {
        return;
    };
    let _ = writeln!(file, "{line}");
}

fn new_local_gh_id(prefix: &str) -> String {
    format!("{prefix}-{}", rand::random::<u64>())
}

fn record_local_repo_state(
    home_dir: &std::path::Path,
    team: &str,
    repo: &str,
    action: &str,
) -> anyhow::Result<()> {
    let path = home_dir.join(".atm/daemon/gh-monitor-repo-state.json");
    let mut state = if path.exists() {
        serde_json::from_slice::<LocalGhRepoStateFile>(&std::fs::read(&path)?).unwrap_or_default()
    } else {
        LocalGhRepoStateFile::default()
    };
    let now = chrono::Utc::now();
    let now_rfc3339 = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let expires =
        (now + chrono::Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let key = format!("{}|{}", team.trim(), repo.trim().to_ascii_lowercase());

    if let Some(record) = state.records.iter_mut().find(|record| {
        format!(
            "{}|{}",
            record.team.trim(),
            record.repo.trim().to_ascii_lowercase()
        ) == key
    }) {
        record.updated_at = now_rfc3339.clone();
        record.cache_expires_at = expires.clone();
        record.last_refresh_at = Some(now_rfc3339.clone());
        record.budget_used_in_window = record.budget_used_in_window.saturating_add(1);
        record.last_call = Some(serde_json::json!({
            "action": action,
            "duration_ms": 0,
            "success": true,
            "at": now_rfc3339,
        }));
    } else {
        state.records.push(LocalGhRepoStateRecord {
            team: team.to_string(),
            repo: repo.to_string(),
            updated_at: now_rfc3339.clone(),
            cache_expires_at: expires,
            last_refresh_at: Some(now_rfc3339.clone()),
            budget_limit_per_hour: 100,
            budget_used_in_window: 1,
            budget_window_started_at: now_rfc3339.clone(),
            budget_warning_threshold: 80,
            warning_emitted_at: None,
            blocked: false,
            in_flight: 0,
            idle_poll_interval_secs: 60,
            active_poll_interval_secs: 15,
            branch_ref_counts: Vec::new(),
            last_call: Some(serde_json::json!({
                "action": action,
                "duration_ms": 0,
                "success": true,
                "at": now_rfc3339,
            })),
            rate_limit: None,
            owner: None,
        });
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&state)?)?;
    std::fs::rename(tmp, path)?;
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
