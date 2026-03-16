//! Reusable CI-monitor core.
//!
//! This crate owns the CI-monitor production surface that can be reused without
//! depending on ATM daemon bootstrap, plugin lifecycle, or socket transport code.

pub mod consts;
mod gh_ledger;
mod github_provider;
#[cfg(any(test, feature = "test-support"))]
mod mock_provider;
mod provider;
mod registry;
pub mod repo_state;
pub mod service;
mod types;

pub use gh_ledger::read_gh_observability_records;
pub use gh_ledger::{
    GhLedgerKind, GhLedgerRecord, append_gh_observability_record, flush_gh_observability_records,
    gh_observability_ledger_path, new_gh_call_id, new_gh_request_id,
};
pub use github_provider::GitHubActionsProvider;
pub use provider::{
    CiProvider, ErasedCiProvider, GhCliCallMetadata, GhCliCallOutcome, GhCliObserver,
    GhCliObserverRef,
};
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
