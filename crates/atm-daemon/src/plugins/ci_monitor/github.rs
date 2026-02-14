//! GitHub Actions provider using the `gh` CLI

use super::provider::CiProvider;
use super::types::{CiFilter, CiJob, CiRun, CiRunConclusion, CiRunStatus, CiStep};
use crate::plugin::PluginError;
use serde::Deserialize;
use std::process::Command;

/// GitHub Actions provider that uses the `gh` CLI
#[derive(Debug)]
pub struct GitHubActionsProvider {
    owner: String,
    repo: String,
}

impl GitHubActionsProvider {
    /// Create a new GitHub Actions provider for the given owner/repo
    pub fn new(owner: String, repo: String) -> Self {
        Self { owner, repo }
    }

    /// Execute a `gh` command and return stdout
    async fn run_gh(&self, args: &[&str]) -> Result<String, PluginError> {
        // Run gh command in a blocking task
        let args_owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        tokio::task::spawn_blocking(move || {
            let output = Command::new("gh")
                .args(&args_owned)
                .output()
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        PluginError::Provider {
                            message: "gh CLI not found. Install from https://cli.github.com/"
                                .to_string(),
                            source: Some(Box::new(e)),
                        }
                    } else {
                        PluginError::Provider {
                            message: format!("Failed to execute gh: {e}"),
                            source: Some(Box::new(e)),
                        }
                    }
                })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(PluginError::Provider {
                    message: format!("gh command failed: {stderr}"),
                    source: None,
                });
            }

            let stdout = String::from_utf8(output.stdout).map_err(|e| PluginError::Provider {
                message: format!("Invalid UTF-8 in gh output: {e}"),
                source: Some(Box::new(e)),
            })?;

            Ok(stdout)
        })
        .await
        .map_err(|e| PluginError::Runtime {
            message: format!("Task join error: {e}"),
            source: Some(Box::new(e)),
        })?
    }

    /// Parse GitHub workflow run status
    fn parse_status(status: &str) -> CiRunStatus {
        match status.to_lowercase().as_str() {
            "queued" => CiRunStatus::Queued,
            "in_progress" => CiRunStatus::InProgress,
            "completed" => CiRunStatus::Completed,
            "waiting" => CiRunStatus::Waiting,
            "requested" => CiRunStatus::Requested,
            "pending" => CiRunStatus::Pending,
            _ => CiRunStatus::Pending,
        }
    }

    /// Parse GitHub workflow run conclusion
    fn parse_conclusion(conclusion: Option<&str>) -> Option<CiRunConclusion> {
        conclusion.map(|c| match c.to_lowercase().as_str() {
            "success" => CiRunConclusion::Success,
            "failure" => CiRunConclusion::Failure,
            "cancelled" => CiRunConclusion::Cancelled,
            "skipped" => CiRunConclusion::Skipped,
            "timed_out" => CiRunConclusion::TimedOut,
            "action_required" => CiRunConclusion::ActionRequired,
            "neutral" => CiRunConclusion::Neutral,
            "stale" => CiRunConclusion::Stale,
            _ => CiRunConclusion::Neutral,
        })
    }

    /// Parse a GhRun into a CiRun
    fn parse_run(&self, gh_run: &GhRun, include_jobs: bool) -> CiRun {
        CiRun {
            id: gh_run.database_id,
            name: gh_run.name.clone(),
            status: Self::parse_status(&gh_run.status),
            conclusion: Self::parse_conclusion(gh_run.conclusion.as_deref()),
            head_branch: gh_run.head_branch.clone(),
            head_sha: gh_run.head_sha.clone(),
            url: gh_run.url.clone(),
            created_at: gh_run.created_at.clone(),
            updated_at: gh_run.updated_at.clone(),
            jobs: if include_jobs {
                gh_run.jobs.as_ref().map(|jobs| {
                    jobs.iter()
                        .map(|gh_job| self.parse_job(gh_job))
                        .collect()
                })
            } else {
                None
            },
        }
    }

    /// Parse a GhJob into a CiJob
    fn parse_job(&self, gh_job: &GhJob) -> CiJob {
        CiJob {
            id: gh_job.database_id,
            name: gh_job.name.clone(),
            status: Self::parse_status(&gh_job.status),
            conclusion: Self::parse_conclusion(gh_job.conclusion.as_deref()),
            started_at: gh_job.started_at.clone(),
            completed_at: gh_job.completed_at.clone(),
            steps: gh_job.steps.as_ref().map(|steps| {
                steps
                    .iter()
                    .map(|gh_step| CiStep {
                        name: gh_step.name.clone(),
                        status: Self::parse_status(&gh_step.status),
                        conclusion: Self::parse_conclusion(gh_step.conclusion.as_deref()),
                        number: gh_step.number,
                    })
                    .collect()
            }),
        }
    }
}

impl CiProvider for GitHubActionsProvider {
    async fn list_runs(&self, filter: &CiFilter) -> Result<Vec<CiRun>, PluginError> {
        let repo_arg = format!("{}/{}", self.owner, self.repo);
        let mut args = vec![
            "run".to_string(),
            "list".to_string(),
            "--repo".to_string(),
            repo_arg,
            "--json".to_string(),
            "databaseId,name,status,conclusion,headBranch,headSha,url,createdAt,updatedAt"
                .to_string(),
        ];

        // Add branch filter
        if let Some(branch) = &filter.branch {
            args.push("--branch".to_string());
            args.push(branch.clone());
        }

        // Add status filter
        if let Some(status) = filter.status {
            let status_arg = match status {
                CiRunStatus::Queued => "queued",
                CiRunStatus::InProgress => "in_progress",
                CiRunStatus::Completed => "completed",
                CiRunStatus::Waiting => "waiting",
                CiRunStatus::Requested => "requested",
                CiRunStatus::Pending => "pending",
            };
            args.push("--status".to_string());
            args.push(status_arg.to_string());
        }

        // Add limit (per_page)
        if let Some(per_page) = filter.per_page {
            args.push("--limit".to_string());
            args.push(per_page.to_string());
        }

        // Add event filter
        if let Some(event) = &filter.event {
            args.push("--event".to_string());
            args.push(event.clone());
        }

        // Convert to &str for run_gh
        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self.run_gh(&args_refs).await?;

        let gh_runs: Vec<GhRun> =
            serde_json::from_str(&output).map_err(|e| PluginError::Provider {
                message: format!("Failed to parse gh JSON: {e}"),
                source: Some(Box::new(e)),
            })?;

        let mut runs: Vec<CiRun> = gh_runs.iter().map(|gh| self.parse_run(gh, false)).collect();

        // Apply conclusion filter (gh doesn't support this directly)
        if let Some(conclusion) = filter.conclusion {
            runs.retain(|run| run.conclusion == Some(conclusion));
        }

        // Apply created filter (gh doesn't support this directly)
        if let Some(created) = &filter.created {
            runs.retain(|run| run.created_at >= *created);
        }

        Ok(runs)
    }

    async fn get_run(&self, run_id: u64) -> Result<CiRun, PluginError> {
        let run_id_arg = run_id.to_string();
        let repo_arg = format!("{}/{}", self.owner, self.repo);
        let args = [
            "run",
            "view",
            &run_id_arg,
            "--repo",
            &repo_arg,
            "--json",
            "databaseId,name,status,conclusion,headBranch,headSha,url,createdAt,updatedAt,jobs",
        ];

        let output = self.run_gh(&args).await?;

        let gh_run: GhRun =
            serde_json::from_str(&output).map_err(|e| PluginError::Provider {
                message: format!("Failed to parse gh JSON: {e}"),
                source: Some(Box::new(e)),
            })?;

        Ok(self.parse_run(&gh_run, true))
    }

    async fn get_job_log(&self, job_id: u64) -> Result<String, PluginError> {
        let job_id_arg = job_id.to_string();
        let repo_arg = format!("{}/{}", self.owner, self.repo);
        let args = ["run", "view", "--repo", &repo_arg, "--job", &job_id_arg, "--log"];

        self.run_gh(&args).await
    }

    fn provider_name(&self) -> &str {
        "GitHub Actions"
    }
}

/// GitHub run JSON schema (from `gh run list --json`)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhRun {
    database_id: u64,
    name: String,
    status: String,
    conclusion: Option<String>,
    head_branch: String,
    head_sha: String,
    url: String,
    created_at: String,
    updated_at: String,
    jobs: Option<Vec<GhJob>>,
}

/// GitHub job JSON schema
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhJob {
    database_id: u64,
    name: String,
    status: String,
    conclusion: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    steps: Option<Vec<GhStep>>,
}

/// GitHub step JSON schema
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhStep {
    name: String,
    status: String,
    conclusion: Option<String>,
    number: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_github_actions_provider_creation() {
        let provider = GitHubActionsProvider::new("owner".to_string(), "repo".to_string());
        assert_eq!(provider.provider_name(), "GitHub Actions");
        assert_eq!(provider.owner, "owner");
        assert_eq!(provider.repo, "repo");
    }

    #[test]
    fn test_parse_status() {
        assert_eq!(
            GitHubActionsProvider::parse_status("queued"),
            CiRunStatus::Queued
        );
        assert_eq!(
            GitHubActionsProvider::parse_status("in_progress"),
            CiRunStatus::InProgress
        );
        assert_eq!(
            GitHubActionsProvider::parse_status("completed"),
            CiRunStatus::Completed
        );
        assert_eq!(
            GitHubActionsProvider::parse_status("QUEUED"),
            CiRunStatus::Queued
        );
        assert_eq!(
            GitHubActionsProvider::parse_status("unknown"),
            CiRunStatus::Pending
        );
    }

    #[test]
    fn test_parse_conclusion() {
        assert_eq!(
            GitHubActionsProvider::parse_conclusion(Some("success")),
            Some(CiRunConclusion::Success)
        );
        assert_eq!(
            GitHubActionsProvider::parse_conclusion(Some("failure")),
            Some(CiRunConclusion::Failure)
        );
        assert_eq!(
            GitHubActionsProvider::parse_conclusion(Some("cancelled")),
            Some(CiRunConclusion::Cancelled)
        );
        assert_eq!(
            GitHubActionsProvider::parse_conclusion(Some("SUCCESS")),
            Some(CiRunConclusion::Success)
        );
        assert_eq!(GitHubActionsProvider::parse_conclusion(None), None);
    }

    #[test]
    fn test_parse_run() {
        let provider = GitHubActionsProvider::new("owner".to_string(), "repo".to_string());

        let gh_run = GhRun {
            database_id: 123456789,
            name: "CI".to_string(),
            status: "completed".to_string(),
            conclusion: Some("success".to_string()),
            head_branch: "main".to_string(),
            head_sha: "abc123def456".to_string(),
            url: "https://github.com/owner/repo/actions/runs/123456789".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:05:00Z".to_string(),
            jobs: None,
        };

        let run = provider.parse_run(&gh_run, false);

        assert_eq!(run.id, 123456789);
        assert_eq!(run.name, "CI");
        assert_eq!(run.status, CiRunStatus::Completed);
        assert_eq!(run.conclusion, Some(CiRunConclusion::Success));
        assert_eq!(run.head_branch, "main");
        assert_eq!(run.head_sha, "abc123def456");
        assert!(run.jobs.is_none());
    }

    #[test]
    fn test_parse_run_with_jobs() {
        let provider = GitHubActionsProvider::new("owner".to_string(), "repo".to_string());

        let gh_run = GhRun {
            database_id: 123456789,
            name: "CI".to_string(),
            status: "completed".to_string(),
            conclusion: Some("success".to_string()),
            head_branch: "main".to_string(),
            head_sha: "abc123def456".to_string(),
            url: "https://github.com/owner/repo/actions/runs/123456789".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:05:00Z".to_string(),
            jobs: Some(vec![GhJob {
                database_id: 987654321,
                name: "build".to_string(),
                status: "completed".to_string(),
                conclusion: Some("success".to_string()),
                started_at: Some("2026-01-01T00:01:00Z".to_string()),
                completed_at: Some("2026-01-01T00:04:00Z".to_string()),
                steps: None,
            }]),
        };

        let run = provider.parse_run(&gh_run, true);

        assert_eq!(run.jobs.as_ref().unwrap().len(), 1);
        let job = &run.jobs.as_ref().unwrap()[0];
        assert_eq!(job.id, 987654321);
        assert_eq!(job.name, "build");
        assert_eq!(job.status, CiRunStatus::Completed);
    }

    #[test]
    fn test_parse_job() {
        let provider = GitHubActionsProvider::new("owner".to_string(), "repo".to_string());

        let gh_job = GhJob {
            database_id: 987654321,
            name: "test".to_string(),
            status: "completed".to_string(),
            conclusion: Some("failure".to_string()),
            started_at: Some("2026-01-01T00:01:00Z".to_string()),
            completed_at: Some("2026-01-01T00:03:00Z".to_string()),
            steps: Some(vec![GhStep {
                name: "Checkout".to_string(),
                status: "completed".to_string(),
                conclusion: Some("success".to_string()),
                number: 1,
            }]),
        };

        let job = provider.parse_job(&gh_job);

        assert_eq!(job.id, 987654321);
        assert_eq!(job.name, "test");
        assert_eq!(job.status, CiRunStatus::Completed);
        assert_eq!(job.conclusion, Some(CiRunConclusion::Failure));
        assert_eq!(job.steps.as_ref().unwrap().len(), 1);
        assert_eq!(job.steps.as_ref().unwrap()[0].name, "Checkout");
    }
}
