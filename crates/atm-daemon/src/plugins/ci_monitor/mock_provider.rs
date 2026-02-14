//! Mock CI provider for testing

use super::provider::CiProvider;
use super::types::{CiFilter, CiJob, CiRun, CiRunConclusion, CiRunStatus, CiStep};
use crate::plugin::PluginError;
use std::sync::{Arc, Mutex};

/// Mock CI provider for testing. Returns canned data.
#[derive(Debug, Clone)]
pub struct MockCiProvider {
    /// Runs to return from list_runs/get_run
    pub runs: Vec<CiRun>,
    /// Jobs to return (added to runs when fetching full details)
    pub jobs: Vec<CiJob>,
    /// If set, all methods return this error
    pub error: Option<String>,
    /// Track calls for verification
    pub call_log: Arc<Mutex<Vec<MockCall>>>,
}

/// Record of method calls for test assertions
#[derive(Debug, Clone, PartialEq)]
pub enum MockCall {
    ListRuns(CiFilter),
    GetRun(u64),
    GetJobLog(u64),
}

impl MockCiProvider {
    /// Create a new mock provider with empty data
    pub fn new() -> Self {
        Self {
            runs: Vec::new(),
            jobs: Vec::new(),
            error: None,
            call_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Create a mock provider with specific runs
    pub fn with_runs(runs: Vec<CiRun>) -> Self {
        Self {
            runs,
            jobs: Vec::new(),
            error: None,
            call_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Create a mock provider with runs and jobs
    pub fn with_runs_and_jobs(runs: Vec<CiRun>, jobs: Vec<CiJob>) -> Self {
        Self {
            runs,
            jobs,
            error: None,
            call_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Set the error that all methods should return
    pub fn with_error(mut self, error: String) -> Self {
        self.error = Some(error);
        self
    }

    /// Get a copy of the call log for assertions
    pub fn get_calls(&self) -> Vec<MockCall> {
        self.call_log.lock().unwrap().clone()
    }

    /// Clear the call log
    pub fn clear_calls(&self) {
        self.call_log.lock().unwrap().clear();
    }

    /// Helper to log a call
    fn log_call(&self, call: MockCall) {
        self.call_log.lock().unwrap().push(call);
    }
}

impl Default for MockCiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CiProvider for MockCiProvider {
    async fn list_runs(&self, filter: &CiFilter) -> Result<Vec<CiRun>, PluginError> {
        self.log_call(MockCall::ListRuns(filter.clone()));

        if let Some(err) = &self.error {
            return Err(PluginError::Provider {
                message: err.clone(),
                source: None,
            });
        }

        // Apply filters to runs
        let mut filtered = self.runs.clone();

        // Filter by branch
        if let Some(branch) = &filter.branch {
            filtered.retain(|run| &run.head_branch == branch);
        }

        // Filter by status
        if let Some(status) = filter.status {
            filtered.retain(|run| run.status == status);
        }

        // Filter by conclusion
        if let Some(conclusion) = filter.conclusion {
            filtered.retain(|run| run.conclusion == Some(conclusion));
        }

        // Apply pagination
        if let Some(per_page) = filter.per_page {
            let page = filter.page.unwrap_or(1);
            let start = ((page - 1) * per_page) as usize;
            let end = (start + per_page as usize).min(filtered.len());
            filtered = filtered[start..end].to_vec();
        }

        Ok(filtered)
    }

    async fn get_run(&self, run_id: u64) -> Result<CiRun, PluginError> {
        self.log_call(MockCall::GetRun(run_id));

        if let Some(err) = &self.error {
            return Err(PluginError::Provider {
                message: err.clone(),
                source: None,
            });
        }

        let mut run = self
            .runs
            .iter()
            .find(|run| run.id == run_id)
            .cloned()
            .ok_or_else(|| PluginError::Provider {
                message: format!("Run #{run_id} not found"),
                source: None,
            })?;

        // Add jobs to the run if available
        if !self.jobs.is_empty() {
            run.jobs = Some(self.jobs.clone());
        }

        Ok(run)
    }

    async fn get_job_log(&self, job_id: u64) -> Result<String, PluginError> {
        self.log_call(MockCall::GetJobLog(job_id));

        if let Some(err) = &self.error {
            return Err(PluginError::Provider {
                message: err.clone(),
                source: None,
            });
        }

        Ok(format!("Mock log output for job {job_id}"))
    }

    fn provider_name(&self) -> &str {
        "MockCiProvider"
    }
}

/// Helper function to create a test CI run
pub fn create_test_run(
    id: u64,
    name: &str,
    branch: &str,
    status: CiRunStatus,
    conclusion: Option<CiRunConclusion>,
) -> CiRun {
    CiRun {
        id,
        name: name.to_string(),
        status,
        conclusion,
        head_branch: branch.to_string(),
        head_sha: format!("sha{id}"),
        url: format!("https://github.com/test/repo/actions/runs/{id}"),
        created_at: "2026-02-13T10:00:00Z".to_string(),
        updated_at: "2026-02-13T10:05:00Z".to_string(),
        jobs: None,
    }
}

/// Helper function to create a test CI job
pub fn create_test_job(
    id: u64,
    name: &str,
    status: CiRunStatus,
    conclusion: Option<CiRunConclusion>,
) -> CiJob {
    CiJob {
        id,
        name: name.to_string(),
        status,
        conclusion,
        started_at: Some("2026-02-13T10:01:00Z".to_string()),
        completed_at: Some("2026-02-13T10:04:00Z".to_string()),
        steps: None,
    }
}

/// Helper function to create a test CI step
pub fn create_test_step(
    name: &str,
    number: u64,
    status: CiRunStatus,
    conclusion: Option<CiRunConclusion>,
) -> CiStep {
    CiStep {
        name: name.to_string(),
        status,
        conclusion,
        number,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_provider_new() {
        let provider = MockCiProvider::new();
        assert!(provider.runs.is_empty());
        assert!(provider.jobs.is_empty());
        assert!(provider.error.is_none());
        assert!(provider.get_calls().is_empty());
    }

    #[tokio::test]
    async fn test_mock_provider_with_runs() {
        let runs = vec![create_test_run(
            1,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Success),
        )];

        let provider = MockCiProvider::with_runs(runs.clone());
        assert_eq!(provider.runs.len(), 1);
        assert_eq!(provider.runs[0].id, 1);
    }

    #[tokio::test]
    async fn test_mock_provider_list_runs_logs_call() {
        let provider = MockCiProvider::new();
        let filter = CiFilter::default();

        let _ = provider.list_runs(&filter).await;

        let calls = provider.get_calls();
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0], MockCall::ListRuns(_)));
    }

    #[tokio::test]
    async fn test_mock_provider_error() {
        let provider = MockCiProvider::new().with_error("Test error".to_string());

        let result = provider.list_runs(&CiFilter::default()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Test error"));
    }

    #[tokio::test]
    async fn test_mock_provider_filter_by_branch() {
        let runs = vec![
            create_test_run(
                1,
                "CI",
                "main",
                CiRunStatus::Completed,
                Some(CiRunConclusion::Success),
            ),
            create_test_run(
                2,
                "CI",
                "develop",
                CiRunStatus::Completed,
                Some(CiRunConclusion::Success),
            ),
        ];

        let provider = MockCiProvider::with_runs(runs);
        let filter = CiFilter {
            branch: Some("main".to_string()),
            ..Default::default()
        };

        let result = provider.list_runs(&filter).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].head_branch, "main");
    }

    #[tokio::test]
    async fn test_mock_provider_filter_by_conclusion() {
        let runs = vec![
            create_test_run(
                1,
                "CI",
                "main",
                CiRunStatus::Completed,
                Some(CiRunConclusion::Success),
            ),
            create_test_run(
                2,
                "CI",
                "main",
                CiRunStatus::Completed,
                Some(CiRunConclusion::Failure),
            ),
        ];

        let provider = MockCiProvider::with_runs(runs);
        let filter = CiFilter {
            conclusion: Some(CiRunConclusion::Failure),
            ..Default::default()
        };

        let result = provider.list_runs(&filter).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].conclusion, Some(CiRunConclusion::Failure));
    }

    #[tokio::test]
    async fn test_mock_provider_get_run() {
        let runs = vec![create_test_run(
            42,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Success),
        )];

        let provider = MockCiProvider::with_runs(runs);
        let run = provider.get_run(42).await.unwrap();
        assert_eq!(run.id, 42);
        assert_eq!(run.name, "CI");

        let calls = provider.get_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], MockCall::GetRun(42));
    }

    #[tokio::test]
    async fn test_mock_provider_get_run_not_found() {
        let provider = MockCiProvider::new();
        let result = provider.get_run(999).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_mock_provider_get_run_with_jobs() {
        let runs = vec![create_test_run(
            1,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Success),
        )];
        let jobs = vec![create_test_job(
            101,
            "build",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Success),
        )];

        let provider = MockCiProvider::with_runs_and_jobs(runs, jobs.clone());
        let run = provider.get_run(1).await.unwrap();

        assert!(run.jobs.is_some());
        let run_jobs = run.jobs.unwrap();
        assert_eq!(run_jobs.len(), 1);
        assert_eq!(run_jobs[0].name, "build");
    }

    #[tokio::test]
    async fn test_mock_provider_get_job_log() {
        let provider = MockCiProvider::new();
        let log = provider.get_job_log(123).await.unwrap();

        assert!(log.contains("123"));
        assert!(log.contains("Mock log output"));

        let calls = provider.get_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], MockCall::GetJobLog(123));
    }

    #[tokio::test]
    async fn test_mock_provider_clear_calls() {
        let provider = MockCiProvider::new();
        let _ = provider.list_runs(&CiFilter::default()).await;
        assert_eq!(provider.get_calls().len(), 1);

        provider.clear_calls();
        assert!(provider.get_calls().is_empty());
    }

    #[test]
    fn test_create_test_run_helper() {
        let run = create_test_run(
            123,
            "Test Run",
            "feature-branch",
            CiRunStatus::InProgress,
            None,
        );

        assert_eq!(run.id, 123);
        assert_eq!(run.name, "Test Run");
        assert_eq!(run.head_branch, "feature-branch");
        assert_eq!(run.status, CiRunStatus::InProgress);
        assert_eq!(run.conclusion, None);
    }

    #[test]
    fn test_create_test_job_helper() {
        let job = create_test_job(
            456,
            "test-job",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );

        assert_eq!(job.id, 456);
        assert_eq!(job.name, "test-job");
        assert_eq!(job.status, CiRunStatus::Completed);
        assert_eq!(job.conclusion, Some(CiRunConclusion::Failure));
    }

    #[test]
    fn test_create_test_step_helper() {
        let step = create_test_step("Setup", 1, CiRunStatus::Completed, Some(CiRunConclusion::Success));

        assert_eq!(step.name, "Setup");
        assert_eq!(step.number, 1);
        assert_eq!(step.status, CiRunStatus::Completed);
        assert_eq!(step.conclusion, Some(CiRunConclusion::Success));
    }
}
