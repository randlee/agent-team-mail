//! CI Monitor plugin — provider abstraction for CI/CD platforms

mod config;
mod github;
pub(crate) mod helpers;
mod loader;
mod mock_provider;
mod plugin;
mod provider;
mod registry;
#[cfg(unix)]
pub(crate) mod service;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod types;

pub use config::{CiMonitorConfig, DedupStrategy, NotifyTarget};
pub use github::GitHubActionsProvider;
pub use loader::CiProviderLoader;
pub use mock_provider::{
    MockCall, MockCiProvider, create_test_job, create_test_run, create_test_step,
};
pub use plugin::CiMonitorPlugin;
pub use provider::{CiProvider, ErasedCiProvider};
pub use registry::{CiFactoryFn, CiProviderFactory, CiProviderRegistry};
pub use types::{CiFilter, CiJob, CiRun, CiRunConclusion, CiRunStatus, CiStep};
