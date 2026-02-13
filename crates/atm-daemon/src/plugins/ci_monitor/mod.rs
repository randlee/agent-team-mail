//! CI Monitor plugin — provider abstraction for CI/CD platforms

mod config;
mod github;
mod plugin;
mod provider;
mod registry;
mod types;

pub use config::CiMonitorConfig;
pub use github::GitHubActionsProvider;
pub use plugin::CiMonitorPlugin;
pub use provider::{CiProvider, ErasedCiProvider};
pub use registry::{CiFactoryFn, CiProviderFactory, CiProviderRegistry};
pub use types::{CiFilter, CiJob, CiRun, CiRunConclusion, CiRunStatus, CiStep};
