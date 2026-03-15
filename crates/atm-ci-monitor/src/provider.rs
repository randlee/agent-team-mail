//! Provider trait for CI operations across platforms.

use crate::types::{CiFilter, CiProviderError, CiPullRequest, CiRun};
use std::future::Future;
use std::pin::Pin;

pub trait CiProvider: Send + Sync + std::fmt::Debug {
    fn list_runs(
        &self,
        filter: &CiFilter,
    ) -> impl Future<Output = Result<Vec<CiRun>, CiProviderError>> + Send;

    fn get_run(&self, run_id: u64) -> impl Future<Output = Result<CiRun, CiProviderError>> + Send;

    fn get_job_log(
        &self,
        job_id: u64,
    ) -> impl Future<Output = Result<String, CiProviderError>> + Send;

    fn get_pull_request(
        &self,
        pr_number: u64,
    ) -> impl Future<Output = Result<Option<CiPullRequest>, CiProviderError>> + Send;

    fn provider_name(&self) -> &str;
}

pub trait ErasedCiProvider: Send + Sync + std::fmt::Debug {
    fn list_runs<'a>(
        &'a self,
        filter: &'a CiFilter,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CiRun>, CiProviderError>> + Send + 'a>>;

    fn get_run<'a>(
        &'a self,
        run_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<CiRun, CiProviderError>> + Send + 'a>>;

    fn get_job_log<'a>(
        &'a self,
        job_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<String, CiProviderError>> + Send + 'a>>;

    fn get_pull_request<'a>(
        &'a self,
        pr_number: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Option<CiPullRequest>, CiProviderError>> + Send + 'a>>;

    fn provider_name(&self) -> &str;
}

impl<T: CiProvider> ErasedCiProvider for T {
    fn list_runs<'a>(
        &'a self,
        filter: &'a CiFilter,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CiRun>, CiProviderError>> + Send + 'a>> {
        Box::pin(CiProvider::list_runs(self, filter))
    }

    fn get_run<'a>(
        &'a self,
        run_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<CiRun, CiProviderError>> + Send + 'a>> {
        Box::pin(CiProvider::get_run(self, run_id))
    }

    fn get_job_log<'a>(
        &'a self,
        job_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<String, CiProviderError>> + Send + 'a>> {
        Box::pin(CiProvider::get_job_log(self, job_id))
    }

    fn get_pull_request<'a>(
        &'a self,
        pr_number: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Option<CiPullRequest>, CiProviderError>> + Send + 'a>>
    {
        Box::pin(CiProvider::get_pull_request(self, pr_number))
    }

    fn provider_name(&self) -> &str {
        CiProvider::provider_name(self)
    }
}
