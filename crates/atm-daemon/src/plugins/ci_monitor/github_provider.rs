//! GitHub Actions provider and attributed gh execution owned by the daemon plugin layer.

use super::github_schema::{GhJob, GhPrView, GhRun};
use agent_team_mail_ci_monitor::{
    CiFilter, CiJob, CiProvider, CiProviderError, CiPullRequest, CiRun, CiRunConclusion,
    CiRunStatus, CiStep, GhCliCallMetadata, GhCliCallOutcome, GhCliObserverContext,
    GhCliObserverRef, build_gh_cli_observer, new_gh_call_id, new_gh_request_id,
};
use std::process::Command;
use std::time::Instant;

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone)]
pub struct GitHubActionsProvider {
    owner: String,
    repo: String,
    observer: Option<GhCliObserverRef>,
}

impl GitHubActionsProvider {
    pub fn new(owner: String, repo: String) -> Self {
        Self {
            owner,
            repo,
            observer: None,
        }
    }

    pub fn with_observer(mut self, observer: GhCliObserverRef) -> Self {
        self.observer = Some(observer);
        self
    }

    pub fn run_gh_with_metadata_blocking(
        observer: Option<GhCliObserverRef>,
        metadata: GhCliCallMetadata,
    ) -> Result<String, CiProviderError> {
        if let Some(observer) = &observer {
            observer.before_gh_call(&metadata)?;
        }

        let started = Instant::now();
        let metadata_for_outcome = metadata.clone();
        let result = run_gh_subprocess(&metadata.args);
        let duration_ms = started.elapsed().as_millis() as u64;

        if let Some(observer) = observer {
            match &result {
                Ok(_) => observer.after_gh_call(&GhCliCallOutcome {
                    metadata: metadata_for_outcome,
                    duration_ms,
                    success: true,
                    error: None,
                }),
                Err(err) => observer.after_gh_call(&GhCliCallOutcome {
                    metadata: metadata_for_outcome,
                    duration_ms,
                    success: false,
                    error: Some(err.to_string()),
                }),
            }
        }

        result
    }

    async fn run_gh_with_metadata(
        &self,
        metadata: GhCliCallMetadata,
    ) -> Result<String, CiProviderError> {
        let observer = self.observer.clone();
        tokio::task::spawn_blocking(move || Self::run_gh_with_metadata_blocking(observer, metadata))
            .await
            .map_err(|e| CiProviderError::runtime(format!("Task join error: {e}")))?
    }

    fn repo_scope(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

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
            attempt: gh_run.attempt,
            pull_requests: gh_run
                .pull_requests
                .as_ref()
                .map(|prs: &Vec<CiPullRequest>| {
                    prs.iter()
                        .map(|pr| CiPullRequest {
                            number: pr.number,
                            url: pr.url.clone(),
                            head_ref_name: None,
                            head_ref_oid: None,
                            created_at: None,
                            merge_state_status: None,
                        })
                        .collect()
                }),
            jobs: if include_jobs {
                gh_run.jobs.as_ref().map(|jobs: &Vec<GhJob>| {
                    jobs.iter().map(|gh_job| self.parse_job(gh_job)).collect()
                })
            } else {
                None
            },
        }
    }

    fn parse_job(&self, gh_job: &GhJob) -> CiJob {
        CiJob {
            id: gh_job.database_id,
            name: gh_job.name.clone(),
            status: Self::parse_status(&gh_job.status),
            conclusion: Self::parse_conclusion(gh_job.conclusion.as_deref()),
            started_at: gh_job.started_at.clone(),
            completed_at: gh_job.completed_at.clone(),
            url: gh_job.url.clone(),
            steps: gh_job
                .steps
                .as_ref()
                .map(|steps: &Vec<agent_team_mail_ci_monitor::GhStep>| {
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

pub fn run_attributed_gh_command(
    ctx: &GhCliObserverContext,
    action: &str,
    args: &[&str],
    branch: Option<&str>,
    reference: Option<&str>,
) -> anyhow::Result<String> {
    run_attributed_gh_command_with_ids(
        ctx,
        action,
        args,
        branch,
        reference,
        new_gh_request_id(),
        new_gh_call_id(),
    )
}

pub fn run_attributed_gh_command_with_ids(
    ctx: &GhCliObserverContext,
    action: &str,
    args: &[&str],
    branch: Option<&str>,
    reference: Option<&str>,
    request_id: String,
    call_id: String,
) -> anyhow::Result<String> {
    let observer = build_gh_cli_observer(ctx.clone());
    let metadata = GhCliCallMetadata {
        request_id,
        call_id,
        repo_scope: ctx.repo.clone(),
        caller: action.to_string(),
        action: action.to_string(),
        args: args.iter().map(|value| (*value).to_string()).collect(),
        branch: branch.map(str::to_string),
        reference: reference.map(str::to_string),
        ledger_home: Some(ctx.home.clone()),
        team: Some(ctx.team.clone()),
        runtime: Some(ctx.runtime.clone()),
        poller_key: ctx.poller_key.clone(),
    };
    GitHubActionsProvider::run_gh_with_metadata_blocking(Some(observer), metadata)
        .map_err(|err| anyhow::anyhow!(err.to_string()))
}

fn run_gh_subprocess(args: &[String]) -> Result<String, CiProviderError> {
    #[cfg(test)]
    GH_SUBPROCESS_COUNT.fetch_add(1, Ordering::SeqCst);

    // NOT_MONITORED_PATH: provider subprocess execution is the gh firewall boundary being wrapped.
    let output = Command::new("gh").args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CiProviderError::provider("gh CLI not found. Install from https://cli.github.com/")
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

    String::from_utf8(output.stdout)
        .map_err(|e| CiProviderError::provider(format!("Invalid UTF-8 in gh output: {e}")))
}

#[cfg(test)]
static GH_SUBPROCESS_COUNT: AtomicUsize = AtomicUsize::new(0);

impl std::fmt::Debug for GitHubActionsProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubActionsProvider")
            .field("owner", &self.owner)
            .field("repo", &self.repo)
            .field("observer", &self.observer.as_ref().map(|_| "<observer>"))
            .finish()
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

        if let Some(branch) = &filter.branch {
            args.push("--branch".to_string());
            args.push(branch.clone());
        }

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

        if let Some(per_page) = filter.per_page {
            args.push("--limit".to_string());
            args.push(per_page.to_string());
        }

        if let Some(event) = &filter.event {
            args.push("--event".to_string());
            args.push(event.clone());
        }

        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self
            .run_gh("gh_run_list", &args_refs, filter.branch.as_deref(), None)
            .await?;

        let gh_runs: Vec<GhRun> = serde_json::from_str(&output)
            .map_err(|e| CiProviderError::provider(format!("Failed to parse gh JSON: {e}")))?;

        let mut runs: Vec<CiRun> = gh_runs.iter().map(|gh| self.parse_run(gh, false)).collect();

        if let Some(conclusion) = filter.conclusion {
            runs.retain(|run| run.conclusion == Some(conclusion));
        }

        if let Some(created) = &filter.created {
            runs.retain(|run| run.created_at.as_str() >= created.as_str());
        }

        Ok(runs)
    }

    async fn get_run(&self, run_id: u64) -> Result<CiRun, CiProviderError> {
        let run_id_arg = run_id.to_string();
        let repo_scope = self.repo_scope();
        let args = [
            "-R",
            repo_scope.as_str(),
            "run",
            "view",
            run_id_arg.as_str(),
            "--json",
            "databaseId,name,status,conclusion,headBranch,headSha,url,createdAt,updatedAt,attempt,pullRequests,jobs",
        ];
        let output = self.run_gh("gh_run_view", &args, None, None).await?;
        let gh_run: GhRun = serde_json::from_str(&output)
            .map_err(|e| CiProviderError::provider(format!("Failed to parse gh JSON: {e}")))?;
        Ok(self.parse_run(&gh_run, true))
    }

    async fn get_job_log(&self, job_id: u64) -> Result<String, CiProviderError> {
        let job_id_arg = job_id.to_string();
        let repo_scope = self.repo_scope();
        let args = [
            "-R",
            repo_scope.as_str(),
            "run",
            "view",
            "--job",
            job_id_arg.as_str(),
            "--log",
        ];
        self.run_gh("gh_job_log", &args, None, None).await
    }

    async fn get_pull_request(
        &self,
        pr_number: u64,
    ) -> Result<Option<CiPullRequest>, CiProviderError> {
        let pr_number_arg = pr_number.to_string();
        let repo_scope = self.repo_scope();
        let args = [
            "-R",
            repo_scope.as_str(),
            "pr",
            "view",
            pr_number_arg.as_str(),
            "--json",
            "number,url,headRefName,headRefOid,createdAt,mergeStateStatus",
        ];
        let output = self.run_gh("gh_pr_view", &args, None, None).await?;
        let pr: GhPrView = serde_json::from_str(&output)
            .map_err(|e| CiProviderError::provider(format!("Failed to parse gh JSON: {e}")))?;
        Ok(Some(CiPullRequest {
            number: pr.number,
            url: pr.url,
            head_ref_name: pr.head_ref_name,
            head_ref_oid: pr.head_ref_oid,
            created_at: pr.created_at,
            merge_state_status: pr.merge_state_status,
        }))
    }

    fn provider_name(&self) -> &str {
        "GitHub Actions"
    }

    async fn run_gh(
        &self,
        action: &str,
        args: &[&str],
        branch: Option<&str>,
        reference: Option<&str>,
    ) -> Result<String, CiProviderError> {
        let metadata = GhCliCallMetadata {
            request_id: new_gh_request_id(),
            call_id: new_gh_call_id(),
            repo_scope: self.repo_scope(),
            caller: action.to_string(),
            action: action.to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            branch: branch.map(str::to_string),
            reference: reference.map(str::to_string),
            ledger_home: None,
            team: None,
            runtime: None,
            poller_key: None,
        };
        self.run_gh_with_metadata(metadata).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Debug)]
    struct BlockingObserver;

    impl agent_team_mail_ci_monitor::GhCliObserver for BlockingObserver {
        fn before_gh_call(&self, _metadata: &GhCliCallMetadata) -> Result<(), CiProviderError> {
            Err(CiProviderError::provider(
                r#"{"code":"gh_firewall_blocked","reason":"test_block","message":"blocked by test observer"}"#,
            ))
        }

        fn after_gh_call(&self, _outcome: &GhCliCallOutcome) {}
    }

    #[test]
    fn blocked_calls_do_not_spawn_gh_subprocess() {
        GH_SUBPROCESS_COUNT.store(0, Ordering::SeqCst);
        let metadata = GhCliCallMetadata {
            request_id: new_gh_request_id(),
            call_id: new_gh_call_id(),
            repo_scope: "owner/repo".to_string(),
            caller: "gh_run_list".to_string(),
            action: "gh_run_list".to_string(),
            args: vec!["run".to_string(), "list".to_string()],
            branch: None,
            reference: None,
            ledger_home: None,
            team: None,
            runtime: None,
            poller_key: None,
        };

        let err = GitHubActionsProvider::run_gh_with_metadata_blocking(
            Some(Arc::new(BlockingObserver)),
            metadata,
        )
        .unwrap_err();

        assert!(err.to_string().contains("\"code\":\"gh_firewall_blocked\""));
        assert_eq!(
            GH_SUBPROCESS_COUNT.load(Ordering::SeqCst),
            0,
            "blocked requests must fail before launching gh"
        );
    }
}
