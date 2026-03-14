//! GitHub Actions provider using the `gh` CLI

use super::provider::CiProvider;
use super::types::{
    CiFilter, CiJob, CiProviderError, CiPullRequest, CiRun, CiRunConclusion, CiRunStatus, CiStep,
};
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
    async fn run_gh(&self, args: &[&str]) -> Result<String, CiProviderError> {
        // Run gh command in a blocking task
        let args_owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        tokio::task::spawn_blocking(move || {
            let output = Command::new("gh").args(&args_owned).output().map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    CiProviderError::provider(
                        "gh CLI not found. Install from https://cli.github.com/",
                    )
                } else {
                    CiProviderError::provider(format!("Failed to execute gh: {e}"))
                }
            })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(CiProviderError::provider(format!(
                    "gh command failed: {stderr}"
                )));
            }

            let stdout = String::from_utf8(output.stdout).map_err(|e| {
                CiProviderError::provider(format!("Invalid UTF-8 in gh output: {e}"))
            })?;

            Ok(stdout)
        })
        .await
        .map_err(|e| CiProviderError::runtime(format!("Task join error: {e}")))?
    }

    fn repo_scope(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
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
            created_at: gh_run.created_at.clone().unwrap_or_default(),
            updated_at: gh_run.updated_at.clone().unwrap_or_default(),
            attempt: gh_run.attempt,
            pull_requests: gh_run.pull_requests.as_ref().map(|prs| {
                prs.iter()
                    .map(|pr| CiPullRequest {
                        number: pr.number.unwrap_or_default(),
                        url: pr.url.clone(),
                        head_ref_name: None,
                        head_ref_oid: None,
                        created_at: None,
                        merge_state_status: None,
                    })
                    .collect()
            }),
            jobs: if include_jobs {
                gh_run
                    .jobs
                    .as_ref()
                    .map(|jobs| jobs.iter().map(|gh_job| self.parse_job(gh_job)).collect())
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
            url: gh_job.url.clone(),
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
    async fn list_runs(&self, filter: &CiFilter) -> Result<Vec<CiRun>, CiProviderError> {
        let mut args = vec![
            "-R".to_string(),
            self.repo_scope(),
            "run".to_string(),
            "list".to_string(),
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

        let gh_runs: Vec<GhRun> = serde_json::from_str(&output)
            .map_err(|e| CiProviderError::provider(format!("Failed to parse gh JSON: {e}")))?;

        let mut runs: Vec<CiRun> = gh_runs.iter().map(|gh| self.parse_run(gh, false)).collect();

        // Apply conclusion filter (gh doesn't support this directly)
        if let Some(conclusion) = filter.conclusion {
            runs.retain(|run| run.conclusion == Some(conclusion));
        }

        // Apply created filter (gh doesn't support this directly)
        if let Some(created) = &filter.created {
            runs.retain(|run| run.created_at.as_str() >= created.as_str());
        }

        Ok(runs)
    }

    async fn get_run(&self, run_id: u64) -> Result<CiRun, CiProviderError> {
        let run_id_arg = run_id.to_string();
        let args = [
            "-R",
            &self.repo_scope(),
            "run",
            "view",
            &run_id_arg,
            "--json",
            "databaseId,name,status,conclusion,headBranch,headSha,url,createdAt,updatedAt,attempt,pullRequests,jobs",
        ];

        let output = self.run_gh(&args).await?;

        let gh_run: GhRun = serde_json::from_str(&output)
            .map_err(|e| CiProviderError::provider(format!("Failed to parse gh JSON: {e}")))?;

        Ok(self.parse_run(&gh_run, true))
    }

    async fn get_job_log(&self, job_id: u64) -> Result<String, CiProviderError> {
        let job_id_arg = job_id.to_string();
        let args = [
            "-R",
            &self.repo_scope(),
            "run",
            "view",
            "--job",
            &job_id_arg,
            "--log",
        ];

        self.run_gh(&args).await
    }

    async fn get_pull_request(
        &self,
        pr_number: u64,
    ) -> Result<Option<CiPullRequest>, CiProviderError> {
        let pr_number_arg = pr_number.to_string();
        let repo_scope = self.repo_scope();
        let base_args = [
            "-R",
            repo_scope.as_str(),
            "pr",
            "view",
            &pr_number_arg,
            "--json",
        ];

        let output = self
            .run_gh(&[
                base_args[0],
                base_args[1],
                base_args[2],
                base_args[3],
                base_args[4],
                base_args[5],
                "mergeStateStatus,url",
            ])
            .await?;
        let pr: GhPullRequestView = serde_json::from_str(&output)
            .map_err(|e| CiProviderError::provider(format!("Failed to parse gh JSON: {e}")))?;
        let mut ci_pr = CiPullRequest {
            number: pr.number.unwrap_or(pr_number),
            url: pr.url,
            head_ref_name: pr.head_ref_name,
            head_ref_oid: pr.head_ref_oid,
            created_at: pr.created_at,
            merge_state_status: pr.merge_state_status,
        };

        if ci_pr
            .merge_state_status
            .as_deref()
            .is_some_and(super::gh_cli::is_pr_merge_state_dirty)
        {
            return Ok(Some(ci_pr));
        }

        if ci_pr.head_ref_name.is_none()
            && ci_pr.head_ref_oid.is_none()
            && ci_pr.created_at.is_none()
        {
            let detail_output = self
                .run_gh(&[
                    base_args[0],
                    base_args[1],
                    base_args[2],
                    base_args[3],
                    base_args[4],
                    base_args[5],
                    "headRefName,headRefOid,createdAt",
                ])
                .await?;
            let detail: GhPullRequestView = serde_json::from_str(&detail_output)
                .map_err(|e| CiProviderError::provider(format!("Failed to parse gh JSON: {e}")))?;
            ci_pr.number = detail.number.unwrap_or(ci_pr.number);
            ci_pr.head_ref_name = detail.head_ref_name;
            ci_pr.head_ref_oid = detail.head_ref_oid;
            ci_pr.created_at = detail.created_at;
            if ci_pr.url.is_none() {
                ci_pr.url = detail.url;
            }
            if ci_pr.merge_state_status.is_none() {
                ci_pr.merge_state_status = detail.merge_state_status;
            }
        }

        Ok(Some(ci_pr))
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
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    conclusion: Option<String>,
    #[serde(default)]
    head_branch: String,
    #[serde(default)]
    head_sha: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    attempt: Option<u64>,
    pull_requests: Option<Vec<GhAssociatedPullRequest>>,
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
    url: Option<String>,
    steps: Option<Vec<GhStep>>,
}

/// GitHub step JSON schema
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhStep {
    name: String,
    status: String,
    conclusion: Option<String>,
    #[serde(default)]
    number: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhAssociatedPullRequest {
    number: Option<u64>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPullRequestView {
    number: Option<u64>,
    url: Option<String>,
    head_ref_name: Option<String>,
    head_ref_oid: Option<String>,
    created_at: Option<String>,
    merge_state_status: Option<String>,
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
            created_at: Some("2026-01-01T00:00:00Z".to_string()),
            updated_at: Some("2026-01-01T00:05:00Z".to_string()),
            attempt: Some(1),
            pull_requests: None,
            jobs: None,
        };

        let run = provider.parse_run(&gh_run, false);

        assert_eq!(run.id, 123456789);
        assert_eq!(run.name, "CI");
        assert_eq!(run.status, CiRunStatus::Completed);
        assert_eq!(run.conclusion, Some(CiRunConclusion::Success));
        assert_eq!(run.head_branch, "main");
        assert_eq!(run.head_sha, "abc123def456");
        assert_eq!(run.attempt, Some(1));
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
            created_at: Some("2026-01-01T00:00:00Z".to_string()),
            updated_at: Some("2026-01-01T00:05:00Z".to_string()),
            attempt: Some(2),
            pull_requests: None,
            jobs: Some(vec![GhJob {
                database_id: 987654321,
                name: "build".to_string(),
                status: "completed".to_string(),
                conclusion: Some("success".to_string()),
                started_at: Some("2026-01-01T00:01:00Z".to_string()),
                completed_at: Some("2026-01-01T00:04:00Z".to_string()),
                url: Some("https://github.com/owner/repo/actions/jobs/987654321".to_string()),
                steps: None,
            }]),
        };

        let run = provider.parse_run(&gh_run, true);

        assert_eq!(run.jobs.as_ref().unwrap().len(), 1);
        let job = &run.jobs.as_ref().unwrap()[0];
        assert_eq!(job.id, 987654321);
        assert_eq!(job.name, "build");
        assert_eq!(job.status, CiRunStatus::Completed);
        assert_eq!(
            job.url.as_deref(),
            Some("https://github.com/owner/repo/actions/jobs/987654321")
        );
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
            url: Some("https://github.com/owner/repo/actions/jobs/987654321".to_string()),
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
