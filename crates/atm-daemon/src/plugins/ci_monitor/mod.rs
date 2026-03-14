//! CI Monitor plugin — provider abstraction for CI/CD platforms

mod config;
#[cfg(unix)]
pub(crate) mod gh_alerts;
pub(crate) mod gh_cli;
#[cfg(unix)]
pub(crate) mod gh_monitor;
mod github_provider;
mod github_schema;
#[cfg(unix)]
pub(crate) mod health;
#[cfg(unix)]
pub(crate) mod helpers;
mod loader;
mod mock_provider;
mod plugin;
#[cfg(unix)]
pub(crate) mod polling;
mod provider;
mod registry;
#[cfg(unix)]
pub(crate) mod routing;
#[cfg(unix)]
pub(crate) mod service;
#[cfg(all(test, unix))]
pub(crate) mod test_support;
pub(crate) mod types;

pub use config::{CiMonitorConfig, DedupStrategy, NotifyTarget};
pub use github_provider::GitHubActionsProvider;
pub use loader::CiProviderLoader;
pub use plugin::CiMonitorPlugin;
pub use provider::{CiProvider, ErasedCiProvider};
pub use registry::{CiFactoryFn, CiProviderFactory, CiProviderRegistry};
pub use types::{
    CiFilter, CiJob, CiProviderError, CiPullRequest, CiRun, CiRunConclusion, CiRunStatus, CiStep,
};

// Production surface: config, provider, registry, plugin, and CI domain types only.
// Test-only symbols live under `mock_support` so tests do not rely on the root production API.
pub mod mock_support {
    #[allow(unused_imports)]
    pub use super::mock_provider::{
        MockCall, MockCiProvider, create_test_job, create_test_run, create_test_step,
    };
}
