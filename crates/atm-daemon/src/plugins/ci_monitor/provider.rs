//! Provider trait for CI operations across platforms

use super::types::{CiFilter, CiRun};
use crate::plugin::PluginError;
use std::future::Future;
use std::pin::Pin;

/// Async trait for provider-agnostic CI operations.
///
/// Each CI platform (GitHub Actions, Azure Pipelines, etc.) implements this trait.
/// Uses RPITIT (Return Position Impl Trait in Traits) with explicit Send bounds.
pub trait CiProvider: Send + Sync + std::fmt::Debug {
    /// List CI runs matching filters
    fn list_runs(
        &self,
        filter: &CiFilter,
    ) -> impl Future<Output = Result<Vec<CiRun>, PluginError>> + Send;

    /// Get a single CI run by ID with job details
    fn get_run(&self, run_id: u64) -> impl Future<Output = Result<CiRun, PluginError>> + Send;

    /// Get job log output
    fn get_job_log(
        &self,
        job_id: u64,
    ) -> impl Future<Output = Result<String, PluginError>> + Send;

    /// Provider name for logging/display
    fn provider_name(&self) -> &str;
}

/// Object-safe version of CiProvider for type erasure.
///
/// This trait is implemented automatically for all types that implement CiProvider.
/// Allows storing `Box<dyn ErasedCiProvider>` in the registry or plugin state.
pub trait ErasedCiProvider: Send + Sync + std::fmt::Debug {
    fn list_runs<'a>(
        &'a self,
        filter: &'a CiFilter,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CiRun>, PluginError>> + Send + 'a>>;

    fn get_run<'a>(
        &'a self,
        run_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<CiRun, PluginError>> + Send + 'a>>;

    fn get_job_log<'a>(
        &'a self,
        job_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<String, PluginError>> + Send + 'a>>;

    fn provider_name(&self) -> &str;
}

/// Blanket implementation of ErasedCiProvider for all CiProvider types.
impl<T: CiProvider> ErasedCiProvider for T {
    fn list_runs<'a>(
        &'a self,
        filter: &'a CiFilter,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CiRun>, PluginError>> + Send + 'a>> {
        Box::pin(CiProvider::list_runs(self, filter))
    }

    fn get_run<'a>(
        &'a self,
        run_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<CiRun, PluginError>> + Send + 'a>> {
        Box::pin(CiProvider::get_run(self, run_id))
    }

    fn get_job_log<'a>(
        &'a self,
        job_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<String, PluginError>> + Send + 'a>> {
        Box::pin(CiProvider::get_job_log(self, job_id))
    }

    fn provider_name(&self) -> &str {
        CiProvider::provider_name(self)
    }
}
