//! Reusable CI-monitor core.
//!
//! This crate owns the CI-monitor production surface that can be reused without
//! depending on ATM daemon bootstrap, plugin lifecycle, or socket transport code.

pub mod consts;
mod gh_ledger;
mod github_provider;
#[cfg(any(test, feature = "test-support"))]
mod mock_provider;
mod observability;
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
pub use observability::{
    GhCliObserverContext, RateLimitUpdate, build_gh_cli_observer, emit_gh_info_degraded,
    emit_gh_info_denied, emit_gh_info_live_refresh, emit_gh_info_requested,
    emit_gh_info_served_from_cache, gh_repo_state_cache_age_secs, gh_repo_state_path_for,
    new_gh_execution_call_id, new_gh_info_request_id, read_gh_repo_state,
    read_gh_repo_state_record, run_attributed_gh_command, run_attributed_gh_command_with_ids,
    update_gh_repo_state_blocked, update_gh_repo_state_in_flight, update_gh_repo_state_rate_limit,
};
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
