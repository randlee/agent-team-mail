//! Shared types for the CI Monitor plugin provider abstraction

use serde::{Deserialize, Serialize};

/// A CI workflow/pipeline run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiRun {
    /// Provider-specific ID
    pub id: u64,
    /// Run name/workflow name
    pub name: String,
    /// Current status
    pub status: CiRunStatus,
    /// Final conclusion (if completed)
    pub conclusion: Option<CiRunConclusion>,
    /// Branch this run is for
    pub head_branch: String,
    /// Commit SHA this run is for
    pub head_sha: String,
    /// Web URL to the run
    pub url: String,
    /// Creation timestamp (ISO 8601)
    pub created_at: String,
    /// Last update timestamp (ISO 8601)
    pub updated_at: String,
    /// Jobs in this run (optional, included when fetching run details)
    pub jobs: Option<Vec<CiJob>>,
}

/// A job within a CI run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiJob {
    /// Provider-specific job ID
    pub id: u64,
    /// Job name
    pub name: String,
    /// Current status
    pub status: CiRunStatus,
    /// Final conclusion (if completed)
    pub conclusion: Option<CiRunConclusion>,
    /// Job start timestamp (ISO 8601)
    pub started_at: Option<String>,
    /// Job completion timestamp (ISO 8601)
    pub completed_at: Option<String>,
    /// Steps in this job (optional)
    pub steps: Option<Vec<CiStep>>,
}

/// A step within a CI job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiStep {
    /// Step name
    pub name: String,
    /// Current status
    pub status: CiRunStatus,
    /// Final conclusion (if completed)
    pub conclusion: Option<CiRunConclusion>,
    /// Step number in the job
    pub number: u64,
}

/// CI run/job status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CiRunStatus {
    /// Run is queued
    Queued,
    /// Run is in progress
    InProgress,
    /// Run has completed
    Completed,
    /// Run is waiting
    Waiting,
    /// Run was requested
    Requested,
    /// Run is pending
    Pending,
}

/// CI run/job conclusion (final result)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CiRunConclusion {
    /// Run succeeded
    Success,
    /// Run failed
    Failure,
    /// Run was cancelled
    Cancelled,
    /// Run was skipped
    Skipped,
    /// Run timed out
    TimedOut,
    /// Action required
    ActionRequired,
    /// Neutral conclusion
    Neutral,
    /// Run is stale
    Stale,
}

/// Filter for querying CI runs
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CiFilter {
    /// Filter by branch
    pub branch: Option<String>,
    /// Filter by status
    pub status: Option<CiRunStatus>,
    /// Filter by conclusion
    pub conclusion: Option<CiRunConclusion>,
    /// Results per page
    pub per_page: Option<u32>,
    /// Page number
    pub page: Option<u32>,
    /// Filter by event type (e.g., "push", "pull_request")
    pub event: Option<String>,
    /// Only runs created after this timestamp (ISO 8601)
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
