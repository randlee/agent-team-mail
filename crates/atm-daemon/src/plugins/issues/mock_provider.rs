//! Mock issue provider for testing

use super::provider::IssueProvider;
use super::types::{Issue, IssueComment, IssueFilter};
use crate::plugin::PluginError;
use std::sync::{Arc, Mutex};

/// Mock issue provider for testing. Returns canned data.
#[derive(Debug, Clone)]
pub struct MockProvider {
    /// Issues to return from list_issues/get_issue
    pub issues: Vec<Issue>,
    /// Comments to return from list_comments
    pub comments: Vec<IssueComment>,
    /// If set, all methods return this error
    pub error: Option<String>,
    /// Track calls for verification
    pub call_log: Arc<Mutex<Vec<MockCall>>>,
}

/// Record of method calls for test assertions
#[derive(Debug, Clone, PartialEq)]
pub enum MockCall {
    ListIssues(IssueFilter),
    GetIssue(u64),
    AddComment { issue_number: u64, body: String },
    ListComments(u64),
}

impl MockProvider {
    /// Create a new mock provider with empty data
    pub fn new() -> Self {
        Self {
            issues: Vec::new(),
            comments: Vec::new(),
            error: None,
            call_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Create a mock provider with specific issues
    pub fn with_issues(issues: Vec<Issue>) -> Self {
        Self {
            issues,
            comments: Vec::new(),
            error: None,
            call_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Set the error that all methods should return
    pub fn with_error(mut self, error: String) -> Self {
        self.error = Some(error);
        self
    }

    /// Get a copy of the call log for assertions
    pub fn get_calls(&self) -> Vec<MockCall> {
        self.call_log.lock().unwrap().clone()
    }

    /// Clear the call log
    pub fn clear_calls(&self) {
        self.call_log.lock().unwrap().clear();
    }

    /// Helper to log a call
    fn log_call(&self, call: MockCall) {
        self.call_log.lock().unwrap().push(call);
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl IssueProvider for MockProvider {
    async fn list_issues(&self, filter: &IssueFilter) -> Result<Vec<Issue>, PluginError> {
        self.log_call(MockCall::ListIssues(filter.clone()));

        if let Some(err) = &self.error {
            return Err(PluginError::Provider {
                message: err.clone(),
                source: None,
            });
        }

        // Apply filters to issues
        let mut filtered = self.issues.clone();

        // Filter by labels (all must match)
        if !filter.labels.is_empty() {
            filtered.retain(|issue| {
                filter
                    .labels
                    .iter()
                    .all(|label| issue.labels.iter().any(|l| &l.name == label))
            });
        }

        // Filter by assignees
        if !filter.assignees.is_empty() {
            filtered.retain(|issue| {
                filter
                    .assignees
                    .iter()
                    .any(|assignee| issue.assignees.contains(assignee))
            });
        }

        // Filter by state
        if let Some(state) = filter.state {
            filtered.retain(|issue| issue.state == state);
        }

        // Filter by since timestamp
        if let Some(since) = &filter.since {
            filtered.retain(|issue| &issue.updated_at >= since);
        }

        Ok(filtered)
    }

    async fn get_issue(&self, number: u64) -> Result<Issue, PluginError> {
        self.log_call(MockCall::GetIssue(number));

        if let Some(err) = &self.error {
            return Err(PluginError::Provider {
                message: err.clone(),
                source: None,
            });
        }

        self.issues
            .iter()
            .find(|issue| issue.number == number)
            .cloned()
            .ok_or_else(|| PluginError::Provider {
                message: format!("Issue #{number} not found"),
                source: None,
            })
    }

    async fn add_comment(&self, issue_number: u64, body: &str) -> Result<IssueComment, PluginError> {
        self.log_call(MockCall::AddComment {
            issue_number,
            body: body.to_string(),
        });

        if let Some(err) = &self.error {
            return Err(PluginError::Provider {
                message: err.clone(),
                source: None,
            });
        }

        // Create a synthetic comment
        Ok(IssueComment {
            id: format!("comment-{issue_number}"),
            body: body.to_string(),
            author: "test-user".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    async fn list_comments(&self, issue_number: u64) -> Result<Vec<IssueComment>, PluginError> {
        self.log_call(MockCall::ListComments(issue_number));

        if let Some(err) = &self.error {
            return Err(PluginError::Provider {
                message: err.clone(),
                source: None,
            });
        }

        Ok(self.comments.clone())
    }

    fn provider_name(&self) -> &str {
        "MockProvider"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::{IssueLabel, IssueState};

    #[test]
    fn test_mock_provider_new() {
        let provider = MockProvider::new();
        assert!(provider.issues.is_empty());
        assert!(provider.comments.is_empty());
        assert!(provider.error.is_none());
        assert!(provider.get_calls().is_empty());
    }

    #[test]
    fn test_mock_provider_with_issues() {
        let issues = vec![Issue {
            id: "1".to_string(),
            number: 42,
            title: "Test".to_string(),
            body: None,
            state: IssueState::Open,
            labels: Vec::new(),
            assignees: Vec::new(),
            author: "test".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            url: "https://example.com/42".to_string(),
        }];

        let provider = MockProvider::with_issues(issues.clone());
        assert_eq!(provider.issues.len(), 1);
        assert_eq!(provider.issues[0].number, 42);
    }

    #[tokio::test]
    async fn test_mock_provider_list_issues_logs_call() {
        let provider = MockProvider::new();
        let filter = IssueFilter::default();

        let _ = provider.list_issues(&filter).await;

        let calls = provider.get_calls();
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0], MockCall::ListIssues(_)));
    }

    #[tokio::test]
    async fn test_mock_provider_error() {
        let provider = MockProvider::new().with_error("Test error".to_string());

        let result = provider.list_issues(&IssueFilter::default()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Test error"));
    }

    #[tokio::test]
    async fn test_mock_provider_filter_by_labels() {
        let issues = vec![
            Issue {
                id: "1".to_string(),
                number: 1,
                title: "Bug".to_string(),
                body: None,
                state: IssueState::Open,
                labels: vec![IssueLabel {
                    name: "bug".to_string(),
                    color: None,
                }],
                assignees: Vec::new(),
                author: "test".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                url: "https://example.com/1".to_string(),
            },
            Issue {
                id: "2".to_string(),
                number: 2,
                title: "Feature".to_string(),
                body: None,
                state: IssueState::Open,
                labels: vec![IssueLabel {
                    name: "feature".to_string(),
                    color: None,
                }],
                assignees: Vec::new(),
                author: "test".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                url: "https://example.com/2".to_string(),
            },
        ];

        let provider = MockProvider::with_issues(issues);
        let filter = IssueFilter {
            labels: vec!["bug".to_string()],
            ..Default::default()
        };

        let result = provider.list_issues(&filter).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].number, 1);
    }

    #[tokio::test]
    async fn test_mock_provider_get_issue() {
        let issues = vec![Issue {
            id: "1".to_string(),
            number: 42,
            title: "Test".to_string(),
            body: None,
            state: IssueState::Open,
            labels: Vec::new(),
            assignees: Vec::new(),
            author: "test".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            url: "https://example.com/42".to_string(),
        }];

        let provider = MockProvider::with_issues(issues);
        let issue = provider.get_issue(42).await.unwrap();
        assert_eq!(issue.number, 42);
        assert_eq!(issue.title, "Test");

        let calls = provider.get_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], MockCall::GetIssue(42));
    }

    #[tokio::test]
    async fn test_mock_provider_get_issue_not_found() {
        let provider = MockProvider::new();
        let result = provider.get_issue(999).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_mock_provider_add_comment() {
        let provider = MockProvider::new();
        let comment = provider.add_comment(42, "Test comment").await.unwrap();

        assert_eq!(comment.body, "Test comment");
        assert_eq!(comment.author, "test-user");

        let calls = provider.get_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0],
            MockCall::AddComment {
                issue_number: 42,
                body: "Test comment".to_string()
            }
        );
    }

    #[tokio::test]
    async fn test_mock_provider_clear_calls() {
        let provider = MockProvider::new();
        let _ = provider.list_issues(&IssueFilter::default()).await;
        assert_eq!(provider.get_calls().len(), 1);

        provider.clear_calls();
        assert!(provider.get_calls().is_empty());
    }
}
