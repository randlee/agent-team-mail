//! Issues plugin implementation

use super::config::IssuesConfig;
use super::github::GitHubProvider;
use super::loader::ProviderLoader;
use super::provider::ErasedIssueProvider;
use super::registry::{ProviderFactory, ProviderRegistry};
use super::types::{Issue, IssueFilter, IssueState};
use crate::plugin::{Capability, Plugin, PluginContext, PluginError, PluginMetadata};
use agent_team_mail_core::context::GitProvider as GitProviderType;
use agent_team_mail_core::schema::{AgentMember, InboxMessage};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Issues plugin — bridges git provider issues to agent team messaging
pub struct IssuesPlugin {
    /// The issue provider (GitHub, etc.)
    provider: Option<Box<dyn ErasedIssueProvider>>,
    /// Plugin configuration from [plugins.issues]
    config: IssuesConfig,
    /// Provider registry for runtime provider selection
    registry: Option<ProviderRegistry>,
    /// Provider loader (kept alive to hold dynamic libraries)
    loader: Option<ProviderLoader>,
    /// Cached context for runtime use
    ctx: Option<PluginContext>,
    /// Tracking: last poll timestamp for incremental fetching
    last_poll: Option<String>,
}

impl IssuesPlugin {
    /// Create a new Issues plugin instance
    pub fn new() -> Self {
        Self {
            provider: None,
            config: IssuesConfig::default(),
            registry: None,
            loader: None,
            ctx: None,
            last_poll: None,
        }
    }

    /// Inject a provider for testing (replaces normal init provider creation)
    ///
    /// NOTE: This is intended for testing only. Production code should use init()
    /// which creates the provider from the PluginContext.
    pub fn with_provider(mut self, provider: Box<dyn ErasedIssueProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Inject config for testing
    ///
    /// NOTE: This is intended for testing only. Production code should use init()
    /// which parses config from the PluginContext.
    pub fn with_config(mut self, config: IssuesConfig) -> Self {
        self.config = config;
        self
    }

    /// Build the provider registry with built-in and external providers
    fn build_registry(&mut self, atm_home: &std::path::Path) -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();

        // Register built-in GitHub provider
        registry.register(ProviderFactory {
            name: "github".to_string(),
            description: "GitHub issue provider (built-in)".to_string(),
            create: Arc::new(|_config| {
                // GitHub provider doesn't need config for construction
                // It will get owner/repo from git context at creation time
                Err(PluginError::Provider {
                    message: "GitHub provider requires owner/repo from git context".to_string(),
                    source: None,
                })
            }),
        });

        // Load external providers from provider directory
        let provider_dir = atm_home.join("providers");
        let mut loader = ProviderLoader::new();
        match loader.load_from_directory(&provider_dir) {
            Ok(factories) => {
                debug!("Loaded {} external providers", factories.len());
                for factory in factories {
                    registry.register(factory);
                }
            }
            Err(e) => {
                warn!("Failed to load external providers: {}", e);
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
        registry: &ProviderRegistry,
        git_provider: &GitProviderType,
        config_table: Option<&toml::Table>,
    ) -> Result<Box<dyn ErasedIssueProvider>, PluginError> {
        // If config specifies a provider, use it
        if let Some(provider_name) = &self.config.provider {
            debug!("Using configured provider: {}", provider_name);

            // Special case: GitHub built-in needs owner/repo from git context
            if provider_name == "github" {
                if let GitProviderType::GitHub { owner, repo } = git_provider {
                    return Ok(Box::new(GitHubProvider::new(owner.clone(), repo.clone())));
                }
                return Err(PluginError::Provider {
                    message: "Configured provider 'github' but git remote is not GitHub".to_string(),
                    source: None,
                });
            }

            // Try to create from registry
            return registry.create_provider(provider_name, config_table);
        }

        // Auto-detect provider from git remote
        match git_provider {
            GitProviderType::GitHub { owner, repo } => {
                debug!("Auto-detected GitHub provider from git remote");
                Ok(Box::new(GitHubProvider::new(owner.clone(), repo.clone())))
            }
            GitProviderType::AzureDevOps { org, project, repo } => {
                // Try to find azure-devops provider in registry
                if registry.has_provider("azure-devops") {
                    debug!("Using azure-devops provider from registry");
                    registry.create_provider("azure-devops", config_table)
                } else {
                    Err(PluginError::Provider {
                        message: format!(
                            "Azure DevOps provider not found in registry (org: {org}, project: {project}, repo: {repo})"
                        ),
                        source: None,
                    })
                }
            }
            GitProviderType::GitLab { namespace, repo } => {
                if registry.has_provider("gitlab") {
                    debug!("Using gitlab provider from registry");
                    registry.create_provider("gitlab", config_table)
                } else {
                    Err(PluginError::Provider {
                        message: format!(
                            "GitLab provider not found in registry (namespace: {namespace}, repo: {repo})"
                        ),
                        source: None,
                    })
                }
            }
            GitProviderType::Bitbucket { workspace, repo } => {
                if registry.has_provider("bitbucket") {
                    debug!("Using bitbucket provider from registry");
                    registry.create_provider("bitbucket", config_table)
                } else {
                    Err(PluginError::Provider {
                        message: format!(
                            "Bitbucket provider not found in registry (workspace: {workspace}, repo: {repo})"
                        ),
                        source: None,
                    })
                }
            }
            GitProviderType::Unknown { host } => Err(PluginError::Provider {
                message: format!("No issue provider for unknown git host: {host}"),
                source: None,
            }),
        }
    }

    /// Transform an Issue into an InboxMessage for delivery
    fn issue_to_message(&self, issue: &Issue) -> InboxMessage {
        let state_display = match issue.state {
            IssueState::Open => "Open",
            IssueState::Closed => "Closed",
        };

        let labels_str = issue
            .labels
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        let content = format!(
            "[issue:{}] {} — {}\n\n{}\n\nLabels: {}\nURL: {}",
            issue.number,
            issue.title,
            state_display,
            issue.body.as_deref().unwrap_or("(no description)"),
            if labels_str.is_empty() {
                "(none)"
            } else {
                &labels_str
            },
            issue.url,
        );

        let message_id = if issue.updated_at.is_empty() {
            format!("issue-{}", issue.number)
        } else {
            format!("issue-{}-{}", issue.number, issue.updated_at)
        };

        InboxMessage {
            from: self.config.agent.clone(),
            text: content,
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(format!("Issue #{}: {}", issue.number, issue.title)),
            message_id: Some(message_id),
            unknown_fields: HashMap::new(),
        }
    }

    /// Parse issue number from message content
    ///
    /// Looks for `[issue:NUMBER]` prefix in the message text
    fn parse_issue_reference(text: &str) -> Option<u64> {
        // Look for [issue:123] pattern at the start of the text
        let text = text.trim();
        if !text.starts_with("[issue:") {
            return None;
        }

        // Extract the number between [issue: and ]
        let end_idx = text.find(']')?;
        let issue_part = &text[7..end_idx]; // Skip "[issue:"
        issue_part.parse::<u64>().ok()
    }
}

impl Default for IssuesPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for IssuesPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "issues",
            version: "0.1.0",
            description: "Bridge between git provider issues and agent team messaging",
            capabilities: vec![
                Capability::IssueTracking,
                Capability::AdvertiseMembers,
                Capability::EventListener,
                Capability::InjectMessages,
            ],
        }
    }

    async fn init(&mut self, ctx: &PluginContext) -> Result<(), PluginError> {
        // Parse config from context
        let config_table = ctx.plugin_config("issues");
        self.config = if let Some(table) = config_table {
            IssuesConfig::from_toml(table)?
        } else {
            IssuesConfig::default()
        };

        // If disabled, skip provider setup
        if !self.config.enabled {
            self.ctx = Some(ctx.clone());
            return Ok(());
        }

        // Get repo info for synthetic member registration (needed even if provider injected)
        let repo = ctx.system.repo.as_ref().ok_or_else(|| PluginError::Init {
            message: "No repository information available".to_string(),
            source: None,
        })?;

        // Determine ATM home directory
        // When ATM_HOME is set, use it directly (test-friendly)
        let atm_home = if let Ok(atm_home_env) = std::env::var("ATM_HOME") {
            PathBuf::from(atm_home_env)
        } else {
            agent_team_mail_core::home::get_home_dir()
                .map_err(|e| PluginError::Init {
                    message: format!("Could not determine home directory: {e}"),
                    source: None,
                })?
                .join(".config/atm")
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

            // Create the issue provider from the registry
            self.provider = Some(self.create_provider_from_registry(&registry, git_provider, config_table)?);
        }

        // Store registry for potential runtime use
        self.registry = Some(registry);

        // Determine target team (use default_team from config if not specified)
        let target_team = if self.config.team.is_empty() {
            ctx.config.core.default_team.clone()
        } else {
            self.config.team.clone()
        };

        // Register synthetic member
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let member = AgentMember {
            agent_id: format!("{}@{}", self.config.agent, target_team),
            name: self.config.agent.clone(),
            agent_type: "plugin:issues".to_string(),
            model: "synthetic".to_string(),
            prompt: None,
            color: Some("purple".to_string()),
            plan_mode_required: None,
            joined_at: now_ms,
            tmux_pane_id: None,
            cwd: repo
                .path
                .to_string_lossy()
                .to_string(),
            subscriptions: Vec::new(),
            backend_type: None,
            is_active: Some(true),
            last_active: Some(now_ms),
            unknown_fields: HashMap::new(),
        };

        ctx.roster
            .add_member(&target_team, member, "issues")
            .map_err(|e| PluginError::Init {
                message: format!("Failed to register synthetic member: {e}"),
                source: None,
            })?;

        // Update config with resolved team name
        self.config.team = target_team;

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

        let mut ticker = interval(Duration::from_secs(self.config.poll_interval));

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    break;
                }
                _ = ticker.tick() => {
                    // Build filter from config
                    let filter = IssueFilter {
                        labels: self.config.labels.clone(),
                        assignees: self.config.assignees.clone(),
                        state: Some(IssueState::Open),
                        since: self.last_poll.clone(),
                    };

                    // Fetch issues
                    match provider.list_issues(&filter).await {
                        Ok(issues) => {
                            // Process each new issue
                            for issue in issues {
                                let msg = self.issue_to_message(&issue);
                                if let Err(e) = ctx.mail.send(&self.config.team, &self.config.agent, &msg) {
                                    warn!("Issues plugin: Failed to send message for issue #{}: {e}", issue.number);
                                }
                            }

                            // Update last poll timestamp
                            self.last_poll = Some(chrono::Utc::now().to_rfc3339());
                        }
                        Err(e) => {
                            warn!("Issues plugin: Failed to fetch issues: {e}");
                            // Continue polling after error
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
                    .cleanup_plugin(&self.config.team, "issues", crate::roster::CleanupMode::Soft)
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

        // Check if the message is a reply to an issue (has [issue:NUMBER] prefix)
        if let Some(issue_number) = Self::parse_issue_reference(&msg.text)
            && let Some(provider) = &self.provider
        {
            // Extract the reply body (everything after the [issue:NUMBER] line)
            let reply_body = msg
                .text
                .lines()
                .skip(1) // Skip the [issue:NUMBER] line
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();

            if !reply_body.is_empty() {
                match provider.add_comment(issue_number, &reply_body).await {
                    Ok(_) => {
                        // Successfully posted comment
                    }
                    Err(e) => {
                        warn!(
                            "Issues plugin: Failed to post comment on issue #{issue_number}: {e}"
                        );
                    }
                }
            }
        }

        // Not a reply or no [issue:NUMBER] prefix - ignore
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = IssuesPlugin::new();
        let metadata = plugin.metadata();

        assert_eq!(metadata.name, "issues");
        assert_eq!(metadata.version, "0.1.0");
        assert!(metadata
            .description
            .contains("git provider issues and agent team messaging"));

        assert!(metadata.capabilities.contains(&Capability::IssueTracking));
        assert!(metadata
            .capabilities
            .contains(&Capability::AdvertiseMembers));
        assert!(metadata.capabilities.contains(&Capability::EventListener));
        assert!(metadata.capabilities.contains(&Capability::InjectMessages));
    }

    #[test]
    fn test_parse_issue_reference() {
        assert_eq!(IssuesPlugin::parse_issue_reference("[issue:123] Fix bug"), Some(123));
        assert_eq!(
            IssuesPlugin::parse_issue_reference("[issue:456] Another issue\nWith body"),
            Some(456)
        );
        assert_eq!(IssuesPlugin::parse_issue_reference("No issue here"), None);
        assert_eq!(IssuesPlugin::parse_issue_reference("[task:123] Not an issue"), None);
        assert_eq!(IssuesPlugin::parse_issue_reference("[issue:abc] Invalid number"), None);
        assert_eq!(IssuesPlugin::parse_issue_reference("  [issue:789]  "), Some(789));
    }

    #[test]
    fn test_issue_to_message_formatting() {
        let plugin = IssuesPlugin::new();

        let issue = Issue {
            id: "123".to_string(),
            number: 42,
            title: "Test issue".to_string(),
            body: Some("This is the issue body".to_string()),
            state: IssueState::Open,
            labels: vec![
                super::super::types::IssueLabel {
                    name: "bug".to_string(),
                    color: Some("ff0000".to_string()),
                },
                super::super::types::IssueLabel {
                    name: "urgent".to_string(),
                    color: None,
                },
            ],
            assignees: vec!["alice".to_string()],
            author: "bob".to_string(),
            created_at: "2026-02-11T10:00:00Z".to_string(),
            updated_at: "2026-02-11T12:00:00Z".to_string(),
            url: "https://github.com/owner/repo/issues/42".to_string(),
        };

        let msg = plugin.issue_to_message(&issue);

        assert_eq!(msg.from, "issues-bot");
        assert!(msg.text.contains("[issue:42]"));
        assert!(msg.text.contains("Test issue"));
        assert!(msg.text.contains("Open"));
        assert!(msg.text.contains("This is the issue body"));
        assert!(msg.text.contains("bug, urgent"));
        assert!(msg.text.contains("https://github.com/owner/repo/issues/42"));
        assert!(!msg.read);
        assert_eq!(msg.summary, Some("Issue #42: Test issue".to_string()));
        assert!(msg
            .message_id
            .as_deref()
            .unwrap_or("")
            .starts_with("issue-42-"));
    }

    #[test]
    fn test_issue_to_message_minimal() {
        let plugin = IssuesPlugin::new();

        let issue = Issue {
            id: "789".to_string(),
            number: 10,
            title: "Minimal issue".to_string(),
            body: None,
            state: IssueState::Closed,
            labels: Vec::new(),
            assignees: Vec::new(),
            author: "charlie".to_string(),
            created_at: "2026-02-10T08:00:00Z".to_string(),
            updated_at: "2026-02-11T09:00:00Z".to_string(),
            url: "https://example.com/issues/10".to_string(),
        };

        let msg = plugin.issue_to_message(&issue);

        assert!(msg.text.contains("[issue:10]"));
        assert!(msg.text.contains("Minimal issue"));
        assert!(msg.text.contains("Closed"));
        assert!(msg.text.contains("(no description)"));
        assert!(msg.text.contains("(none)")); // No labels
    }

    #[test]
    fn test_issue_message_id_empty_updated_at() {
        let plugin = IssuesPlugin::new();

        let issue = Issue {
            id: "100".to_string(),
            number: 100,
            title: "No updated_at".to_string(),
            body: None,
            state: IssueState::Open,
            labels: Vec::new(),
            assignees: Vec::new(),
            author: "tester".to_string(),
            created_at: "2026-02-11T10:00:00Z".to_string(),
            updated_at: "".to_string(),
            url: "https://example.com/issues/100".to_string(),
        };

        let msg = plugin.issue_to_message(&issue);
        assert_eq!(msg.message_id, Some("issue-100".to_string()));
    }

    #[test]
    fn test_issue_update_generates_distinct_message_ids() {
        use agent_team_mail_core::io::inbox::inbox_append;
        use tempfile::TempDir;

        let plugin = IssuesPlugin::new();
        let temp_dir = TempDir::new().unwrap();
        let inbox_path = temp_dir.path().join("agent.json");

        let issue_v1 = Issue {
            id: "200".to_string(),
            number: 200,
            title: "Issue update".to_string(),
            body: Some("First version".to_string()),
            state: IssueState::Open,
            labels: Vec::new(),
            assignees: Vec::new(),
            author: "tester".to_string(),
            created_at: "2026-02-11T10:00:00Z".to_string(),
            updated_at: "2026-02-11T12:00:00Z".to_string(),
            url: "https://example.com/issues/200".to_string(),
        };

        let issue_v2 = Issue {
            updated_at: "2026-02-11T12:30:00Z".to_string(),
            body: Some("Second version".to_string()),
            ..issue_v1.clone()
        };

        let msg1 = plugin.issue_to_message(&issue_v1);
        let msg2 = plugin.issue_to_message(&issue_v2);

        assert_ne!(msg1.message_id, msg2.message_id);

        inbox_append(&inbox_path, &msg1, "test-team", "issues-bot").unwrap();
        inbox_append(&inbox_path, &msg2, "test-team", "issues-bot").unwrap();

        let content = std::fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<InboxMessage> = serde_json::from_str(&content).unwrap();
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_plugin_default() {
        let plugin = IssuesPlugin::default();
        assert!(plugin.provider.is_none());
        assert!(plugin.ctx.is_none());
        assert!(plugin.last_poll.is_none());
    }

    #[test]
    fn test_build_registry_keeps_loader_alive() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut plugin = IssuesPlugin::new();

        let _registry = plugin.build_registry(temp_dir.path());
        assert!(plugin.loader.is_some());
    }

    #[tokio::test]
    async fn test_handle_message_ignores_self_messages() {
        use crate::plugins::issues::mock_provider::{MockCall, MockProvider};

        let provider = MockProvider::new();
        let provider_clone = provider.clone();

        let mut plugin = IssuesPlugin::new()
            .with_provider(Box::new(provider))
            .with_config(IssuesConfig {
                agent: "issues-bot".to_string(),
                ..IssuesConfig::default()
            });

        let msg = InboxMessage {
            from: "issues-bot".to_string(),
            text: "[issue:42]\nThis should be ignored".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: None,
            message_id: None,
            unknown_fields: HashMap::new(),
        };

        plugin.handle_message(&msg).await.unwrap();

        // No AddComment should be called
        let calls = provider_clone.get_calls();
        assert!(calls
            .iter()
            .all(|c| !matches!(c, MockCall::AddComment { .. })));
    }
}
