//! Issues plugin — provider abstraction for issue tracking

mod azure_devops;
mod config;
mod github;
mod plugin;
mod provider;
mod types;

pub use azure_devops::AzureDevOpsProvider;
pub use config::IssuesConfig;
pub use github::GitHubProvider;
pub use plugin::IssuesPlugin;
pub use provider::{ErasedIssueProvider, IssueProvider};
pub use types::{Issue, IssueComment, IssueFilter, IssueLabel, IssueState};

use atm_core::context::GitProvider;
use crate::plugin::PluginError;

/// Create an issue provider for the given git provider
///
/// Returns a boxed trait object that implements ErasedIssueProvider.
///
/// # Arguments
///
/// * `provider` - The git provider (GitHub, Azure DevOps, etc.)
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
        GitProvider::AzureDevOps { org, project, repo } => Ok(Box::new(AzureDevOpsProvider::new(
            org.clone(),
            project.clone(),
            repo.clone(),
        ))),
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
    fn test_create_provider_azure_devops() {
        let provider = GitProvider::AzureDevOps {
            org: "org".to_string(),
            project: "project".to_string(),
            repo: "repo".to_string(),
        };
        let result = create_provider(&provider, None);
        assert!(result.is_ok());
        let provider = result.unwrap();
        assert_eq!(provider.provider_name(), "Azure DevOps");
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
