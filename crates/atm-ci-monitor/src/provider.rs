//! Provider trait for CI operations across platforms.

use crate::types::{CiFilter, CiProviderError, CiPullRequest, CiRun};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct GhCliCallMetadata {
    pub repo_scope: String,
    pub action: String,
    pub args: Vec<String>,
    pub branch: Option<String>,
    pub reference: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GhCliCallOutcome {
    pub metadata: GhCliCallMetadata,
    pub duration_ms: u64,
    pub success: bool,
    pub error: Option<String>,
}

pub trait GhCliObserver: Send + Sync + std::fmt::Debug {
    fn before_gh_call(&self, metadata: &GhCliCallMetadata) -> Result<(), CiProviderError>;
    fn after_gh_call(&self, outcome: &GhCliCallOutcome);
}

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

    fn run_gh(
        &self,
        action: &str,
        args: &[&str],
        branch: Option<&str>,
        reference: Option<&str>,
    ) -> impl Future<Output = Result<String, CiProviderError>> + Send;

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

    fn run_gh<'a>(
        &'a self,
        action: &'a str,
        args: &'a [&'a str],
        branch: Option<&'a str>,
        reference: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<String, CiProviderError>> + Send + 'a>>;

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

    fn run_gh<'a>(
        &'a self,
        action: &'a str,
        args: &'a [&'a str],
        branch: Option<&'a str>,
        reference: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<String, CiProviderError>> + Send + 'a>> {
        Box::pin(CiProvider::run_gh(self, action, args, branch, reference))
    }

    fn provider_name(&self) -> &str {
        CiProvider::provider_name(self)
    }
}

pub type GhCliObserverRef = Arc<dyn GhCliObserver>;
