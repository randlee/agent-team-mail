//! CI Monitor plugin implementation

use super::config::CiMonitorConfig;
use super::github::GitHubActionsProvider;
use super::provider::ErasedCiProvider;
use super::registry::{CiProviderFactory, CiProviderRegistry};
use super::types::{CiFilter, CiRunConclusion, CiRunStatus};
use crate::plugin::{Capability, Plugin, PluginContext, PluginError, PluginMetadata};
use atm_core::context::GitProvider as GitProviderType;
use atm_core::schema::{AgentMember, InboxMessage};
use std::collections::HashSet;
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
    /// Cached context for runtime use
    ctx: Option<PluginContext>,
    /// Tracking: seen run IDs with their conclusions for deduplication
    seen_runs: HashSet<String>,
}

impl CiMonitorPlugin {
    /// Create a new CI Monitor plugin instance
    pub fn new() -> Self {
        Self {
            provider: None,
            config: CiMonitorConfig::default(),
            registry: None,
            ctx: None,
            seen_runs: HashSet::new(),
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
    fn build_registry(&mut self, _atm_home: &std::path::Path) -> CiProviderRegistry {
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

        // TODO: Load external providers from provider directory
        // For now, we only support built-in GitHub provider

        // Load config-specified provider libraries
        if !self.config.provider_libraries.is_empty() {
            let _paths: Vec<PathBuf> = self.config.provider_libraries.values().cloned().collect();
            // TODO: Implement dynamic library loading for external CI providers
            // This will be similar to the Issues plugin's ProviderLoader pattern
        }

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

    /// Generate a deduplication key for a run
    ///
    /// Format: "{run_id}-{conclusion}-{updated_at}"
    /// This ensures we only notify once per run status transition
    fn dedup_key(run_id: u64, conclusion: Option<CiRunConclusion>, updated_at: &str) -> String {
        format!(
            "{}-{}-{}",
            run_id,
            conclusion.map(|c| format!("{c:?}")).unwrap_or_else(|| "InProgress".to_string()),
            updated_at
        )
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

        let message_id = Self::dedup_key(run.id, run.conclusion, &run.updated_at);

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
            self.provider =
                Some(self.create_provider_from_registry(&registry, git_provider, config_table)?);
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

        let provider = self.provider.as_ref().unwrap();
        let ctx = self.ctx.as_ref().ok_or_else(|| PluginError::Runtime {
            message: "Plugin not initialized".to_string(),
            source: None,
        })?;

        let mut ticker = interval(Duration::from_secs(self.config.poll_interval_secs));

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    break;
                }
                _ = ticker.tick() => {
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
                                    if let Some(conclusion) = run.conclusion {
                                        if !self.config.notify_on.contains(&conclusion) {
                                            continue;
                                        }

                                        // Generate dedup key
                                        let key = Self::dedup_key(run.id, Some(conclusion), &run.updated_at);

                                        // Skip if we've already seen this run+conclusion
                                        if self.seen_runs.contains(&key) {
                                            continue;
                                        }

                                        // Fetch full run details with jobs
                                        let full_run = match provider.get_run(run.id).await {
                                            Ok(r) => r,
                                            Err(e) => {
                                                warn!("CI Monitor: Failed to fetch run details for #{}: {e}", run.id);
                                                continue;
                                            }
                                        };

                                        // Create and send notification
                                        let msg = self.run_to_message(&full_run);
                                        if let Err(e) = ctx.mail.send(&self.config.team, &self.config.agent, &msg) {
                                            warn!("CI Monitor: Failed to send message for run #{}: {e}", run.id);
                                        } else {
                                            debug!("CI Monitor: Notified about run #{} ({:?})", run.id, conclusion);
                                            self.seen_runs.insert(key);
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
    fn test_dedup_key_format() {
        let key1 = CiMonitorPlugin::dedup_key(
            123456,
            Some(CiRunConclusion::Failure),
            "2026-02-13T10:00:00Z",
        );
        assert_eq!(key1, "123456-Failure-2026-02-13T10:00:00Z");

        let key2 = CiMonitorPlugin::dedup_key(123456, None, "2026-02-13T10:00:00Z");
        assert_eq!(key2, "123456-InProgress-2026-02-13T10:00:00Z");
    }

    #[test]
    fn test_dedup_key_distinct_on_conclusion_change() {
        let key1 = CiMonitorPlugin::dedup_key(
            123,
            Some(CiRunConclusion::Failure),
            "2026-02-13T10:00:00Z",
        );
        let key2 = CiMonitorPlugin::dedup_key(123, None, "2026-02-13T10:00:00Z");

        assert_ne!(key1, key2);
    }

    #[test]
    fn test_dedup_key_distinct_on_updated_at() {
        let key1 = CiMonitorPlugin::dedup_key(
            123,
            Some(CiRunConclusion::Failure),
            "2026-02-13T10:00:00Z",
        );
        let key2 = CiMonitorPlugin::dedup_key(
            123,
            Some(CiRunConclusion::Failure),
            "2026-02-13T10:30:00Z",
        );

        assert_ne!(key1, key2);
    }
}
