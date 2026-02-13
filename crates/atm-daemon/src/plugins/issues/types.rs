//! Shared types for the Issues plugin provider abstraction

use serde::{Deserialize, Serialize};

/// An issue from a git hosting provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Provider-specific ID (e.g., "123" for GitHub)
    pub id: String,
    /// Issue number
    pub number: u64,
    /// Issue title
    pub title: String,
    /// Issue body/description
    pub body: Option<String>,
    /// Current state
    pub state: IssueState,
    /// Labels attached to the issue
    pub labels: Vec<IssueLabel>,
    /// Assigned users
    pub assignees: Vec<String>,
    /// Issue author
    pub author: String,
    /// Creation timestamp (ISO 8601)
    pub created_at: String,
    /// Last update timestamp (ISO 8601)
    pub updated_at: String,
    /// Web URL to the issue
    pub url: String,
}

/// Issue state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IssueState {
    /// Issue is open
    Open,
    /// Issue is closed
    Closed,
}

/// Label attached to an issue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueLabel {
    /// Label name
    pub name: String,
    /// Label color (hex code, optional)
    pub color: Option<String>,
}

/// Comment on an issue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueComment {
    /// Provider-specific comment ID
    pub id: String,
    /// Comment body text
    pub body: String,
    /// Comment author
    pub author: String,
    /// Creation timestamp (ISO 8601)
    pub created_at: String,
}

/// Filter for querying issues
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueFilter {
    /// Filter by labels (all must match)
    pub labels: Vec<String>,
    /// Filter by assignees
    pub assignees: Vec<String>,
    /// Filter by state (None = all states)
    pub state: Option<IssueState>,
    /// Only issues updated after this timestamp (ISO 8601)
    pub since: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_serialization() {
        let issue = Issue {
            id: "123".to_string(),
            number: 42,
            title: "Test issue".to_string(),
            body: Some("Issue body".to_string()),
            state: IssueState::Open,
            labels: vec![IssueLabel {
                name: "bug".to_string(),
                color: Some("ff0000".to_string()),
            }],
            assignees: vec!["user1".to_string()],
            author: "author1".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            url: "https://github.com/owner/repo/issues/42".to_string(),
        };

        let json = serde_json::to_string(&issue).unwrap();
        let deserialized: Issue = serde_json::from_str(&json).unwrap();

        assert_eq!(issue.id, deserialized.id);
        assert_eq!(issue.number, deserialized.number);
        assert_eq!(issue.title, deserialized.title);
        assert_eq!(issue.state, deserialized.state);
    }

    #[test]
    fn test_issue_state_serialization() {
        let open = IssueState::Open;
        let closed = IssueState::Closed;

        let open_json = serde_json::to_string(&open).unwrap();
        let closed_json = serde_json::to_string(&closed).unwrap();

        assert_eq!(open_json, r#""Open""#);
        assert_eq!(closed_json, r#""Closed""#);

        let open_de: IssueState = serde_json::from_str(&open_json).unwrap();
        let closed_de: IssueState = serde_json::from_str(&closed_json).unwrap();

        assert_eq!(open_de, IssueState::Open);
        assert_eq!(closed_de, IssueState::Closed);
    }

    #[test]
    fn test_issue_filter_default() {
        let filter = IssueFilter::default();
        assert!(filter.labels.is_empty());
        assert!(filter.assignees.is_empty());
        assert!(filter.state.is_none());
        assert!(filter.since.is_none());
    }

    #[test]
    fn test_issue_filter_with_filters() {
        let filter = IssueFilter {
            labels: vec!["bug".to_string(), "urgent".to_string()],
            assignees: vec!["user1".to_string()],
            state: Some(IssueState::Open),
            since: Some("2026-01-01T00:00:00Z".to_string()),
        };

        assert_eq!(filter.labels.len(), 2);
        assert_eq!(filter.assignees.len(), 1);
        assert_eq!(filter.state, Some(IssueState::Open));
        assert_eq!(filter.since, Some("2026-01-01T00:00:00Z".to_string()));
    }
}
