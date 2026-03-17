//! Transport-free provider-side CI-monitor helpers.

use crate::provider::ErasedCiProvider;
use crate::types::{CiFilter, CiRun, CiRunStatus};

const CI_MONITOR_INTERNAL_ERROR: &str = "INTERNAL_ERROR";

#[derive(Debug, Clone)]
pub struct CiMonitorServiceError {
    pub code: &'static str,
    pub message: String,
}

impl CiMonitorServiceError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(CI_MONITOR_INTERNAL_ERROR, message)
    }
}

impl std::fmt::Display for CiMonitorServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CiMonitorServiceError {}

pub type CiMonitorServiceResult<T> = std::result::Result<T, CiMonitorServiceError>;

pub async fn list_completed_runs(
    provider: &dyn ErasedCiProvider,
) -> CiMonitorServiceResult<Vec<CiRun>> {
    let filter = CiFilter {
        status: Some(CiRunStatus::Completed),
        per_page: Some(20),
        ..Default::default()
    };

    provider
        .list_runs(&filter)
        .await
        .map_err(|e| CiMonitorServiceError::internal(format!("Failed to list runs: {e}")))
}

pub async fn fetch_run_details(
    provider: &dyn ErasedCiProvider,
    run_id: u64,
) -> CiMonitorServiceResult<CiRun> {
    provider
        .get_run(run_id)
        .await
        .map_err(|e| CiMonitorServiceError::internal(format!("Failed to fetch run details: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_support::{MockCall, MockCiProvider, create_test_job, create_test_run};
    use crate::types::CiRunConclusion;

    #[tokio::test]
    async fn test_list_completed_runs_uses_completed_filter() {
        let run = create_test_run(
            7,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Success),
        );
        let provider = MockCiProvider::with_runs(vec![run.clone()]);

        let runs = list_completed_runs(&provider).await.unwrap();

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run.id);
        assert_eq!(runs[0].name, run.name);
        assert_eq!(
            provider.get_calls(),
            vec![MockCall::ListRuns(CiFilter {
                status: Some(CiRunStatus::Completed),
                per_page: Some(20),
                ..Default::default()
            })]
        );
    }

    #[tokio::test]
    async fn test_fetch_run_details_uses_provider_boundary() {
        let run = create_test_run(
            42,
            "CI",
            "develop",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );
        let provider = MockCiProvider::with_runs_and_jobs(
            vec![run],
            vec![create_test_job(
                9001,
                "unit",
                CiRunStatus::Completed,
                Some(CiRunConclusion::Failure),
            )],
        );

        let full_run = fetch_run_details(&provider, 42).await.unwrap();

        assert_eq!(full_run.id, 42);
        assert_eq!(full_run.jobs.as_ref().map(Vec::len), Some(1));
        assert_eq!(provider.get_calls(), vec![MockCall::GetRun(42)]);
    }
}
