//! CI Monitor plugin implementation

use super::config::{CiMonitorConfig, DedupStrategy};
use super::github::GitHubActionsProvider;
use super::loader::CiProviderLoader;
use super::provider::ErasedCiProvider;
use super::registry::{CiProviderFactory, CiProviderRegistry};
use super::types::{CiFilter, CiRunConclusion, CiRunStatus};
use crate::plugin::{Capability, Plugin, PluginContext, PluginError, PluginMetadata};
use atm_core::context::GitProvider as GitProviderType;
use atm_core::schema::{AgentMember, InboxMessage};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// CI Monitor plugin — bridges CI provider runs to agent team messaging
pub struct CiMonitorPlugin {
    /// The CI provider (GitHub Actions, Azure Pipelines, etc.)
    provider: Option<Box<dyn ErasedCiProvider>>,
    /// Plugin configuration from [plugins.ci_monitor]
    config: CiMonitorConfig,
    /// Provider registry for runtime provider selection
    registry: Option<CiProviderRegistry>,
    /// Provider loader (kept alive to hold dynamic libraries)
    loader: Option<CiProviderLoader>,
    /// Cached context for runtime use
    ctx: Option<PluginContext>,
    /// Tracking: seen run dedup keys with their timestamps
    seen_runs: HashMap<String, DateTime<Utc>>,
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
        git_provider: &GitProviderType,
        config_table: Option<&toml::Table>,
    ) -> Result<Box<dyn ErasedCiProvider>, PluginError> {
        // If config specifies owner/repo, use those; otherwise auto-detect from git
        let (owner, repo) = if let (Some(owner), Some(repo)) = (&self.config.owner, &self.config.repo) {
            (owner.clone(), repo.clone())
        } else {
            // Auto-detect from git remote
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
                        message: format!("GitLab not yet supported (namespace: {namespace}, repo: {repo})"),
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
            message: format!("Failed to create report directory: {}", self.config.report_dir.display()),
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
            run.id, conclusion_display, conclusion_display, run.head_branch, run.head_sha, run.url, failed_jobs
        );

        std::fs::write(&md_path, md_content).map_err(|e| PluginError::Runtime {
            message: format!("Failed to write Markdown report: {}", md_path.display()),
            source: Some(Box::new(e)),
        })?;

        debug!("Generated reports for run #{}: {} and {}", run.id, json_path.display(), md_path.display());
        Ok(())
    }

    /// Transform a CI failure into an InboxMessage for delivery
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

        let content = format!(
            "[ci:{}] CI {} on {}: {}\nCommit: {}\nFailed jobs: {}\nURL: {}",
            run.id,
            conclusion_display,
            run.head_branch,
            run.name,
            run.head_sha,
            failed_jobs,
            run.url
        );

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
}

impl Default for CiMonitorPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for CiMonitorPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "ci_monitor",
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
        let config_table = ctx.plugin_config("ci_monitor");
        self.config = if let Some(table) = config_table {
            CiMonitorConfig::from_toml(table)?
        } else {
            CiMonitorConfig::default()
        };

        // If disabled, skip provider setup
        if !self.config.enabled {
            self.ctx = Some(ctx.clone());
            return Ok(());
        }

        // Get repo info for synthetic member registration
        let repo = ctx.system.repo.as_ref().ok_or_else(|| PluginError::Init {
            message: "No repository information available".to_string(),
            source: None,
        })?;

        // Determine ATM home directory
        let atm_home = if let Ok(atm_home_env) = std::env::var("ATM_HOME") {
            PathBuf::from(atm_home_env)
        } else {
            dirs::config_dir()
                .ok_or_else(|| PluginError::Init {
                    message: "Could not determine config directory".to_string(),
                    source: None,
                })?
                .join("atm")
        };

        // Build the provider registry
        let registry = self.build_registry(&atm_home);
        debug!(
            "Provider registry initialized with {} providers: {:?}",
            registry.len(),
            registry.list_providers()
        );

        // Create provider if not already injected (for testing)
        if self.provider.is_none() {
            let git_provider = repo.provider.as_ref().ok_or_else(|| PluginError::Init {
                message: "No git provider configured".to_string(),
                source: None,
            })?;

            // Create the CI provider from the registry
            // Pass provider_config for external providers
            let provider_config = self.config.provider_config.as_ref();
            self.provider =
                Some(self.create_provider_from_registry(&registry, git_provider, provider_config)?);
        }

        // Store registry for potential runtime use
        self.registry = Some(registry);

        // Register synthetic member
        let member = AgentMember {
            agent_id: format!("{}@{}", self.config.agent, self.config.team),
            name: self.config.agent.clone(),
            agent_type: "plugin:ci_monitor".to_string(),
            model: "synthetic".to_string(),
            prompt: None,
            color: Some("blue".to_string()),
            plan_mode_required: None,
            joined_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            tmux_pane_id: None,
            cwd: repo.path.to_string_lossy().to_string(),
            subscriptions: Vec::new(),
            backend_type: None,
            is_active: Some(true),
            unknown_fields: std::collections::HashMap::new(),
        };

        ctx.roster
            .add_member(&self.config.team, member, "ci_monitor")
            .map_err(|e| PluginError::Init {
                message: format!("Failed to register synthetic member: {e}"),
                source: None,
            })?;

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

        // Clone context for use in loop (Arc, so cheap)
        let ctx = self.ctx.as_ref().ok_or_else(|| PluginError::Runtime {
            message: "Plugin not initialized".to_string(),
            source: None,
        })?.clone();

        let mut ticker = interval(Duration::from_secs(self.config.poll_interval_secs));

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    break;
                }
                _ = ticker.tick() => {
                    // Evict old dedup cache entries
                    self.evict_old_dedup_entries();

                    // Get provider reference for this iteration
                    let provider = match self.provider.as_ref() {
                        Some(p) => p,
                        None => {
                            warn!("CI Monitor: Provider disappeared during run");
                            break;
                        }
                    };

                    // Build filter from config
                    let mut filter = CiFilter {
                        status: Some(CiRunStatus::Completed),
                        per_page: Some(20),
                        ..Default::default()
                    };

                    // If watched_branches is specified, check each branch
                    let branches = if self.config.watched_branches.is_empty() {
                        vec![None]
                    } else {
                        self.config.watched_branches.iter().map(|b| Some(b.clone())).collect()
                    };

                    for branch in branches {
                        filter.branch = branch;

                        // Fetch runs
                        match provider.list_runs(&filter).await {
                            Ok(runs) => {
                                // Process each run
                                for run in runs {
                                    // Check if this run matches our notification criteria
                                    if let Some(conclusion) = run.conclusion
                                        && self.config.notify_on.contains(&conclusion)
                                    {
                                        // Fetch full run details with jobs
                                        let full_run = match provider.get_run(run.id).await {
                                            Ok(r) => r,
                                            Err(e) => {
                                                warn!("CI Monitor: Failed to fetch run details for #{}: {e}", run.id);
                                                continue;
                                            }
                                        };

                                        // Generate dedup key
                                        let key = self.dedup_key(&full_run);

                                        // Skip if we've already seen this run+conclusion
                                        if self.seen_runs.contains_key(&key) {
                                            continue;
                                        }

                                        // Generate failure reports
                                        if let Err(e) = self.generate_reports(&full_run) {
                                            warn!("CI Monitor: Failed to generate reports for run #{}: {e}", run.id);
                                        }

                                        // Create and send notification
                                        let msg = self.run_to_message(&full_run);
                                        if let Err(e) = ctx.mail.send(&self.config.team, &self.config.agent, &msg) {
                                            warn!("CI Monitor: Failed to send message for run #{}: {e}", run.id);
                                        } else {
                                            debug!("CI Monitor: Notified about run #{} ({:?})", run.id, conclusion);
                                            self.seen_runs.insert(key, Utc::now());
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("CI Monitor: Failed to fetch runs: {e}");
                                // Continue polling after error
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), PluginError> {
        if let Some(ctx) = &self.ctx {
            // Clean up synthetic member (soft cleanup - mark inactive)
            if !self.config.team.is_empty() {
                ctx.roster
                    .cleanup_plugin(
                        &self.config.team,
                        "ci_monitor",
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

        assert_eq!(metadata.name, "ci_monitor");
        assert_eq!(metadata.version, "0.1.0");
        assert!(metadata.description.contains("CI/CD pipeline"));

        assert!(metadata.capabilities.contains(&Capability::EventListener));
        assert!(metadata
            .capabilities
            .contains(&Capability::AdvertiseMembers));
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
    fn test_dedup_key_per_commit() {
        use crate::plugins::ci_monitor::{create_test_run, CiRunConclusion, CiRunStatus};
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
        use crate::plugins::ci_monitor::{create_test_run, CiRunConclusion, CiRunStatus};
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
        use crate::plugins::ci_monitor::{create_test_run, CiRunConclusion, CiRunStatus};
        let plugin = CiMonitorPlugin::new();
        let mut run1 = create_test_run(
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
}
