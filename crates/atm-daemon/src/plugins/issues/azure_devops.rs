//! Azure DevOps issue provider stub (not yet implemented)

use super::provider::IssueProvider;
use super::types::{Issue, IssueComment, IssueFilter};
use crate::plugin::PluginError;

/// Azure DevOps issue provider (stub)
#[derive(Debug)]
pub struct AzureDevOpsProvider {
    org: String,
    project: String,
    repo: String,
}

impl AzureDevOpsProvider {
    /// Create a new Azure DevOps provider
    pub fn new(org: String, project: String, repo: String) -> Self {
        Self { org, project, repo }
    }
}

impl IssueProvider for AzureDevOpsProvider {
    async fn list_issues(&self, _filter: &IssueFilter) -> Result<Vec<Issue>, PluginError> {
        Err(PluginError::Provider {
            message: format!(
                "Azure DevOps provider not yet implemented (org: {}, project: {}, repo: {})",
                self.org, self.project, self.repo
            ),
            source: None,
        })
    }

    async fn get_issue(&self, number: u64) -> Result<Issue, PluginError> {
        Err(PluginError::Provider {
            message: format!("Azure DevOps provider not yet implemented (issue {number})"),
            source: None,
        })
    }

    async fn add_comment(&self, issue_number: u64, _body: &str) -> Result<IssueComment, PluginError> {
        Err(PluginError::Provider {
            message: format!("Azure DevOps provider not yet implemented (comment on issue {issue_number})"),
            source: None,
        })
    }

    async fn list_comments(&self, issue_number: u64) -> Result<Vec<IssueComment>, PluginError> {
        Err(PluginError::Provider {
            message: format!("Azure DevOps provider not yet implemented (comments for issue {issue_number})"),
            source: None,
        })
    }

    fn provider_name(&self) -> &str {
        "Azure DevOps"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_azure_devops_provider_creation() {
        let provider = AzureDevOpsProvider::new(
            "myorg".to_string(),
            "myproject".to_string(),
            "myrepo".to_string(),
        );
        assert_eq!(provider.provider_name(), "Azure DevOps");
        assert_eq!(provider.org, "myorg");
        assert_eq!(provider.project, "myproject");
        assert_eq!(provider.repo, "myrepo");
    }

    #[tokio::test]
    async fn test_azure_devops_not_implemented() {
        let provider = AzureDevOpsProvider::new(
            "org".to_string(),
            "proj".to_string(),
            "repo".to_string(),
        );

        let filter = IssueFilter::default();
        let result = provider.list_issues(&filter).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not yet implemented"));
    }
}
