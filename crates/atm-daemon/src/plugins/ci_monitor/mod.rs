//! CI Monitor plugin — provider abstraction for CI/CD platforms

mod config;
#[cfg(unix)]
pub(crate) mod gh_alerts;
mod gh_command_routing;
#[cfg(unix)]
pub(crate) mod gh_monitor;
mod github_provider;
pub(crate) mod github_schema;
#[cfg(unix)]
pub(crate) mod health;
#[cfg(unix)]
pub(crate) mod helpers;
mod loader;
#[cfg(any(test, feature = "test-support"))]
mod mock_provider;
mod plugin;
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
pub use gh_command_routing::{
    GH_MONITOR_REPORT_SCHEMA_VERSION, GhCiRollup, GhMergeReport, GhMonitorCheckReport,
    GhMonitorListItem, GhMonitorReportPr, GhMonitorReviewReport, GhPrListSummary,
    GhPrReportSummary, build_merge_report, build_pr_list_summary, build_pr_report_summary,
    extract_check_reports, extract_review_reports, normalize_merge_status,
    normalize_report_review_decision, normalize_review_status, summarize_ci_rollup,
    validate_gh_cli_prerequisites,
};
pub(crate) use github_provider::run_plugin_owned_gh_subprocess;
pub use github_provider::{
    GitHubActionsProvider, run_attributed_gh_command, run_attributed_gh_command_with_ids,
};
pub use plugin::CiMonitorPlugin;
pub use provider::{CiProvider, ErasedCiProvider};
pub use registry::CiProviderFactory;
pub use types::{
    CiFilter, CiJob, CiProviderError, CiPullRequest, CiRun, CiRunConclusion, CiRunStatus, CiStep,
};

// Production surface: config, provider traits, plugin entrypoint, factory metadata, and
// CI domain types only. Concrete providers/loaders/registries stay internal so this module
// matches the future crate-facing boundary more closely.
// Test-only symbols live under `mock_support` so tests do not rely on the root production API.
#[cfg(any(test, feature = "test-support"))]
pub mod mock_support {
    pub use super::mock_provider::{
        MockCall, MockCiProvider, create_test_job, create_test_run, create_test_step,
    };
}
