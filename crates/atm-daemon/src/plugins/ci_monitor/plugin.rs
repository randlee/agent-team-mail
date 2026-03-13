//! CI Monitor plugin implementation

use super::config::{CiMonitorConfig, DedupStrategy};
use super::github_provider::GitHubActionsProvider;
use super::loader::CiProviderLoader;
use super::provider::ErasedCiProvider;
use super::registry::{CiProviderFactory, CiProviderRegistry};
#[cfg(unix)]
use super::service::{fetch_run_details, list_completed_runs};
#[cfg(unix)]
use super::types::GhMonitorHealthFile;
#[cfg(test)]
use super::types::{CiFilter, CiRunStatus};
use super::types::{CiJob, CiRunConclusion};
use crate::plugin::{Capability, Plugin, PluginContext, PluginError, PluginMetadata};
use agent_team_mail_core::context::{GitProvider as GitProviderType, RepoContext};
use agent_team_mail_core::daemon_client::GhMonitorHealth;
use agent_team_mail_core::schema::{AgentMember, InboxMessage, TeamConfig};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

const RUNTIME_HISTORY_FILE_NAME: &str = "runtime-history.json";
const RUNTIME_PROCESSED_RUN_LIMIT: usize = 500;
const MAX_ERROR_BACKOFF_SECS: u64 = 40;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct RuntimeHistory {
    workflow_samples: HashMap<String, Vec<u64>>,
    job_samples: HashMap<String, Vec<u64>>,
    processed_run_ids: Vec<u64>,
    drift_last_alert_epoch_secs: HashMap<String, i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct GhMonitorStateRecord {
    team: String,
    state: String,
    run_id: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(default)]
struct GhMonitorStateFile {
    records: Vec<GhMonitorStateRecord>,
}

#[derive(Debug, Clone)]
struct RuntimeDriftEvent {
    alert_key: String,
    kind: &'static str,
    name: String,
    current_secs: u64,
    baseline_secs: u64,
}

/// CI Monitor plugin — bridges CI provider runs to agent team messaging
pub struct CiMonitorPlugin {
    /// The CI provider (GitHub Actions, Azure Pipelines, etc.)
    provider: Option<Box<dyn ErasedCiProvider>>,
    /// Plugin configuration from [plugins.gh_monitor]
    config: CiMonitorConfig,
    /// Provider registry for runtime provider selection
    registry: Option<CiProviderRegistry>,
    /// Provider loader (kept alive to hold dynamic libraries)
    loader: Option<CiProviderLoader>,
    /// Cached context for runtime use
    ctx: Option<PluginContext>,
    /// Tracking: seen run dedup keys with their timestamps
    seen_runs: HashMap<String, DateTime<Utc>>,
    /// Runtime duration baselines and processed-run dedup state.
    runtime_history: RuntimeHistory,
    /// Persisted runtime history path (initialized in init when enabled).
    runtime_history_path: Option<PathBuf>,
}

impl CiMonitorPlugin {
    /// Create a new CI Monitor plugin instance
    pub fn new() -> Self {
        Self {
            provider: None,
            config: CiMonitorConfig::default(),
            registry: None,
            loader: None,
            ctx: None,
            seen_runs: HashMap::new(),
            runtime_history: RuntimeHistory::default(),
            runtime_history_path: None,
        }
    }

    /// Inject a provider for testing (replaces normal init provider creation)
    ///
    /// NOTE: This is intended for testing only. Production code should use init()
    /// which creates the provider from the PluginContext.
    pub fn with_provider(mut self, provider: Box<dyn ErasedCiProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Inject config for testing
    ///
    /// NOTE: This is intended for testing only. Production code should use init()
    /// which parses config from the PluginContext.
    pub fn with_config(mut self, config: CiMonitorConfig) -> Self {
        self.config = config;
        self
    }

    #[cfg(test)]
    fn with_runtime_history(mut self, runtime_history: RuntimeHistory) -> Self {
        self.runtime_history = runtime_history;
        self
    }

    /// Build the provider registry with built-in and external providers
    fn build_registry(&mut self, atm_home: &std::path::Path) -> CiProviderRegistry {
        let mut registry = CiProviderRegistry::new();

        // Register built-in GitHub Actions provider
        registry.register(CiProviderFactory {
            name: "github".to_string(),
            description: "GitHub Actions provider (built-in)".to_string(),
            create: Arc::new(|_config| {
                // GitHub provider requires owner/repo from git context
                Err(PluginError::Provider {
                    message: "GitHub Actions provider requires owner/repo from git context"
                        .to_string(),
                    source: None,
                })
            }),
        });

        // Load external providers from provider directory
        let provider_dir = atm_home.join("providers");
        let mut loader = CiProviderLoader::new();
        match loader.load_from_directory(&provider_dir) {
            Ok(factories) => {
                debug!("Loaded {} external CI providers", factories.len());
                for factory in factories {
                    registry.register(factory);
                }
            }
            Err(e) => {
                warn!("Failed to load external CI providers: {}", e);
            }
        }

        // Load config-specified provider libraries
        if !self.config.provider_libraries.is_empty() {
            let paths: Vec<PathBuf> = self.config.provider_libraries.values().cloned().collect();
            let factories = loader.load_libraries(&paths);
            for factory in factories {
                registry.register(factory);
            }
        }

        // Keep loader alive so dynamic libraries stay loaded
        self.loader = Some(loader);

        registry
    }

    /// Select and create a provider from the registry
    fn create_provider_from_registry(
        &self,
        registry: &CiProviderRegistry,
        git_provider: Option<&GitProviderType>,
        config_table: Option<&toml::Table>,
    ) -> Result<Box<dyn ErasedCiProvider>, PluginError> {
        // Prefer git auto-detection when available; only fall back to explicit
        // plugin config owner/repo when repository context is unavailable.
        let (owner, repo) = if let Some(git_provider) = git_provider {
            match git_provider {
                GitProviderType::GitHub { owner, repo } => (owner.clone(), repo.clone()),
                GitProviderType::AzureDevOps { org, project, repo } => {
                    return Err(PluginError::Provider {
                        message: format!(
                            "Azure DevOps not yet supported (org: {org}, project: {project}, repo: {repo})"
                        ),
                        source: None,
                    });
                }
                GitProviderType::GitLab { namespace, repo } => {
                    return Err(PluginError::Provider {
                        message: format!(
                            "GitLab not yet supported (namespace: {namespace}, repo: {repo})"
                        ),
                        source: None,
                    });
                }
                GitProviderType::Bitbucket { workspace, repo } => {
                    return Err(PluginError::Provider {
                        message: format!(
                            "Bitbucket not yet supported (workspace: {workspace}, repo: {repo})"
                        ),
                        source: None,
                    });
                }
                GitProviderType::Unknown { host } => {
                    return Err(PluginError::Provider {
                        message: format!("No CI provider for unknown git host: {host}"),
                        source: None,
                    });
                }
            }
        } else if let (Some(owner), Some(repo)) = (&self.config.owner, &self.config.repo) {
            debug!(
                "gh_monitor falling back to config-provided repo {}/{} because git auto-detection was unavailable",
                owner, repo
            );
            (owner.clone(), repo.clone())
        } else {
            return Err(PluginError::Provider {
                message: "No repository information available".to_string(),
                source: None,
            });
        };

        // For now, only GitHub is supported built-in
        if self.config.provider == "github" {
            debug!("Creating GitHub Actions provider for {}/{}", owner, repo);
            Ok(Box::new(GitHubActionsProvider::new(owner, repo)))
        } else {
            // Try to create from registry (for external providers)
            registry.create_provider(&self.config.provider, config_table)
        }
    }

    /// Generate a deduplication key for a run based on configured strategy
    ///
    /// PerCommit: "ci-{head_sha}-{conclusion}" — notify once per commit+conclusion
    /// PerRun: "ci-{run_id}-{conclusion}" — notify once per run_id+conclusion
    fn dedup_key(&self, run: &super::types::CiRun) -> String {
        let conclusion_str = run
            .conclusion
            .map(|c| format!("{c:?}"))
            .unwrap_or_else(|| "InProgress".to_string());

        match self.config.dedup_strategy {
            DedupStrategy::PerCommit => {
                format!("ci-{}-{}", run.head_sha, conclusion_str)
            }
            DedupStrategy::PerRun => {
                format!("ci-{}-{}", run.id, conclusion_str)
            }
        }
    }

    /// Evict old entries from the dedup cache based on TTL
    fn evict_old_dedup_entries(&mut self) {
        let ttl = chrono::Duration::hours(self.config.dedup_ttl_hours as i64);
        let cutoff = Utc::now() - ttl;

        self.seen_runs.retain(|_key, timestamp| *timestamp > cutoff);
    }

    /// Generate failure reports (JSON + Markdown) in the report directory
    fn generate_reports(&self, run: &super::types::CiRun) -> Result<(), PluginError> {
        // Create report directory if it doesn't exist
        std::fs::create_dir_all(&self.config.report_dir).map_err(|e| PluginError::Runtime {
            message: format!(
                "Failed to create report directory: {}",
                self.config.report_dir.display()
            ),
            source: Some(Box::new(e)),
        })?;

        // Write JSON report
        let json_path = self.config.report_dir.join(format!("{}.json", run.id));
        let json_content = serde_json::to_string_pretty(run).map_err(|e| PluginError::Runtime {
            message: format!("Failed to serialize run to JSON: {}", run.id),
            source: Some(Box::new(e)),
        })?;
        std::fs::write(&json_path, json_content).map_err(|e| PluginError::Runtime {
            message: format!("Failed to write JSON report: {}", json_path.display()),
            source: Some(Box::new(e)),
        })?;

        // Write Markdown report
        let md_path = self.config.report_dir.join(format!("{}.md", run.id));
        let conclusion_display = match run.conclusion {
            Some(CiRunConclusion::Failure) => "Failed",
            Some(CiRunConclusion::TimedOut) => "Timed Out",
            Some(CiRunConclusion::Cancelled) => "Cancelled",
            Some(CiRunConclusion::ActionRequired) => "Action Required",
            _ => "Unknown Issue",
        };

        let failed_jobs = run
            .jobs
            .as_ref()
            .map(|jobs| {
                jobs.iter()
                    .filter(|job| {
                        job.conclusion == Some(CiRunConclusion::Failure)
                            || job.conclusion == Some(CiRunConclusion::TimedOut)
                    })
                    .map(|job| format!("- {}", job.name))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| "- (job details not available)".to_string());

        let md_content = format!(
            "# CI Run {} - {}\n\n\
            **Status:** {}\n\
            **Branch:** {}\n\
            **Commit:** {}\n\
            **URL:** {}\n\n\
            ## Failed Jobs\n\n\
            {}\n\n\
            ---\n\
            *Generated by atm-daemon CI Monitor plugin*\n",
            run.id,
            conclusion_display,
            conclusion_display,
            run.head_branch,
            run.head_sha,
            run.url,
            failed_jobs
        );

        std::fs::write(&md_path, md_content).map_err(|e| PluginError::Runtime {
            message: format!("Failed to write Markdown report: {}", md_path.display()),
            source: Some(Box::new(e)),
        })?;

        debug!(
            "Generated reports for run #{}: {} and {}",
            run.id,
            json_path.display(),
            md_path.display()
        );
        Ok(())
    }

    /// Check if a branch matches the configured branch filter
    ///
    /// Returns true if:
    /// - No branch patterns configured (match all)
    /// - Branch matches any configured glob pattern
    fn matches_branch(&self, branch: &str) -> bool {
        match &self.config.branch_matcher {
            None => true, // No filter = match all
            Some(matcher) => matcher.is_match(branch),
        }
    }

    fn resolve_repo_context(&self, ctx: &PluginContext) -> Result<RepoContext, PluginError> {
        if let Some(repo) = ctx.system.repo.as_ref() {
            return Ok(repo.clone());
        }

        let (owner, repo) = match (&self.config.owner, &self.config.repo) {
            (Some(owner), Some(repo)) => (owner.clone(), repo.clone()),
            _ => {
                return Err(PluginError::Init {
                    message: "No repository information available".to_string(),
                    source: None,
                });
            }
        };

        let home_dir =
            agent_team_mail_core::home::get_home_dir().map_err(|e| PluginError::Init {
                message: format!("Could not determine home directory: {e}"),
                source: None,
            })?;
        let current_dir = std::env::current_dir().map_err(|e| PluginError::Init {
            message: format!("Could not determine current directory: {e}"),
            source: None,
        })?;

        let config_root = std::env::var("ATM_CONFIG")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                agent_team_mail_core::config::resolve_plugin_config_location(
                    "gh_monitor",
                    &current_dir,
                    &home_dir,
                )
                .map(|location| location.path)
            })
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf));

        let config_root = match config_root {
            Some(config_root) => config_root,
            None => {
                warn!(
                    "gh_monitor: no git context and no config file path available; report_dir will resolve relative to daemon CWD ({})",
                    current_dir.display()
                );
                current_dir
            }
        };

        debug!(
            "gh_monitor falling back to config-provided repo {}/{} rooted at {}",
            owner,
            repo,
            config_root.display()
        );

        Ok(RepoContext::new(repo.clone(), config_root)
            .with_remote(format!("https://github.com/{owner}/{repo}.git")))
    }

    /// Transform a CI failure into an InboxMessage for delivery
    ///
    /// If multiple notify_targets are configured, includes a note listing all recipients.
    fn run_to_message(&self, run: &super::types::CiRun) -> InboxMessage {
        let conclusion_display = match run.conclusion {
            Some(CiRunConclusion::Failure) => "failed",
            Some(CiRunConclusion::TimedOut) => "timed out",
            Some(CiRunConclusion::Cancelled) => "was cancelled",
            Some(CiRunConclusion::ActionRequired) => "requires action",
            _ => "has an issue",
        };

        // Extract failed job names
        let failed_jobs = run
            .jobs
            .as_ref()
            .map(|jobs| {
                jobs.iter()
                    .filter(|job| {
                        job.conclusion == Some(CiRunConclusion::Failure)
                            || job.conclusion == Some(CiRunConclusion::TimedOut)
                    })
                    .map(|job| job.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|| "(job details not available)".to_string());

        let mut content = format!(
            "[ci:{}] CI {} on {}: {}\nCommit: {}\nFailed jobs: {}\nURL: {}",
            run.id,
            conclusion_display,
            run.head_branch,
            run.name,
            run.head_sha,
            failed_jobs,
            run.url
        );

        // Add multi-recipient note if multiple targets configured
        if self.config.notify_target.len() > 1 {
            let recipients: Vec<String> = self
                .config
                .notify_target
                .iter()
                .map(|t| {
                    let team = t.team.as_ref().unwrap_or(&self.config.team);
                    format!("{}@{}", t.agent, team)
                })
                .collect();
            content.push_str(&format!("\n\nNotified: {}", recipients.join(", ")));
        }

        let message_id = self.dedup_key(run);

        InboxMessage {
            from: self.config.agent.clone(),
            text: content,
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(format!(
                "CI {} on {}: {}",
                conclusion_display, run.head_branch, run.name
            )),
            message_id: Some(message_id),
            unknown_fields: std::collections::HashMap::new(),
        }
    }

    fn runtime_history_file_path(report_dir: &std::path::Path) -> PathBuf {
        report_dir.join(RUNTIME_HISTORY_FILE_NAME)
    }

    fn load_runtime_history(path: &std::path::Path) -> RuntimeHistory {
        match std::fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str::<RuntimeHistory>(&content) {
                Ok(history) => history,
                Err(e) => {
                    warn!(
                        "CI Monitor: Failed to parse runtime history {}: {}",
                        path.display(),
                        e
                    );
                    RuntimeHistory::default()
                }
            },
            Err(_) => RuntimeHistory::default(),
        }
    }

    fn persist_runtime_history(&self) {
        let Some(path) = &self.runtime_history_path else {
            return;
        };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            warn!(
                "CI Monitor: Failed to create runtime history directory {}: {}",
                parent.display(),
                e
            );
            return;
        }
        let serialized = match serde_json::to_string_pretty(&self.runtime_history) {
            Ok(s) => s,
            Err(e) => {
                warn!("CI Monitor: Failed to serialize runtime history: {e}");
                return;
            }
        };
        if let Err(e) = std::fs::write(path, serialized) {
            warn!(
                "CI Monitor: Failed to write runtime history {}: {}",
                path.display(),
                e
            );
        }
    }

    fn duration_secs(start: &str, end: &str) -> Option<u64> {
        let start = chrono::DateTime::parse_from_rfc3339(start).ok()?;
        let end = chrono::DateTime::parse_from_rfc3339(end).ok()?;
        let delta = end.signed_duration_since(start).num_seconds();
        (delta > 0).then_some(delta as u64)
    }

    fn trim_history_samples(samples: &mut Vec<u64>, limit: usize) {
        if samples.len() > limit {
            let overflow = samples.len() - limit;
            samples.drain(0..overflow);
        }
    }

    pub(crate) fn evaluate_runtime_drift(
        samples: &[u64],
        current_secs: u64,
        min_samples: usize,
        threshold_percent: u64,
    ) -> Option<u64> {
        if samples.len() < min_samples {
            return None;
        }
        let baseline_secs = samples.iter().sum::<u64>() / samples.len() as u64;
        if baseline_secs == 0 {
            return None;
        }
        let threshold_secs = baseline_secs.saturating_mul(100 + threshold_percent) / 100;
        (current_secs > threshold_secs).then_some(baseline_secs)
    }

    fn is_alert_on_cooldown(
        history: &RuntimeHistory,
        alert_key: &str,
        now_epoch_secs: i64,
        cooldown_secs: u64,
    ) -> bool {
        let Some(last_alert_epoch_secs) = history.drift_last_alert_epoch_secs.get(alert_key) else {
            return false;
        };
        now_epoch_secs.saturating_sub(*last_alert_epoch_secs) < cooldown_secs as i64
    }

    fn update_runtime_history_and_build_alert(
        &mut self,
        run: &super::types::CiRun,
    ) -> Option<InboxMessage> {
        if !self.config.runtime_drift_enabled {
            return None;
        }
        if self.runtime_history.processed_run_ids.contains(&run.id) {
            return None;
        }

        let now_epoch_secs = Utc::now().timestamp();
        let mut drift_events: Vec<RuntimeDriftEvent> = Vec::new();

        if let Some(run_secs) = Self::duration_secs(&run.created_at, &run.updated_at) {
            let workflow_alert_key = format!("workflow::{}", run.name);
            let baseline = {
                let existing = self
                    .runtime_history
                    .workflow_samples
                    .get(&run.name)
                    .cloned()
                    .unwrap_or_default();
                Self::evaluate_runtime_drift(
                    &existing,
                    run_secs,
                    self.config.runtime_drift_min_samples,
                    self.config.runtime_drift_threshold_percent,
                )
            };
            if let Some(baseline_secs) = baseline {
                if !Self::is_alert_on_cooldown(
                    &self.runtime_history,
                    &workflow_alert_key,
                    now_epoch_secs,
                    self.config.alert_cooldown_secs,
                ) {
                    drift_events.push(RuntimeDriftEvent {
                        alert_key: workflow_alert_key,
                        kind: "workflow",
                        name: run.name.clone(),
                        current_secs: run_secs,
                        baseline_secs,
                    });
                }
            }
            let samples = self
                .runtime_history
                .workflow_samples
                .entry(run.name.clone())
                .or_default();
            samples.push(run_secs);
            Self::trim_history_samples(samples, self.config.runtime_history_limit);
        }

        if let Some(jobs) = run.jobs.as_ref() {
            for job in jobs {
                let Some(job_secs) = Self::job_duration_secs(job) else {
                    continue;
                };
                let job_key = format!("{}::{}", run.name, job.name);
                let baseline = {
                    let existing = self
                        .runtime_history
                        .job_samples
                        .get(&job_key)
                        .cloned()
                        .unwrap_or_default();
                    Self::evaluate_runtime_drift(
                        &existing,
                        job_secs,
                        self.config.runtime_drift_min_samples,
                        self.config.runtime_drift_threshold_percent,
                    )
                };
                if let Some(baseline_secs) = baseline {
                    let job_alert_key = format!("job::{job_key}");
                    if !Self::is_alert_on_cooldown(
                        &self.runtime_history,
                        &job_alert_key,
                        now_epoch_secs,
                        self.config.alert_cooldown_secs,
                    ) {
                        drift_events.push(RuntimeDriftEvent {
                            alert_key: job_alert_key,
                            kind: "job",
                            name: job_key.clone(),
                            current_secs: job_secs,
                            baseline_secs,
                        });
                    }
                }
                let samples = self.runtime_history.job_samples.entry(job_key).or_default();
                samples.push(job_secs);
                Self::trim_history_samples(samples, self.config.runtime_history_limit);
            }
        }

        self.runtime_history.processed_run_ids.push(run.id);
        Self::trim_processed_ids(
            &mut self.runtime_history.processed_run_ids,
            RUNTIME_PROCESSED_RUN_LIMIT,
        );

        if drift_events.is_empty() {
            self.persist_runtime_history();
            return None;
        }

        for event in &drift_events {
            self.runtime_history
                .drift_last_alert_epoch_secs
                .insert(event.alert_key.clone(), now_epoch_secs);
        }
        self.persist_runtime_history();

        let threshold = self.config.runtime_drift_threshold_percent;
        let mut details = String::new();
        for event in &drift_events {
            let line = format!(
                "- {} `{}` current={}s baseline={}s threshold={}%",
                event.kind, event.name, event.current_secs, event.baseline_secs, threshold
            );
            if !details.is_empty() {
                details.push('\n');
            }
            details.push_str(&line);
        }

        Some(InboxMessage {
            from: self.config.agent.clone(),
            text: format!(
                "[runtime-drift:{}] Significant runtime drift detected\nWorkflow: {}\nBranch: {}\nRun URL: {}\n{}",
                run.id, run.name, run.head_branch, run.url, details
            ),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(format!(
                "Runtime drift detected for {} (#{}).",
                run.name, run.id
            )),
            message_id: Some(format!("ci-drift-{}", run.id)),
            unknown_fields: std::collections::HashMap::new(),
        })
    }

    fn trim_processed_ids(ids: &mut Vec<u64>, limit: usize) {
        if ids.len() > limit {
            let overflow = ids.len() - limit;
            ids.drain(0..overflow);
        }
    }

    fn job_duration_secs(job: &CiJob) -> Option<u64> {
        let start = job.started_at.as_deref()?;
        let end = job.completed_at.as_deref()?;
        Self::duration_secs(start, end)
    }

    fn send_message_to_targets(
        &self,
        ctx: &PluginContext,
        msg: &InboxMessage,
        run_id: u64,
    ) -> bool {
        if self.config.notify_target.is_empty() {
            if let Err(e) = ctx.mail.send(&self.config.team, &self.config.agent, msg) {
                warn!(
                    "CI Monitor: Failed to send message for run #{}: {e}",
                    run_id
                );
                return false;
            }
            return true;
        }

        let mut sent_count = 0;
        for target in &self.config.notify_target {
            let target_team = target.team.as_ref().unwrap_or(&self.config.team);
            let inbox_path = ctx
                .mail
                .teams_root()
                .join(target_team)
                .join("inboxes")
                .join(format!("{}.json", target.agent));
            if !inbox_path.exists() {
                warn!(
                    "CI Monitor: Target inbox '{}@{}' not found at {}. Target may not exist or hasn't joined yet.",
                    target.agent,
                    target_team,
                    inbox_path.display()
                );
            }

            if let Err(e) = ctx.mail.send(target_team, &target.agent, msg) {
                warn!(
                    "CI Monitor: Failed to send message to {}@{}: {}",
                    target.agent, target_team, e
                );
            } else {
                sent_count += 1;
            }
        }
        sent_count > 0
    }

    fn gh_monitor_state_path(ctx: &PluginContext) -> PathBuf {
        let Some(home_dir) = ctx.system.claude_root.parent() else {
            return ctx
                .system
                .claude_root
                .join("daemon")
                .join("gh-monitor-state.json");
        };
        agent_team_mail_core::daemon_client::daemon_runtime_dir_for(home_dir)
            .join("gh-monitor-state.json")
    }

    fn is_terminal_monitor_state(state: &str) -> bool {
        matches!(
            state.to_ascii_lowercase().as_str(),
            "success" | "failure" | "timed_out" | "cancelled" | "action_required" | "unknown"
        )
    }

    fn was_terminal_notified_by_command_path(&self, ctx: &PluginContext, run_id: u64) -> bool {
        let path = Self::gh_monitor_state_path(ctx);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(_) => return false,
        };
        let state_file = match serde_json::from_str::<GhMonitorStateFile>(&raw) {
            Ok(parsed) => parsed,
            Err(e) => {
                warn!(
                    "CI Monitor: Failed to parse gh monitor state {}: {}",
                    path.display(),
                    e
                );
                return false;
            }
        };
        state_file.records.iter().any(|record| {
            record.team == self.config.team
                && record.run_id == Some(run_id)
                && Self::is_terminal_monitor_state(&record.state)
        })
    }

    fn team_for_config_error(
        table: Option<&agent_team_mail_core::toml::Table>,
        ctx: &PluginContext,
    ) -> String {
        table
            .and_then(|t| t.get("team"))
            .and_then(|v| v.as_str())
            .filter(|team| !team.trim().is_empty())
            .map(|team| team.trim().to_string())
            .unwrap_or_else(|| ctx.config.core.default_team.clone())
    }

    #[cfg(unix)]
    fn write_health_record(
        ctx: &PluginContext,
        team: &str,
        availability_state: &str,
        message: &str,
    ) {
        let Some(home_dir) = ctx.system.claude_root.parent() else {
            warn!("CI Monitor: failed to derive ATM home for health file");
            return;
        };
        let path = agent_team_mail_core::daemon_client::daemon_gh_monitor_health_path_for(home_dir);
        let mut file = match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<GhMonitorHealthFile>(&raw) {
                Ok(parsed) => parsed,
                Err(e) => {
                    warn!(
                        "CI Monitor: failed parsing health file {}: {}",
                        path.display(),
                        e
                    );
                    GhMonitorHealthFile::default()
                }
            },
            Err(_) => GhMonitorHealthFile::default(),
        };

        let updated_record = GhMonitorHealth {
            team: team.to_string(),
            configured: false,
            enabled: false,
            config_source: None,
            config_path: None,
            lifecycle_state: "running".to_string(),
            availability_state: availability_state.to_string(),
            in_flight: 0,
            updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            message: Some(message.to_string()),
        };

        if let Some(existing) = file.records.iter_mut().find(|record| record.team == team) {
            *existing = updated_record;
        } else {
            file.records.push(updated_record);
        }
        file.records.sort_by(|a, b| a.team.cmp(&b.team));

        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            warn!(
                "CI Monitor: failed to create health directory {}: {}",
                parent.display(),
                e
            );
            return;
        }
        match serde_json::to_string_pretty(&file) {
            Ok(serialized) => {
                if let Err(e) = std::fs::write(&path, serialized) {
                    warn!(
                        "CI Monitor: failed writing health file {}: {}",
                        path.display(),
                        e
                    );
                }
            }
            Err(e) => {
                warn!("CI Monitor: failed serializing health file: {}", e);
            }
        }
    }

    #[cfg(not(unix))]
    fn write_health_record(
        _ctx: &PluginContext,
        _team: &str,
        _availability_state: &str,
        _message: &str,
    ) {
    }

    fn notify_disabled_transition(&self, ctx: &PluginContext, team: &str, message: &str) {
        let lead_agent =
            std::fs::read_to_string(ctx.mail.teams_root().join(team).join("config.json"))
                .ok()
                .and_then(|raw| serde_json::from_str::<TeamConfig>(&raw).ok())
                .and_then(|cfg| cfg.lead_agent_id.split('@').next().map(|s| s.to_string()))
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "team-lead".to_string());

        let text = format!(
            "[gh_monitor] availability transition healthy -> disabled_config_error\nreason: {message}"
        );
        let msg = InboxMessage {
            from: if self.config.agent.is_empty() {
                "ci-monitor".to_string()
            } else {
                self.config.agent.clone()
            },
            text,
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some("gh_monitor: disabled_config_error".to_string()),
            message_id: Some(format!(
                "gh-monitor-config-error-{}",
                Utc::now().timestamp_millis()
            )),
            unknown_fields: std::collections::HashMap::new(),
        };

        if let Err(e) = ctx.mail.send(team, &lead_agent, &msg) {
            warn!(
                "CI Monitor: failed to send disabled_config_error transition alert to {}@{}: {}",
                lead_agent, team, e
            );
        }
    }

    fn project_disabled_config_error(
        &self,
        ctx: &PluginContext,
        table: Option<&agent_team_mail_core::toml::Table>,
        reason: &str,
    ) {
        let team = Self::team_for_config_error(table, ctx);
        let message = format!("invalid gh_monitor config: {reason}");
        Self::write_health_record(ctx, &team, "disabled_config_error", &message);
        self.notify_disabled_transition(ctx, &team, &message);
    }
}

impl Default for CiMonitorPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for CiMonitorPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "gh_monitor",
            version: "0.1.0",
            description: "Monitor CI/CD pipeline status and notify on failures",
            capabilities: vec![
                Capability::EventListener,
                Capability::AdvertiseMembers,
                Capability::InjectMessages,
            ],
        }
    }

    async fn init(&mut self, ctx: &PluginContext) -> Result<(), PluginError> {
        // Parse config from context
        let config_table = ctx.plugin_config("gh_monitor");
        self.config = if let Some(table) = config_table {
            match CiMonitorConfig::from_toml(table) {
                Ok(config) => config,
                Err(e) => {
                    self.project_disabled_config_error(ctx, config_table, &e.to_string());
                    return Err(e);
                }
            }
        } else {
            CiMonitorConfig::default()
        };

        // If disabled, skip provider setup
        if !self.config.enabled {
            self.ctx = Some(ctx.clone());
            return Ok(());
        }

        // Get repo info for synthetic member registration. Prefer git
        // auto-detection; fall back to config-provided owner/repo when daemon
        // startup lacks repository context.
        let repo = match self.resolve_repo_context(ctx) {
            Ok(repo) => repo,
            Err(err) => {
                self.project_disabled_config_error(ctx, config_table, &err.to_string());
                return Err(err);
            }
        };

        // Resolve report directory relative to repo root when configured as a relative path
        if !self.config.report_dir.is_absolute() {
            self.config.report_dir = repo.path.join(&self.config.report_dir);
        }

        if self.config.runtime_drift_enabled {
            let history_path = Self::runtime_history_file_path(&self.config.report_dir);
            self.runtime_history = Self::load_runtime_history(&history_path);
            self.runtime_history_path = Some(history_path);
        } else {
            self.runtime_history = RuntimeHistory::default();
            self.runtime_history_path = None;
        }

        // Determine ATM config root from canonical home resolution.
        let atm_home = agent_team_mail_core::home::get_home_dir()
            .map_err(|e| PluginError::Init {
                message: format!("Could not determine home directory: {e}"),
                source: None,
            })?
            .join(".config/atm");

        // Build the provider registry
        let registry = self.build_registry(&atm_home);
        debug!(
            "Provider registry initialized with {} providers: {:?}",
            registry.len(),
            registry.list_providers()
        );

        // Create provider if not already injected (for testing)
        if self.provider.is_none() {
            // Create the CI provider from the registry
            // Pass provider_config for external providers
            let provider_config = self.config.provider_config.as_ref();
            self.provider = Some(self.create_provider_from_registry(
                &registry,
                repo.provider.as_ref(),
                provider_config,
            )?);
        }

        // Store registry for potential runtime use
        self.registry = Some(registry);

        // Register synthetic member
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let member = AgentMember {
            agent_id: format!("{}@{}", self.config.agent, self.config.team),
            name: self.config.agent.clone(),
            agent_type: "plugin:gh_monitor".to_string(),
            model: "synthetic".to_string(),
            prompt: None,
            color: Some("blue".to_string()),
            plan_mode_required: None,
            joined_at: now_ms,
            tmux_pane_id: None,
            cwd: repo.path.to_string_lossy().to_string(),
            subscriptions: Vec::new(),
            backend_type: None,
            is_active: Some(true),
            last_active: Some(now_ms),
            session_id: None,
            external_backend_type: None,
            external_model: None,
            unknown_fields: std::collections::HashMap::new(),
        };

        ctx.roster
            .add_member(&self.config.team, member, "gh_monitor")
            .map_err(|e| PluginError::Init {
                message: format!("Failed to register synthetic member: {e}"),
                source: None,
            })?;

        // Validate notify targets exist in team config (warn if not found)
        if !self.config.notify_target.is_empty() {
            let targets: Vec<String> = self
                .config
                .notify_target
                .iter()
                .map(|t| {
                    let team = t.team.as_ref().unwrap_or(&self.config.team);
                    format!("{}@{}", t.agent, team)
                })
                .collect();
            debug!(
                "CI Monitor will route notifications to: {}",
                targets.join(", ")
            );

            // Validate each target exists in its team config
            for target in &self.config.notify_target {
                let target_team = target.team.as_ref().unwrap_or(&self.config.team);
                let team_config_path = ctx.mail.teams_root().join(target_team).join("config.json");

                if team_config_path.exists() {
                    // Try to read team config and check if agent exists
                    match std::fs::read_to_string(&team_config_path) {
                        Ok(content) => {
                            match serde_json::from_str::<agent_team_mail_core::schema::TeamConfig>(
                                &content,
                            ) {
                                Ok(team_config) => {
                                    let agent_exists =
                                        team_config.members.iter().any(|m| m.name == target.agent);
                                    if !agent_exists {
                                        warn!(
                                            "CI Monitor: notify_target '{}@{}' not found in team config. \
                                             Target may join later or may be a typo.",
                                            target.agent, target_team
                                        );
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        "CI Monitor: Failed to parse team config for '{}': {}",
                                        target_team, e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            warn!(
                                "CI Monitor: Failed to read team config for '{}': {}",
                                target_team, e
                            );
                        }
                    }
                } else {
                    warn!(
                        "CI Monitor: Team '{}' config not found at {}. \
                         Team may not exist yet.",
                        target_team,
                        team_config_path.display()
                    );
                }
            }
        }

        // Store context for runtime use
        self.ctx = Some(ctx.clone());

        Ok(())
    }

    async fn run(&mut self, cancel: CancellationToken) -> Result<(), PluginError> {
        // If disabled or no provider, just wait for cancellation
        if !self.config.enabled || self.provider.is_none() {
            cancel.cancelled().await;
            return Ok(());
        }

        #[cfg(not(unix))]
        {
            cancel.cancelled().await;
            return Ok(());
        }

        #[cfg(unix)]
        {
            // Clone context for use in loop (Arc, so cheap)
            let ctx = self
                .ctx
                .as_ref()
                .ok_or_else(|| PluginError::Runtime {
                    message: "Plugin not initialized".to_string(),
                    source: None,
                })?
                .clone();

            let base_interval_secs = self.config.poll_interval_secs.max(10);
            let max_backoff_secs = MAX_ERROR_BACKOFF_SECS.max(base_interval_secs);
            let mut next_delay_secs: u64 = 0;

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        break;
                    }
                    _ = sleep(Duration::from_secs(next_delay_secs)) => {
                        // Evict old dedup cache entries
                        self.evict_old_dedup_entries();
                        // Fetch all completed runs
                        let runs = match self.provider.as_ref() {
                            Some(provider) => list_completed_runs(provider.as_ref()).await,
                            None => {
                                warn!("CI Monitor: Provider disappeared during run");
                                break;
                            }
                        };
                        match runs {
                            Ok(runs) => {
                                next_delay_secs = base_interval_secs;
                                // Process each run
                                for run in runs {
                                    // Filter by branch using glob patterns (client-side)
                                    if !self.matches_branch(&run.head_branch) {
                                        continue;
                                    }

                                    let should_notify_failure = run
                                        .conclusion
                                        .map(|c| self.config.notify_on.contains(&c))
                                        .unwrap_or(false);
                                    let needs_full_run =
                                        should_notify_failure || self.config.runtime_drift_enabled;
                                    if !needs_full_run {
                                        continue;
                                    }

                                    // Fetch full run details with jobs
                                    let full_run_result = match self.provider.as_ref() {
                                        Some(provider) => fetch_run_details(provider.as_ref(), run.id).await,
                                        None => {
                                            warn!("CI Monitor: Provider disappeared during run");
                                            break;
                                        }
                                    };
                                    let full_run = match full_run_result {
                                        Ok(r) => r,
                                        Err(e) => {
                                            warn!("CI Monitor: Failed to fetch run details for #{}: {e}", run.id);
                                            continue;
                                        }
                                    };

                                    // Runtime drift alerts (optional enhancement): update persisted
                                    // baselines and notify on significant slowdowns.
                                    if let Some(drift_msg) =
                                        self.update_runtime_history_and_build_alert(&full_run)
                                    {
                                        if self.send_message_to_targets(&ctx, &drift_msg, run.id) {
                                            debug!("CI Monitor: Runtime drift alert sent for run #{}", run.id);
                                        }
                                    }

                                    if should_notify_failure {
                                        // Generate dedup key
                                        let key = self.dedup_key(&full_run);

                                        // Skip if we've already seen this run+conclusion
                                        if self.seen_runs.contains_key(&key) {
                                            continue;
                                        }

                                        // Command-path terminal notifications (atm gh monitor) are
                                        // authoritative for that run_id. Avoid duplicate alerts
                                        // from polling path when terminal state is already recorded.
                                        if self.was_terminal_notified_by_command_path(&ctx, full_run.id)
                                        {
                                            debug!(
                                                "CI Monitor: Skipping duplicate polling notification for run #{} (command-path terminal state present)",
                                                full_run.id
                                            );
                                            self.seen_runs.insert(key, Utc::now());
                                            continue;
                                        }

                                        // Generate failure reports
                                        if let Err(e) = self.generate_reports(&full_run) {
                                            warn!("CI Monitor: Failed to generate reports for run #{}: {e}", run.id);
                                        }

                                        // Create notification message
                                        let msg = self.run_to_message(&full_run);
                                        if self.send_message_to_targets(&ctx, &msg, run.id) {
                                            debug!("CI Monitor: Notified about run #{}", run.id);
                                            self.seen_runs.insert(key, Utc::now());
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("CI Monitor: Failed to fetch runs: {e}");
                                // Continue polling after error using bounded exponential backoff.
                                next_delay_secs = if next_delay_secs == 0 {
                                    base_interval_secs
                                } else {
                                    next_delay_secs
                                        .saturating_mul(2)
                                        .min(max_backoff_secs)
                                };
                            }
                        }
                    }
                }
            }

            Ok(())
        }
    }

    async fn shutdown(&mut self) -> Result<(), PluginError> {
        if let Some(ctx) = &self.ctx {
            // Clean up synthetic member (soft cleanup - mark inactive)
            if !self.config.team.is_empty() {
                ctx.roster
                    .cleanup_plugin(
                        &self.config.team,
                        "gh_monitor",
                        crate::roster::CleanupMode::Soft,
                    )
                    .map_err(|e| PluginError::Shutdown {
                        message: format!("Failed to cleanup roster: {e}"),
                        source: None,
                    })?;
            }
        }

        Ok(())
    }

    async fn handle_message(&mut self, msg: &InboxMessage) -> Result<(), PluginError> {
        // Ignore messages originating from the synthetic agent to avoid self-loop
        if msg.from == self.config.agent {
            return Ok(());
        }

        // Future: Handle inbox replies to trigger re-checks or acknowledge alerts
        // For now, we just ignore all messages

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = CiMonitorPlugin::new();
        let metadata = plugin.metadata();

        assert_eq!(metadata.name, "gh_monitor");
        assert_eq!(metadata.version, "0.1.0");
        assert!(metadata.description.contains("CI/CD pipeline"));

        assert!(metadata.capabilities.contains(&Capability::EventListener));
        assert!(
            metadata
                .capabilities
                .contains(&Capability::AdvertiseMembers)
        );
        assert!(metadata.capabilities.contains(&Capability::InjectMessages));
    }

    #[test]
    fn test_plugin_default() {
        let plugin = CiMonitorPlugin::default();
        assert!(plugin.provider.is_none());
        assert!(plugin.ctx.is_none());
        assert!(plugin.seen_runs.is_empty());
    }

    #[test]
    fn test_create_provider_from_registry_prefers_git_repo_over_config_repo() {
        let config = CiMonitorConfig {
            owner: Some("config-owner".to_string()),
            repo: Some("config-repo".to_string()),
            ..Default::default()
        };
        let plugin = CiMonitorPlugin::new().with_config(config);
        let registry = CiProviderRegistry::new();
        let git_provider = GitProviderType::GitHub {
            owner: "git-owner".to_string(),
            repo: "git-repo".to_string(),
        };

        let provider = plugin
            .create_provider_from_registry(&registry, Some(&git_provider), None)
            .expect("provider");
        let debug = format!("{provider:?}");
        assert!(debug.contains("git-owner"));
        assert!(debug.contains("git-repo"));
        assert!(!debug.contains("config-owner"));
    }

    #[test]
    fn test_create_provider_from_registry_falls_back_to_config_repo_when_git_missing() {
        let config = CiMonitorConfig {
            owner: Some("config-owner".to_string()),
            repo: Some("config-repo".to_string()),
            ..Default::default()
        };
        let plugin = CiMonitorPlugin::new().with_config(config);
        let registry = CiProviderRegistry::new();

        let provider = plugin
            .create_provider_from_registry(&registry, None, None)
            .expect("provider");
        let debug = format!("{provider:?}");
        assert!(debug.contains("config-owner"));
        assert!(debug.contains("config-repo"));
    }

    #[test]
    fn test_dedup_key_per_commit() {
        use crate::plugins::ci_monitor::{CiRunConclusion, CiRunStatus, create_test_run};
        let plugin = CiMonitorPlugin::new(); // Default uses PerCommit
        let run = create_test_run(
            123456,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );
        let key = plugin.dedup_key(&run);
        assert_eq!(key, "ci-sha123456-Failure");
    }

    #[test]
    fn test_dedup_key_per_run() {
        use crate::plugins::ci_monitor::{CiRunConclusion, CiRunStatus, create_test_run};
        let config = CiMonitorConfig {
            dedup_strategy: DedupStrategy::PerRun,
            ..Default::default()
        };
        let plugin = CiMonitorPlugin::new().with_config(config);
        let run = create_test_run(
            123456,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );
        let key = plugin.dedup_key(&run);
        assert_eq!(key, "ci-123456-Failure");
    }

    #[test]
    fn test_dedup_key_distinct_on_conclusion_change() {
        use crate::plugins::ci_monitor::{CiRunConclusion, CiRunStatus, create_test_run};
        let plugin = CiMonitorPlugin::new();
        let run1 = create_test_run(
            123,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );
        let mut run2 = run1.clone();
        run2.conclusion = Some(CiRunConclusion::TimedOut);

        let key1 = plugin.dedup_key(&run1);
        let key2 = plugin.dedup_key(&run2);

        assert_ne!(key1, key2);
    }

    #[test]
    fn test_evaluate_runtime_drift_min_samples_and_threshold_boundary() {
        // min_samples guard
        assert_eq!(
            CiMonitorPlugin::evaluate_runtime_drift(&[120], 300, 2, 50),
            None
        );

        // threshold boundary is strict (must be greater than threshold, not equal)
        assert_eq!(
            CiMonitorPlugin::evaluate_runtime_drift(&[100, 100], 150, 2, 50),
            None
        );

        // above threshold emits baseline
        assert_eq!(
            CiMonitorPlugin::evaluate_runtime_drift(&[100, 100], 151, 2, 50),
            Some(100)
        );
    }

    #[test]
    fn test_runtime_drift_alert_message_deterministic() {
        use crate::plugins::ci_monitor::{
            CiRunConclusion, CiRunStatus, create_test_job, create_test_run,
        };

        let config = CiMonitorConfig {
            runtime_drift_enabled: true,
            runtime_drift_threshold_percent: 50,
            runtime_drift_min_samples: 1,
            alert_cooldown_secs: 300,
            ..Default::default()
        };

        let mut history = RuntimeHistory::default();
        history.workflow_samples.insert("CI".to_string(), vec![100]);
        history
            .job_samples
            .insert("CI::build".to_string(), vec![100]);

        let mut plugin = CiMonitorPlugin::new()
            .with_config(config)
            .with_runtime_history(history);

        let mut run = create_test_run(
            999,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );
        run.created_at = "2026-02-13T10:00:00Z".to_string();
        run.updated_at = "2026-02-13T10:03:20Z".to_string(); // 200s

        let mut job = create_test_job(
            1001,
            "build",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );
        job.started_at = Some("2026-02-13T10:00:10Z".to_string());
        job.completed_at = Some("2026-02-13T10:03:30Z".to_string()); // 200s
        run.jobs = Some(vec![job]);

        let msg = plugin
            .update_runtime_history_and_build_alert(&run)
            .expect("expected runtime drift alert");
        assert!(msg.text.contains("[runtime-drift:999]"));
        assert!(
            msg.text
                .contains("workflow `CI` current=200s baseline=100s threshold=50%")
        );
        assert!(
            msg.text
                .contains("job `CI::build` current=200s baseline=100s threshold=50%")
        );
    }

    #[test]
    fn test_runtime_drift_alert_respects_alert_cooldown() {
        use crate::plugins::ci_monitor::{CiRunConclusion, CiRunStatus, create_test_run};

        let config = CiMonitorConfig {
            runtime_drift_enabled: true,
            runtime_drift_threshold_percent: 50,
            runtime_drift_min_samples: 1,
            alert_cooldown_secs: 300,
            ..Default::default()
        };

        let mut history = RuntimeHistory::default();
        history.workflow_samples.insert("CI".to_string(), vec![100]);

        let mut plugin = CiMonitorPlugin::new()
            .with_config(config)
            .with_runtime_history(history);

        let mut slow_run_1 = create_test_run(
            1001,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );
        slow_run_1.created_at = "2026-02-13T10:00:00Z".to_string();
        slow_run_1.updated_at = "2026-02-13T10:03:20Z".to_string(); // 200s

        let mut slow_run_2 = create_test_run(
            1002,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );
        slow_run_2.created_at = "2026-02-13T10:04:00Z".to_string();
        slow_run_2.updated_at = "2026-02-13T10:14:00Z".to_string(); // 600s

        let first = plugin.update_runtime_history_and_build_alert(&slow_run_1);
        assert!(first.is_some(), "first slow run should emit drift alert");

        let second = plugin.update_runtime_history_and_build_alert(&slow_run_2);
        assert!(
            second.is_none(),
            "second slow run should be suppressed by alert cooldown"
        );
        assert!(
            plugin
                .runtime_history
                .drift_last_alert_epoch_secs
                .contains_key("workflow::CI"),
            "cooldown state should be persisted by key"
        );
    }

    #[test]
    fn test_matches_branch_no_filter() {
        let plugin = CiMonitorPlugin::new();
        assert!(plugin.matches_branch("main"));
        assert!(plugin.matches_branch("develop"));
        assert!(plugin.matches_branch("any-branch"));
    }

    #[test]
    fn test_matches_branch_exact_match() {
        use globset::GlobSetBuilder;
        let mut builder = GlobSetBuilder::new();
        builder.add(globset::Glob::new("main").unwrap());
        let matcher = builder.build().unwrap();

        let config = CiMonitorConfig {
            branch_matcher: Some(matcher),
            ..Default::default()
        };
        let plugin = CiMonitorPlugin::new().with_config(config);

        assert!(plugin.matches_branch("main"));
        assert!(!plugin.matches_branch("develop"));
    }

    #[test]
    fn test_matches_branch_wildcard() {
        use globset::GlobSetBuilder;
        let mut builder = GlobSetBuilder::new();
        builder.add(globset::Glob::new("release/*").unwrap());
        let matcher = builder.build().unwrap();

        let config = CiMonitorConfig {
            branch_matcher: Some(matcher),
            ..Default::default()
        };
        let plugin = CiMonitorPlugin::new().with_config(config);

        assert!(plugin.matches_branch("release/v1.0"));
        assert!(plugin.matches_branch("release/v2.5"));
        assert!(!plugin.matches_branch("main"));
    }

    #[test]
    fn test_matches_branch_multiple_patterns() {
        use globset::GlobSetBuilder;
        let mut builder = GlobSetBuilder::new();
        builder.add(globset::Glob::new("main").unwrap());
        builder.add(globset::Glob::new("release/*").unwrap());
        let matcher = builder.build().unwrap();

        let config = CiMonitorConfig {
            branch_matcher: Some(matcher),
            ..Default::default()
        };
        let plugin = CiMonitorPlugin::new().with_config(config);

        assert!(plugin.matches_branch("main"));
        assert!(plugin.matches_branch("release/v1.0"));
        assert!(!plugin.matches_branch("develop"));
    }

    // E2E routing and filtering tests
    #[tokio::test]
    async fn test_e2e_branch_filter_and_routing() {
        use crate::plugins::ci_monitor::{MockCiProvider, create_test_job, create_test_run};
        use tempfile::TempDir;

        // Setup: Create temporary teams directory
        let temp_dir = TempDir::new().unwrap();
        let teams_root = temp_dir.path().to_path_buf();

        // Create test runs on different branches
        let run1 = create_test_run(
            1,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );
        let run2 = create_test_run(
            2,
            "CI",
            "release/v1.0",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );
        let run3 = create_test_run(
            3,
            "CI",
            "feature/test",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );

        let jobs = vec![create_test_job(
            101,
            "test-job",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        )];
        let provider = MockCiProvider::with_runs_and_jobs(vec![run1, run2, run3], jobs);

        // Configure with branch filter and routing target
        let toml_str = r#"
team = "dev-team"
watched_branches = ["main", "release/*"]
notify_target = "team-lead"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        // Verify config
        assert!(config.branch_matcher.is_some());
        assert_eq!(config.notify_target.len(), 1);
        assert_eq!(config.notify_target[0].agent, "team-lead");

        // Create plugin with mock provider
        let mut plugin = CiMonitorPlugin::new()
            .with_provider(Box::new(provider))
            .with_config(config.clone());

        // Setup minimal context
        let ctx = create_mock_context(teams_root.clone());
        plugin.ctx = Some(ctx);

        // Simulate one poll cycle by manually processing runs
        let provider_ref = plugin.provider.as_ref().unwrap();
        let filter = CiFilter {
            status: Some(CiRunStatus::Completed),
            per_page: Some(20),
            ..Default::default()
        };

        let runs = provider_ref.list_runs(&filter).await.unwrap();

        // Process runs with branch filtering
        for run in runs {
            if !plugin.matches_branch(&run.head_branch) {
                continue;
            }

            if let Some(conclusion) = run.conclusion {
                if config.notify_on.contains(&conclusion) {
                    let full_run = provider_ref.get_run(run.id).await.unwrap();
                    let key = plugin.dedup_key(&full_run);

                    if !plugin.seen_runs.contains_key(&key) {
                        let msg = plugin.run_to_message(&full_run);

                        // Send to routing target
                        let ctx = plugin.ctx.as_ref().unwrap();
                        for target in &plugin.config.notify_target {
                            let target_team = target.team.as_ref().unwrap_or(&plugin.config.team);
                            ctx.mail.send(target_team, &target.agent, &msg).unwrap();
                        }

                        plugin.seen_runs.insert(key, Utc::now());
                    }
                }
            }
        }

        // Verify: Only main and release/v1.0 should have been processed
        assert_eq!(plugin.seen_runs.len(), 2);

        // Verify: Messages were sent to team-lead
        let mail = crate::plugin::MailService::new(teams_root);
        let inbox = mail.read_inbox("dev-team", "team-lead").unwrap();
        assert_eq!(inbox.len(), 2);
        assert!(inbox[0].text.contains("main"));
        assert!(inbox[1].text.contains("release/v1.0"));
    }

    #[tokio::test]
    async fn test_e2e_branch_filter_excludes_non_matching() {
        use crate::plugins::ci_monitor::{MockCiProvider, create_test_run};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let teams_root = temp_dir.path().to_path_buf();

        // Create runs that should NOT match
        let run1 = create_test_run(
            1,
            "CI",
            "develop",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );
        let run2 = create_test_run(
            2,
            "CI",
            "feature/other",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );

        let provider = MockCiProvider::with_runs(vec![run1, run2]);

        let toml_str = r#"
team = "dev-team"
watched_branches = ["main"]
notify_target = "team-lead"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        let mut plugin = CiMonitorPlugin::new()
            .with_provider(Box::new(provider))
            .with_config(config.clone());

        let ctx = create_mock_context(teams_root.clone());
        plugin.ctx = Some(ctx);

        // Process runs
        let provider_ref = plugin.provider.as_ref().unwrap();
        let filter = CiFilter {
            status: Some(CiRunStatus::Completed),
            per_page: Some(20),
            ..Default::default()
        };

        let runs = provider_ref.list_runs(&filter).await.unwrap();

        for run in runs {
            if !plugin.matches_branch(&run.head_branch) {
                continue;
            }

            if let Some(conclusion) = run.conclusion {
                if config.notify_on.contains(&conclusion) {
                    let full_run = provider_ref.get_run(run.id).await.unwrap();
                    let key = plugin.dedup_key(&full_run);

                    if !plugin.seen_runs.contains_key(&key) {
                        let msg = plugin.run_to_message(&full_run);
                        let ctx = plugin.ctx.as_ref().unwrap();
                        for target in &plugin.config.notify_target {
                            let target_team = target.team.as_ref().unwrap_or(&plugin.config.team);
                            ctx.mail.send(target_team, &target.agent, &msg).unwrap();
                        }
                        plugin.seen_runs.insert(key, Utc::now());
                    }
                }
            }
        }

        // Verify: No runs processed
        assert_eq!(plugin.seen_runs.len(), 0);

        // Verify: No messages sent
        let mail = crate::plugin::MailService::new(teams_root);
        let inbox = mail.read_inbox("dev-team", "team-lead").unwrap();
        assert_eq!(inbox.len(), 0);
    }

    // Helper to create minimal plugin context for testing
    fn create_mock_context(teams_root: PathBuf) -> PluginContext {
        create_mock_context_with_config(teams_root, None)
    }

    // Helper to create mock context with optional plugin config
    fn create_mock_context_with_config(
        teams_root: PathBuf,
        ci_monitor_config: Option<toml::Table>,
    ) -> PluginContext {
        create_mock_context_with_repo_config(teams_root, ci_monitor_config, true)
    }

    fn create_mock_context_with_repo_config(
        teams_root: PathBuf,
        ci_monitor_config: Option<toml::Table>,
        with_repo: bool,
    ) -> PluginContext {
        use crate::plugin::MailService;
        use crate::roster::RosterService;
        use agent_team_mail_core::config::Config;
        use agent_team_mail_core::context::{Platform, RepoContext, SystemContext};
        use std::sync::Arc;

        let mut system = SystemContext::new(
            "test-host".to_string(),
            Platform::Linux,
            teams_root.join(".claude"),
            "2.1.39".to_string(),
            "default-team".to_string(),
        );
        if with_repo {
            let repo = RepoContext::new(
                "test-repo".to_string(),
                std::env::temp_dir().join("test-repo"),
            )
            .with_remote("git@github.com:test/repo.git".to_string());
            system = system.with_repo(repo);
        }

        let mut config = Config::default();
        if let Some(table) = ci_monitor_config {
            config.plugins.insert("gh_monitor".to_string(), table);
        }

        PluginContext {
            system: Arc::new(system),
            mail: Arc::new(MailService::new(teams_root.clone())),
            config: Arc::new(config),
            roster: Arc::new(RosterService::new(teams_root)),
        }
    }

    #[tokio::test]
    async fn test_notify_target_validation_warns_on_missing_agent() {
        use crate::plugins::ci_monitor::MockCiProvider;
        use agent_team_mail_core::schema::{AgentMember, TeamConfig};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let teams_root = temp_dir.path().to_path_buf();
        let team_dir = teams_root.join("dev-team");
        let inboxes_dir = team_dir.join("inboxes");
        std::fs::create_dir_all(&inboxes_dir).unwrap();

        // Create ci-monitor inbox (required for synthetic member registration)
        std::fs::write(inboxes_dir.join("ci-monitor.json"), "[]").unwrap();

        // Create team config with only one member (not the notify target)
        let team_config = TeamConfig {
            name: "dev-team".to_string(),
            description: None,
            created_at: 1234567890,
            lead_agent_id: "lead@dev-team".to_string(),
            lead_session_id: "session-123".to_string(),
            members: vec![AgentMember {
                agent_id: "lead@dev-team".to_string(),
                name: "lead".to_string(),
                agent_type: "general-purpose".to_string(),
                model: "claude-opus-4-6".to_string(),
                prompt: None,
                color: None,
                plan_mode_required: None,
                joined_at: 1234567890,
                tmux_pane_id: None,
                cwd: ".".to_string(),
                subscriptions: Vec::new(),
                backend_type: None,
                is_active: None,
                last_active: None,
                session_id: None,
                external_backend_type: None,
                external_model: None,
                unknown_fields: std::collections::HashMap::new(),
            }],
            unknown_fields: std::collections::HashMap::new(),
        };

        // Write team config
        let config_path = team_dir.join("config.json");
        std::fs::write(&config_path, serde_json::to_string(&team_config).unwrap()).unwrap();

        // Create plugin config table
        let toml_str = r#"
team = "dev-team"
notify_target = "nonexistent-agent"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();

        let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(MockCiProvider::new()));

        let ctx = create_mock_context_with_config(teams_root, Some(table));

        // Init should succeed but log a warning (we can't easily test logging here,
        // but we verify it doesn't fail)
        let result = plugin.init(&ctx).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_init_falls_back_to_config_repo_when_system_repo_missing() {
        use crate::plugins::ci_monitor::MockCiProvider;
        use agent_team_mail_core::schema::{AgentMember, TeamConfig};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let teams_root = temp_dir.path().to_path_buf();
        let team_dir = teams_root.join("dev-team");
        let inboxes_dir = team_dir.join("inboxes");
        std::fs::create_dir_all(&inboxes_dir).unwrap();
        std::fs::write(inboxes_dir.join("ci-monitor.json"), "[]").unwrap();

        let team_config = TeamConfig {
            name: "dev-team".to_string(),
            description: None,
            created_at: 1234567890,
            lead_agent_id: "lead@dev-team".to_string(),
            lead_session_id: "session-123".to_string(),
            members: vec![AgentMember {
                agent_id: "lead@dev-team".to_string(),
                name: "lead".to_string(),
                agent_type: "general-purpose".to_string(),
                model: "claude-opus-4-6".to_string(),
                prompt: None,
                color: None,
                plan_mode_required: None,
                joined_at: 1234567890,
                tmux_pane_id: None,
                cwd: ".".to_string(),
                subscriptions: Vec::new(),
                backend_type: None,
                is_active: None,
                last_active: None,
                session_id: None,
                external_backend_type: None,
                external_model: None,
                unknown_fields: std::collections::HashMap::new(),
            }],
            unknown_fields: std::collections::HashMap::new(),
        };
        std::fs::write(
            team_dir.join("config.json"),
            serde_json::to_string(&team_config).unwrap(),
        )
        .unwrap();

        let toml_str = r#"
team = "dev-team"
repo = "config-owner/config-repo"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let ctx = create_mock_context_with_repo_config(teams_root.clone(), Some(table), false);
        let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(MockCiProvider::new()));
        let atm_config_path = temp_dir.path().join(".atm.toml");
        std::fs::write(&atm_config_path, toml_str).unwrap();

        struct EnvRestoreGuard {
            key: &'static str,
            original: Option<String>,
        }

        impl Drop for EnvRestoreGuard {
            fn drop(&mut self) {
                match self.original.take() {
                    Some(value) => unsafe { std::env::set_var(self.key, value) },
                    None => unsafe { std::env::remove_var(self.key) },
                }
            }
        }

        struct CurrentDirGuard(std::path::PathBuf);

        impl Drop for CurrentDirGuard {
            fn drop(&mut self) {
                std::env::set_current_dir(&self.0).expect("restore current directory");
            }
        }

        let _atm_config_guard = EnvRestoreGuard {
            key: "ATM_CONFIG",
            original: std::env::var("ATM_CONFIG").ok(),
        };
        let _cwd_guard = CurrentDirGuard(std::env::current_dir().unwrap());

        unsafe {
            std::env::set_var("ATM_CONFIG", &atm_config_path);
        }
        std::env::set_current_dir(temp_dir.path()).unwrap();
        let result = plugin.init(&ctx).await;

        assert!(result.is_ok());
        let roster = ctx.roster.list_members("dev-team", None).expect("members");
        let member = roster
            .iter()
            .find(|member| member.name == "ci-monitor")
            .expect("synthetic member registered");
        assert_eq!(member.cwd, temp_dir.path().to_string_lossy());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_init_without_git_or_config_repo_writes_disabled_init_health_record() {
        use crate::plugins::ci_monitor::MockCiProvider;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let teams_root = temp_dir.path().to_path_buf();
        let table: toml::Table = toml::from_str("team = \"dev-team\"").unwrap();
        let ctx = create_mock_context_with_repo_config(teams_root.clone(), Some(table), false);
        let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(MockCiProvider::new()));

        let err = plugin
            .init(&ctx)
            .await
            .expect_err("init should fail without git or config repo");
        assert!(
            err.to_string()
                .contains("No repository information available"),
            "unexpected init error: {err}"
        );

        let health_path =
            agent_team_mail_core::daemon_client::daemon_gh_monitor_health_path_for(temp_dir.path());
        let raw = std::fs::read_to_string(&health_path).expect("health record");
        let health: GhMonitorHealthFile = serde_json::from_str(&raw).expect("health json");
        let record = health
            .records
            .iter()
            .find(|record| record.team == "dev-team")
            .expect("dev-team health record");
        assert_eq!(record.availability_state, "disabled_config_error");
        assert!(
            record
                .message
                .as_deref()
                .is_some_and(|message| message.contains("No repository information available"))
        );
    }

    #[tokio::test]
    async fn test_notify_target_validation_passes_on_existing_agent() {
        use crate::plugins::ci_monitor::MockCiProvider;
        use agent_team_mail_core::schema::{AgentMember, TeamConfig};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let teams_root = temp_dir.path().to_path_buf();
        let team_dir = teams_root.join("dev-team");
        let inboxes_dir = team_dir.join("inboxes");
        std::fs::create_dir_all(&inboxes_dir).unwrap();

        // Create ci-monitor inbox (required for synthetic member registration)
        std::fs::write(inboxes_dir.join("ci-monitor.json"), "[]").unwrap();

        // Create team config with target agent
        let team_config = TeamConfig {
            name: "dev-team".to_string(),
            description: None,
            created_at: 1234567890,
            lead_agent_id: "lead@dev-team".to_string(),
            lead_session_id: "session-123".to_string(),
            members: vec![
                AgentMember {
                    agent_id: "lead@dev-team".to_string(),
                    name: "lead".to_string(),
                    agent_type: "general-purpose".to_string(),
                    model: "claude-opus-4-6".to_string(),
                    prompt: None,
                    color: None,
                    plan_mode_required: None,
                    joined_at: 1234567890,
                    tmux_pane_id: None,
                    cwd: ".".to_string(),
                    subscriptions: Vec::new(),
                    backend_type: None,
                    is_active: None,
                    last_active: None,
                    session_id: None,
                    external_backend_type: None,
                    external_model: None,
                    unknown_fields: std::collections::HashMap::new(),
                },
                AgentMember {
                    agent_id: "team-lead@dev-team".to_string(),
                    name: "team-lead".to_string(),
                    agent_type: "general-purpose".to_string(),
                    model: "claude-opus-4-6".to_string(),
                    prompt: None,
                    color: None,
                    plan_mode_required: None,
                    joined_at: 1234567890,
                    tmux_pane_id: None,
                    cwd: ".".to_string(),
                    subscriptions: Vec::new(),
                    backend_type: None,
                    is_active: None,
                    last_active: None,
                    session_id: None,
                    external_backend_type: None,
                    external_model: None,
                    unknown_fields: std::collections::HashMap::new(),
                },
            ],
            unknown_fields: std::collections::HashMap::new(),
        };

        // Write team config
        let config_path = team_dir.join("config.json");
        std::fs::write(&config_path, serde_json::to_string(&team_config).unwrap()).unwrap();

        // Create plugin config table
        let toml_str = r#"
team = "dev-team"
notify_target = "team-lead"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();

        let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(MockCiProvider::new()));

        let ctx = create_mock_context_with_config(teams_root, Some(table));

        // Init should succeed without warnings
        let result = plugin.init(&ctx).await;
        if let Err(ref e) = result {
            eprintln!("Init failed: {}", e);
        }
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_polling_notification_suppressed_when_command_path_already_terminal() {
        use crate::plugins::ci_monitor::{
            CiRunConclusion, CiRunStatus, MockCiProvider, create_test_run,
        };
        use agent_team_mail_core::schema::{AgentMember, TeamConfig};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let teams_root = temp_dir.path().join("teams");
        let team_dir = teams_root.join("dev-team");
        let inboxes_dir = team_dir.join("inboxes");
        std::fs::create_dir_all(&inboxes_dir).unwrap();
        std::fs::write(inboxes_dir.join("ci-monitor.json"), "[]").unwrap();
        std::fs::write(inboxes_dir.join("team-lead.json"), "[]").unwrap();

        let team_config = TeamConfig {
            name: "dev-team".to_string(),
            description: None,
            created_at: 1234567890,
            lead_agent_id: "team-lead@dev-team".to_string(),
            lead_session_id: "session-123".to_string(),
            members: vec![AgentMember {
                agent_id: "team-lead@dev-team".to_string(),
                name: "team-lead".to_string(),
                agent_type: "general-purpose".to_string(),
                model: "claude-opus-4-6".to_string(),
                prompt: None,
                color: None,
                plan_mode_required: None,
                joined_at: 1234567890,
                tmux_pane_id: None,
                cwd: ".".to_string(),
                subscriptions: Vec::new(),
                backend_type: None,
                is_active: None,
                last_active: None,
                session_id: None,
                external_backend_type: None,
                external_model: None,
                unknown_fields: std::collections::HashMap::new(),
            }],
            unknown_fields: std::collections::HashMap::new(),
        };
        std::fs::write(
            team_dir.join("config.json"),
            serde_json::to_string(&team_config).unwrap(),
        )
        .unwrap();

        let run = create_test_run(
            42,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );
        let provider = MockCiProvider::with_runs(vec![run]);

        let table: toml::Table = toml::from_str(
            r#"
team = "dev-team"
agent = "ci-monitor"
notify_target = "team-lead"
poll_interval_secs = 10
"#,
        )
        .unwrap();
        let ctx = create_mock_context_with_config(teams_root.clone(), Some(table));

        let state_path = CiMonitorPlugin::gh_monitor_state_path(&ctx);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        std::fs::write(
            &state_path,
            r#"{"records":[{"team":"dev-team","state":"failure","run_id":42}]}"#,
        )
        .unwrap();

        let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(provider));
        plugin.init(&ctx).await.unwrap();

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel_clone.cancel();
        });

        plugin.run(cancel).await.unwrap();

        let inbox = ctx.mail.read_inbox("dev-team", "team-lead").unwrap();
        assert!(
            inbox.is_empty(),
            "polling path should suppress duplicate failure notification when command path already recorded terminal run"
        );
    }

    #[test]
    fn test_run_to_message_includes_multi_recipient_note() {
        use crate::plugins::ci_monitor::{CiRunConclusion, CiRunStatus, create_test_run};

        // Create plugin with multiple notify targets
        let toml_str = r#"
team = "dev-team"
notify_target = ["lead", "qa-bot@qa-team"]
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        let plugin = CiMonitorPlugin::new().with_config(config);

        let run = create_test_run(
            123,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );

        let msg = plugin.run_to_message(&run);

        // Verify multi-recipient note is included
        assert!(msg.text.contains("Notified: lead@dev-team, qa-bot@qa-team"));
    }

    #[test]
    fn test_run_to_message_no_multi_recipient_note_for_single_target() {
        use crate::plugins::ci_monitor::{CiRunConclusion, CiRunStatus, create_test_run};

        // Create plugin with single notify target
        let toml_str = r#"
team = "dev-team"
notify_target = "lead"
"#;
        let table: toml::Table = toml::from_str(toml_str).unwrap();
        let config = CiMonitorConfig::from_toml(&table).unwrap();

        let plugin = CiMonitorPlugin::new().with_config(config);

        let run = create_test_run(
            123,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        );

        let msg = plugin.run_to_message(&run);

        // Verify no multi-recipient note for single target
        assert!(!msg.text.contains("Notified:"));
    }
}
