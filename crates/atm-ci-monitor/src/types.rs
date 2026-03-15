//! Shared CI-monitor core types.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug)]
pub enum CiProviderError {
    Provider { message: String },
    Runtime { message: String },
}

impl CiProviderError {
    pub fn provider(message: impl Into<String>) -> Self {
        Self::Provider {
            message: message.into(),
        }
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::Runtime {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Provider { message } | Self::Runtime { message } => message,
        }
    }
}

impl fmt::Display for CiProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { message } | Self::Runtime { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for CiProviderError {}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiMonitorTargetKind {
    Pr,
    Workflow,
    Run,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiMonitorRequest {
    pub team: String,
    pub target_kind: CiMonitorTargetKind,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_cwd: Option<String>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiMonitorStatusRequest {
    pub team: String,
    pub target_kind: CiMonitorTargetKind,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_cwd: Option<String>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiMonitorLifecycleAction {
    Start,
    Stop,
    Restart,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiMonitorControlRequest {
    pub team: String,
    pub action: CiMonitorLifecycleAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_team: Option<String>,
    #[serde(default)]
    pub user_authorized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_reason: Option<String>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiMonitorHealth {
    pub team: String,
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    pub lifecycle_state: String,
    pub availability_state: String,
    pub in_flight: u64,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_state_updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_limit_per_hour: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_used_in_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_remaining: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_reset_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_runtime_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_binary_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_atm_home: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_poll_interval_secs: Option<u64>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiMonitorStatus {
    pub team: String,
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    pub target_kind: CiMonitorTargetKind,
    pub target: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_state_updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GhRepoStateFile {
    #[serde(default)]
    pub records: Vec<GhRepoStateRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhRepoStateRecord {
    pub team: String,
    pub repo: String,
    pub updated_at: String,
    pub cache_expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh_at: Option<String>,
    pub budget_limit_per_hour: u64,
    pub budget_used_in_window: u64,
    pub budget_window_started_at: String,
    pub budget_warning_threshold: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning_emitted_at: Option<String>,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub in_flight: u64,
    pub idle_poll_interval_secs: u64,
    pub active_poll_interval_secs: u64,
    #[serde(default)]
    pub branch_ref_counts: Vec<GhBranchRefCount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_call: Option<GhObservedCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<GhRateLimitSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<GhRuntimeOwner>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhBranchRefCount {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhObservedCall {
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub duration_ms: u64,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhRateLimitSnapshot {
    pub remaining: u64,
    pub limit: u64,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhRuntimeOwner {
    pub runtime: String,
    pub executable_path: String,
    pub home_scope: String,
    pub pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiRun {
    pub id: u64,
    pub name: String,
    pub status: CiRunStatus,
    pub conclusion: Option<CiRunConclusion>,
    pub head_branch: String,
    pub head_sha: String,
    pub url: String,
    pub created_at: String,
    pub updated_at: String,
    pub attempt: Option<u64>,
    pub pull_requests: Option<Vec<CiPullRequest>>,
    pub jobs: Option<Vec<CiJob>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiJob {
    pub id: u64,
    pub name: String,
    pub status: CiRunStatus,
    pub conclusion: Option<CiRunConclusion>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub url: Option<String>,
    pub steps: Option<Vec<CiStep>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiStep {
    pub name: String,
    pub status: CiRunStatus,
    pub conclusion: Option<CiRunConclusion>,
    pub number: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CiPullRequest {
    pub number: u64,
    pub url: Option<String>,
    pub head_ref_name: Option<String>,
    pub head_ref_oid: Option<String>,
    pub created_at: Option<String>,
    pub merge_state_status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CiRunStatus {
    Queued,
    InProgress,
    Completed,
    Waiting,
    Requested,
    Pending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CiRunConclusion {
    Success,
    Failure,
    Cancelled,
    Skipped,
    TimedOut,
    ActionRequired,
    Neutral,
    Stale,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CiFilter {
    pub branch: Option<String>,
    pub status: Option<CiRunStatus>,
    pub conclusion: Option<CiRunConclusion>,
    pub per_page: Option<u32>,
    pub page: Option<u32>,
    pub event: Option<String>,
    pub created: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ci_run_serialization() {
        let run = CiRun {
            id: 123456789,
            name: "CI".to_string(),
            status: CiRunStatus::Completed,
            conclusion: Some(CiRunConclusion::Success),
            head_branch: "main".to_string(),
            head_sha: "abc123".to_string(),
            url: "https://github.com/owner/repo/actions/runs/123456789".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:05:00Z".to_string(),
            attempt: Some(1),
            pull_requests: None,
            jobs: None,
        };

        let json = serde_json::to_string(&run).unwrap();
        let deserialized: CiRun = serde_json::from_str(&json).unwrap();

        assert_eq!(run.id, deserialized.id);
        assert_eq!(run.name, deserialized.name);
        assert_eq!(run.status, deserialized.status);
        assert_eq!(run.conclusion, deserialized.conclusion);
    }

    #[test]
    fn test_ci_job_serialization() {
        let job = CiJob {
            id: 987654321,
            name: "build".to_string(),
            status: CiRunStatus::Completed,
            conclusion: Some(CiRunConclusion::Success),
            started_at: Some("2026-01-01T00:01:00Z".to_string()),
            completed_at: Some("2026-01-01T00:04:00Z".to_string()),
            url: Some("https://github.com/owner/repo/actions/jobs/987654321".to_string()),
            steps: None,
        };

        let json = serde_json::to_string(&job).unwrap();
        let deserialized: CiJob = serde_json::from_str(&json).unwrap();

        assert_eq!(job.id, deserialized.id);
        assert_eq!(job.name, deserialized.name);
    }

    #[test]
    fn test_ci_status_serialization() {
        let queued = CiRunStatus::Queued;
        let in_progress = CiRunStatus::InProgress;
        let completed = CiRunStatus::Completed;

        let queued_json = serde_json::to_string(&queued).unwrap();
        let in_progress_json = serde_json::to_string(&in_progress).unwrap();
        let completed_json = serde_json::to_string(&completed).unwrap();

        assert_eq!(queued_json, r#""Queued""#);
        assert_eq!(in_progress_json, r#""InProgress""#);
        assert_eq!(completed_json, r#""Completed""#);

        let queued_de: CiRunStatus = serde_json::from_str(&queued_json).unwrap();
        assert_eq!(queued_de, CiRunStatus::Queued);
    }

    #[test]
    fn test_ci_conclusion_serialization() {
        let success = CiRunConclusion::Success;
        let failure = CiRunConclusion::Failure;

        let success_json = serde_json::to_string(&success).unwrap();
        let failure_json = serde_json::to_string(&failure).unwrap();

        assert_eq!(success_json, r#""Success""#);
        assert_eq!(failure_json, r#""Failure""#);

        let success_de: CiRunConclusion = serde_json::from_str(&success_json).unwrap();
        let failure_de: CiRunConclusion = serde_json::from_str(&failure_json).unwrap();

        assert_eq!(success_de, CiRunConclusion::Success);
        assert_eq!(failure_de, CiRunConclusion::Failure);
    }

    #[test]
    fn test_ci_filter_default() {
        let filter = CiFilter::default();
        assert!(filter.branch.is_none());
        assert!(filter.status.is_none());
        assert!(filter.conclusion.is_none());
        assert!(filter.per_page.is_none());
        assert!(filter.page.is_none());
        assert!(filter.event.is_none());
        assert!(filter.created.is_none());
    }

    #[test]
    fn test_ci_filter_with_values() {
        let filter = CiFilter {
            branch: Some("main".to_string()),
            status: Some(CiRunStatus::Completed),
            conclusion: Some(CiRunConclusion::Success),
            per_page: Some(50),
            page: Some(1),
            event: Some("push".to_string()),
            created: Some("2026-01-01T00:00:00Z".to_string()),
        };

        assert_eq!(filter.branch, Some("main".to_string()));
        assert_eq!(filter.status, Some(CiRunStatus::Completed));
        assert_eq!(filter.conclusion, Some(CiRunConclusion::Success));
        assert_eq!(filter.per_page, Some(50));
        assert_eq!(filter.page, Some(1));
    }
}
