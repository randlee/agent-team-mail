//! CI Monitor plugin — provider abstraction for CI/CD platforms

mod config;
#[cfg(unix)]
pub(crate) mod gh_cli;
#[cfg(unix)]
pub(crate) mod gh_alerts;
#[cfg(unix)]
pub(crate) mod gh_monitor;
mod github_provider;
mod github_schema;
#[cfg(unix)]
pub(crate) mod health;
pub(crate) mod helpers;
mod loader;
mod mock_provider;
mod plugin;
mod provider;
mod registry;
pub(crate) mod service;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod types;

pub use config::{CiMonitorConfig, DedupStrategy, NotifyTarget};
pub use github_provider::GitHubActionsProvider;
pub use loader::CiProviderLoader;
pub use mock_provider::{
    MockCall, MockCiProvider, create_test_job, create_test_run, create_test_step,
};
pub use plugin::CiMonitorPlugin;
pub use provider::{CiProvider, ErasedCiProvider};
pub use registry::{CiFactoryFn, CiProviderFactory, CiProviderRegistry};
pub use types::{CiFilter, CiJob, CiPullRequest, CiRun, CiRunConclusion, CiRunStatus, CiStep};
