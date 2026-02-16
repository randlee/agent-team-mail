//! Issues plugin — provider abstraction for issue tracking

mod config;
mod github;
mod loader;
mod mock_provider;
mod plugin;
mod provider;
mod registry;
mod types;

pub use config::IssuesConfig;
pub use github::GitHubProvider;
pub use loader::ProviderLoader;
pub use mock_provider::{MockCall, MockProvider};
pub use plugin::IssuesPlugin;
pub use provider::{ErasedIssueProvider, IssueProvider};
pub use registry::{ProviderFactory, ProviderRegistry};
pub use types::{Issue, IssueComment, IssueFilter, IssueLabel, IssueState};

use agent_team_mail_core::context::GitProvider;
use crate::plugin::PluginError;

/// Create an issue provider for the given git provider (legacy function)
///
/// This function provides backward compatibility. New code should use `ProviderRegistry` instead.
///
/// Returns a boxed trait object that implements ErasedIssueProvider.
///
/// # Arguments
///
/// * `provider` - The git provider (GitHub, etc.)
/// * `_config` - Optional plugin config (reserved for future use)
///
/// # Errors
///
/// Returns `PluginError::Provider` if the git provider doesn't support issue tracking.
pub fn create_provider(
    provider: &GitProvider,
    _config: Option<&toml::Table>,
) -> Result<Box<dyn ErasedIssueProvider>, PluginError> {
    match provider {
        GitProvider::GitHub { owner, repo } => Ok(Box::new(GitHubProvider::new(
            owner.clone(),
            repo.clone(),
        ))),
        GitProvider::AzureDevOps { org, project, repo } => Err(PluginError::Provider {
            message: format!(
                "Azure DevOps provider moved to external plugin (org: {org}, project: {project}, repo: {repo})"
            ),
            source: None,
        }),
        GitProvider::GitLab { namespace, repo } => Err(PluginError::Provider {
            message: format!("GitLab issue provider not yet implemented (namespace: {namespace}, repo: {repo})"),
            source: None,
        }),
        GitProvider::Bitbucket { workspace, repo } => Err(PluginError::Provider {
            message: format!("Bitbucket issue provider not yet implemented (workspace: {workspace}, repo: {repo})"),
            source: None,
        }),
        GitProvider::Unknown { host } => Err(PluginError::Provider {
            message: format!("No issue provider for unknown git host: {host}"),
            source: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_provider_github() {
        let provider = GitProvider::GitHub {
            owner: "owner".to_string(),
            repo: "repo".to_string(),
        };
        let result = create_provider(&provider, None);
        assert!(result.is_ok());
        let provider = result.unwrap();
        assert_eq!(provider.provider_name(), "GitHub");
    }

    #[test]
    fn test_create_provider_azure_devops_moved_to_external() {
        let provider = GitProvider::AzureDevOps {
            org: "org".to_string(),
            project: "project".to_string(),
            repo: "repo".to_string(),
        };
        let result = create_provider(&provider, None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("moved to external plugin"));
    }

    #[test]
    fn test_create_provider_gitlab_not_implemented() {
        let provider = GitProvider::GitLab {
            namespace: "namespace".to_string(),
            repo: "repo".to_string(),
        };
        let result = create_provider(&provider, None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("GitLab issue provider not yet implemented"));
    }

    #[test]
    fn test_create_provider_bitbucket_not_implemented() {
        let provider = GitProvider::Bitbucket {
            workspace: "workspace".to_string(),
            repo: "repo".to_string(),
        };
        let result = create_provider(&provider, None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Bitbucket issue provider not yet implemented"));
    }

    #[test]
    fn test_create_provider_unknown() {
        let provider = GitProvider::Unknown {
            host: "example.com".to_string(),
        };
        let result = create_provider(&provider, None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No issue provider for unknown git host"));
    }
}
