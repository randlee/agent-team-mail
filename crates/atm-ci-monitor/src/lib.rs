//! Reusable CI-monitor core.
//!
//! This crate owns the CI-monitor production surface that can be reused without
//! depending on ATM daemon bootstrap, plugin lifecycle, or socket transport code.

mod github_provider;
#[cfg(any(test, feature = "test-support"))]
mod mock_provider;
mod provider;
pub mod repo_state;
mod registry;
pub mod service;
mod types;

pub use github_provider::GitHubActionsProvider;
pub use provider::{CiProvider, ErasedCiProvider, GhCliCallMetadata, GhCliCallOutcome, GhCliObserver, GhCliObserverRef};
pub use registry::{CiFactoryFn, CiProviderFactory, CiProviderRegistry};
pub use types::{
    CiFilter, CiJob, CiProviderError, CiPullRequest, CiRun, CiRunConclusion, CiRunStatus, CiStep,
    GhBranchRefCount, GhObservedCall, GhRateLimitSnapshot, GhRepoStateFile, GhRepoStateRecord,
    GhRuntimeOwner,
};
#[cfg(unix)]
pub use types::{
    CiMonitorControlRequest, CiMonitorHealth, CiMonitorLifecycleAction, CiMonitorRequest,
    CiMonitorStatus, CiMonitorStatusRequest, CiMonitorTargetKind,
};

#[cfg(any(test, feature = "test-support"))]
pub mod mock_support {
    pub use super::mock_provider::{
        MockCall, MockCiProvider, create_test_job, create_test_run, create_test_step,
    };
}
