//! CI Monitor plugin — provider abstraction for CI/CD platforms

mod github;
mod provider;
mod registry;
mod types;

pub use github::GitHubActionsProvider;
pub use provider::{CiProvider, ErasedCiProvider};
pub use registry::{CiFactoryFn, CiProviderFactory, CiProviderRegistry};
pub use types::{CiFilter, CiJob, CiRun, CiRunConclusion, CiRunStatus, CiStep};
