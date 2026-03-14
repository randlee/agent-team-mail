//! Mock CI provider for testing.

use crate::provider::CiProvider;
use crate::types::{
    CiFilter, CiJob, CiProviderError, CiPullRequest, CiRun, CiRunConclusion, CiRunStatus, CiStep,
};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct MockCiProvider {
    pub runs: Vec<CiRun>,
    pub jobs: Vec<CiJob>,
    pub error: Option<String>,
    pub pull_requests: Vec<CiPullRequest>,
    pub call_log: Arc<Mutex<Vec<MockCall>>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MockCall {
    ListRuns(CiFilter),
    GetRun(u64),
    GetJobLog(u64),
    GetPullRequest(u64),
}

impl MockCiProvider {
    pub fn new() -> Self {
        Self {
            runs: Vec::new(),
            jobs: Vec::new(),
            error: None,
            pull_requests: Vec::new(),
            call_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_runs(runs: Vec<CiRun>) -> Self {
        Self {
            runs,
            jobs: Vec::new(),
            error: None,
            pull_requests: Vec::new(),
            call_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_runs_and_jobs(runs: Vec<CiRun>, jobs: Vec<CiJob>) -> Self {
        Self {
            runs,
            jobs,
            error: None,
            pull_requests: Vec::new(),
            call_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_pull_requests(mut self, pull_requests: Vec<CiPullRequest>) -> Self {
        self.pull_requests = pull_requests;
        self
    }

    pub fn with_error(mut self, error: String) -> Self {
        self.error = Some(error);
        self
    }

    pub fn get_calls(&self) -> Vec<MockCall> {
        self.call_log.lock().unwrap().clone()
    }

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
    async fn list_runs(&self, filter: &CiFilter) -> Result<Vec<CiRun>, CiProviderError> {
        self.log_call(MockCall::ListRuns(filter.clone()));

        if let Some(err) = &self.error {
            return Err(CiProviderError::provider(err.clone()));
        }

        let mut filtered = self.runs.clone();
        if let Some(branch) = &filter.branch {
            filtered.retain(|run| &run.head_branch == branch);
        }
        if let Some(status) = filter.status {
            filtered.retain(|run| run.status == status);
        }
        if let Some(conclusion) = filter.conclusion {
            filtered.retain(|run| run.conclusion == Some(conclusion));
        }
        if let Some(per_page) = filter.per_page {
            let page = filter.page.unwrap_or(1);
            let start = ((page - 1) * per_page) as usize;
            let end = (start + per_page as usize).min(filtered.len());
            filtered = filtered[start..end].to_vec();
        }

        Ok(filtered)
    }

    async fn get_run(&self, run_id: u64) -> Result<CiRun, CiProviderError> {
        self.log_call(MockCall::GetRun(run_id));

        if let Some(err) = &self.error {
            return Err(CiProviderError::provider(err.clone()));
        }

        let mut run = self
            .runs
            .iter()
            .find(|run| run.id == run_id)
            .cloned()
            .ok_or_else(|| CiProviderError::provider(format!("Run #{run_id} not found")))?;

        if !self.jobs.is_empty() {
            run.jobs = Some(self.jobs.clone());
        }

        Ok(run)
    }

    async fn get_job_log(&self, job_id: u64) -> Result<String, CiProviderError> {
        self.log_call(MockCall::GetJobLog(job_id));

        if let Some(err) = &self.error {
            return Err(CiProviderError::provider(err.clone()));
        }

        Ok(format!("Mock log output for job {job_id}"))
    }

    async fn get_pull_request(
        &self,
        pr_number: u64,
    ) -> Result<Option<CiPullRequest>, CiProviderError> {
        self.log_call(MockCall::GetPullRequest(pr_number));

        if let Some(err) = &self.error {
            return Err(CiProviderError::provider(err.clone()));
        }

        Ok(self
            .pull_requests
            .iter()
            .find(|pr| pr.number == pr_number)
            .cloned())
    }

    fn provider_name(&self) -> &str {
        "MockCiProvider"
    }
}

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
        attempt: Some(1),
        pull_requests: None,
        jobs: None,
    }
}

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
        url: Some(format!("https://github.com/test/repo/actions/jobs/{id}")),
        steps: None,
    }
}

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
        assert!(provider.pull_requests.is_empty());
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
}
