//! Example external CI provider for Azure DevOps Pipelines
//!
//! This is a stub provider demonstrating how to build an external CI provider
//! that can be dynamically loaded by atm-daemon.
//!
//! # Building
//!
//! ```bash
//! cargo build --release
//! ```
//!
//! This produces a shared library at:
//! - macOS: `target/release/libatm_ci_provider_azdo.dylib`
//! - Linux: `target/release/libatm_ci_provider_azdo.so`
//! - Windows: `target/release/atm_ci_provider_azdo.dll`
//!
//! # Installing
//!
//! Copy the library to `~/.config/atm/providers/` (or `$ATM_HOME/providers/`):
//!
//! ```bash
//! mkdir -p ~/.config/atm/providers
//! cp target/release/libatm_ci_provider_azdo.dylib ~/.config/atm/providers/
//! ```
//!
//! # Usage
//!
//! In your `.atm.toml`:
//!
//! ```toml
//! [plugins.ci_monitor]
//! enabled = true
//! provider = "azure-pipelines"
//!
//! [plugins.ci_monitor.azure]
//! organization = "your-org"
//! project = "your-project"
//! ```
//!
//! The daemon will automatically discover and load this provider.

use atm_daemon::plugin::PluginError;
use atm_daemon::plugins::ci_monitor::{
    CiFilter, CiJob, CiProvider, CiProviderFactory, CiRun, CiRunConclusion, CiRunStatus, CiStep,
    ErasedCiProvider,
};
use std::sync::Arc;

/// Azure DevOps Pipelines provider (stub implementation)
#[derive(Debug)]
pub struct AzurePipelinesProvider {
    organization: String,
    project: String,
    repo: String,
}

impl AzurePipelinesProvider {
    pub fn new(organization: String, project: String, repo: String) -> Self {
        Self {
            organization,
            project,
            repo,
        }
    }

    /// Helper to create a stub run for demonstration
    fn create_stub_run(&self, id: u64) -> CiRun {
        CiRun {
            id,
            name: "Azure Pipeline".to_string(),
            status: CiRunStatus::Completed,
            conclusion: Some(CiRunConclusion::Success),
            head_branch: "main".to_string(),
            head_sha: format!("stub-sha-{id}"),
            url: format!(
                "https://dev.azure.com/{}/{}//_build/results?buildId={id}",
                self.organization, self.project
            ),
            created_at: "2026-02-13T10:00:00Z".to_string(),
            updated_at: "2026-02-13T10:05:00Z".to_string(),
            jobs: None,
        }
    }

    /// Helper to create a stub job for demonstration
    fn create_stub_job(&self, id: u64) -> CiJob {
        CiJob {
            id,
            name: "Build".to_string(),
            status: CiRunStatus::Completed,
            conclusion: Some(CiRunConclusion::Success),
            started_at: Some("2026-02-13T10:01:00Z".to_string()),
            completed_at: Some("2026-02-13T10:04:00Z".to_string()),
            steps: Some(vec![
                CiStep {
                    name: "Checkout".to_string(),
                    status: CiRunStatus::Completed,
                    conclusion: Some(CiRunConclusion::Success),
                    number: 1,
                },
                CiStep {
                    name: "Build".to_string(),
                    status: CiRunStatus::Completed,
                    conclusion: Some(CiRunConclusion::Success),
                    number: 2,
                },
            ]),
        }
    }
}

impl CiProvider for AzurePipelinesProvider {
    async fn list_runs(&self, _filter: &CiFilter) -> Result<Vec<CiRun>, PluginError> {
        // STUB: In a real implementation, this would call:
        // az pipelines runs list --organization <org> --project <project> --output json
        //
        // For now, return placeholder data
        Ok(vec![
            self.create_stub_run(1001),
            self.create_stub_run(1002),
        ])
    }

    async fn get_run(&self, run_id: u64) -> Result<CiRun, PluginError> {
        // STUB: In a real implementation, this would call:
        // az pipelines runs show --id <run_id> --organization <org> --project <project> --output json
        //
        // For now, return placeholder data with jobs
        let mut run = self.create_stub_run(run_id);
        run.jobs = Some(vec![self.create_stub_job(2001)]);
        Ok(run)
    }

    async fn get_job_log(&self, job_id: u64) -> Result<String, PluginError> {
        // STUB: In a real implementation, this would call:
        // az pipelines runs artifact download --artifact-name logs --run-id <run_id> ...
        //
        // For now, return placeholder log
        Ok(format!(
            "Azure Pipelines job log (stub) for job {job_id}\n\
             [Step 1] Checkout...\n\
             [Step 2] Build...\n\
             [Step 3] Test...\n"
        ))
    }

    fn provider_name(&self) -> &str {
        "Azure Pipelines (stub)"
    }
}

/// C-ABI function that creates a CI provider factory
///
/// This function MUST be exported with `#[no_mangle]` and `extern "C"`.
/// The daemon will look for this symbol when loading the library.
///
/// # Safety
///
/// The returned pointer must be created with `Box::into_raw()` and will be
/// freed by the daemon using `Box::from_raw()`.
#[no_mangle]
pub extern "C" fn atm_create_ci_provider_factory() -> *mut CiProviderFactory {
    let factory = CiProviderFactory {
        name: "azure-pipelines".to_string(),
        description: "Azure DevOps Pipelines provider (stub)".to_string(),
        create: Arc::new(|config| {
            // Parse config if provided
            let (organization, project, repo) = if let Some(table) = config {
                let org = table
                    .get("azure")
                    .and_then(|v| v.as_table())
                    .and_then(|t| t.get("organization"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("default-org");

                let proj = table
                    .get("azure")
                    .and_then(|v| v.as_table())
                    .and_then(|t| t.get("project"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("default-project");

                let repo = table
                    .get("azure")
                    .and_then(|v| v.as_table())
                    .and_then(|t| t.get("repo"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("default-repo");

                (org.to_string(), proj.to_string(), repo.to_string())
            } else {
                (
                    "default-org".to_string(),
                    "default-project".to_string(),
                    "default-repo".to_string(),
                )
            };

            Ok(Box::new(AzurePipelinesProvider::new(organization, project, repo))
                as Box<dyn ErasedCiProvider>)
        }),
    };

    Box::into_raw(Box::new(factory))
}
