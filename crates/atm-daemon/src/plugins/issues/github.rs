//! GitHub issue provider using the `gh` CLI

use super::provider::IssueProvider;
use super::types::{Issue, IssueComment, IssueFilter, IssueLabel, IssueState};
use crate::plugin::PluginError;
use serde::Deserialize;
use std::process::Command;

/// GitHub issue provider that uses the `gh` CLI
#[derive(Debug)]
pub struct GitHubProvider {
    owner: String,
    repo: String,
}

impl GitHubProvider {
    /// Create a new GitHub provider for the given owner/repo
    pub fn new(owner: String, repo: String) -> Self {
        Self { owner, repo }
    }

    /// Execute a `gh` command and return stdout
    async fn run_gh(&self, args: &[&str]) -> Result<String, PluginError> {
        // Run gh command in a blocking task
        let args_owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        tokio::task::spawn_blocking(move || {
            let output = Command::new("gh")
                .args(&args_owned)
                .output()
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        PluginError::Provider {
                            message: "gh CLI not found. Install from https://cli.github.com/".to_string(),
                            source: Some(Box::new(e)),
                        }
                    } else {
                        PluginError::Provider {
                            message: format!("Failed to execute gh: {e}"),
                            source: Some(Box::new(e)),
                        }
                    }
                })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(PluginError::Provider {
                    message: format!("gh command failed: {stderr}"),
                    source: None,
                });
            }

            let stdout = String::from_utf8(output.stdout).map_err(|e| PluginError::Provider {
                message: format!("Invalid UTF-8 in gh output: {e}"),
                source: Some(Box::new(e)),
            })?;

            Ok(stdout)
        })
        .await
        .map_err(|e| PluginError::Runtime {
            message: format!("Task join error: {e}"),
            source: Some(Box::new(e)),
        })?
    }

    /// Parse GitHub issue JSON from `gh issue list` or `gh issue view`
    fn parse_issue(&self, gh_json: &GhIssue) -> Issue {
        Issue {
            id: gh_json.number.to_string(),
            number: gh_json.number,
            title: gh_json.title.clone(),
            body: gh_json.body.clone(),
            state: if gh_json.state.to_uppercase() == "OPEN" {
                IssueState::Open
            } else {
                IssueState::Closed
            },
            labels: gh_json
                .labels
                .iter()
                .map(|l| IssueLabel {
                    name: l.name.clone(),
                    color: l.color.clone(),
                })
                .collect(),
            assignees: gh_json
                .assignees
                .iter()
                .map(|a| a.login.clone())
                .collect(),
            author: gh_json.author.login.clone(),
            created_at: gh_json.created_at.clone(),
            updated_at: gh_json.updated_at.clone(),
            url: gh_json.url.clone(),
        }
    }
}

impl IssueProvider for GitHubProvider {
    async fn list_issues(&self, filter: &IssueFilter) -> Result<Vec<Issue>, PluginError> {
        // Build args as owned strings to avoid lifetime issues
        let repo_arg = format!("{}/{}", self.owner, self.repo);
        let mut args = vec![
            "issue".to_string(),
            "list".to_string(),
            "--repo".to_string(),
            repo_arg,
            "--json".to_string(),
            "number,title,body,state,labels,assignees,author,createdAt,updatedAt,url".to_string(),
        ];

        // Add state filter
        if let Some(state) = filter.state {
            let state_arg = match state {
                IssueState::Open => "open",
                IssueState::Closed => "closed",
            };
            args.push("--state".to_string());
            args.push(state_arg.to_string());
        } else {
            args.push("--state".to_string());
            args.push("all".to_string());
        }

        // Add label filter (gh uses comma-separated)
        if !filter.labels.is_empty() {
            args.push("--label".to_string());
            args.push(filter.labels.join(","));
        }

        // Add assignee filter (gh accepts multiple --assignee flags)
        for assignee in &filter.assignees {
            args.push("--assignee".to_string());
            args.push(assignee.clone());
        }

        // Convert to &str for run_gh
        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self.run_gh(&args_refs).await?;

        let gh_issues: Vec<GhIssue> =
            serde_json::from_str(&output).map_err(|e| PluginError::Provider {
                message: format!("Failed to parse gh JSON: {e}"),
                source: Some(Box::new(e)),
            })?;

        let mut issues: Vec<Issue> = gh_issues.iter().map(|gh| self.parse_issue(gh)).collect();

        // Apply since filter (gh doesn't support this natively)
        if let Some(since) = &filter.since {
            issues.retain(|issue| issue.updated_at >= *since);
        }

        Ok(issues)
    }

    async fn get_issue(&self, number: u64) -> Result<Issue, PluginError> {
        let number_arg = number.to_string();
        let repo_arg = format!("{}/{}", self.owner, self.repo);
        let args = [
            "issue",
            "view",
            &number_arg,
            "--repo",
            &repo_arg,
            "--json",
            "number,title,body,state,labels,assignees,author,createdAt,updatedAt,url",
        ];

        let output = self.run_gh(&args).await?;

        let gh_issue: GhIssue =
            serde_json::from_str(&output).map_err(|e| PluginError::Provider {
                message: format!("Failed to parse gh JSON: {e}"),
                source: Some(Box::new(e)),
            })?;

        Ok(self.parse_issue(&gh_issue))
    }

    async fn add_comment(&self, issue_number: u64, body: &str) -> Result<IssueComment, PluginError> {
        let number_arg = issue_number.to_string();
        let repo_arg = format!("{}/{}", self.owner, self.repo);
        let args = [
            "issue",
            "comment",
            &number_arg,
            "--repo",
            &repo_arg,
            "--body",
            body,
        ];

        self.run_gh(&args).await?;

        // gh issue comment doesn't return JSON, so we construct a minimal comment
        // In a real implementation, we'd fetch the comment ID via API
        Ok(IssueComment {
            id: "unknown".to_string(),
            body: body.to_string(),
            author: "current-user".to_string(), // Would need to query gh api user
            created_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    async fn list_comments(&self, issue_number: u64) -> Result<Vec<IssueComment>, PluginError> {
        let api_path = format!(
            "repos/{}/{}/issues/{}/comments",
            self.owner, self.repo, issue_number
        );
        let args = [
            "api",
            &api_path,
            "--jq",
            r#"[.[] | {id: (.id | tostring), body, author: .user.login, created_at: .created_at}]"#,
        ];

        let output = self.run_gh(&args).await?;

        let comments: Vec<IssueComment> =
            serde_json::from_str(&output).map_err(|e| PluginError::Provider {
                message: format!("Failed to parse comment JSON: {e}"),
                source: Some(Box::new(e)),
            })?;

        Ok(comments)
    }

    fn provider_name(&self) -> &str {
        "GitHub"
    }
}

/// GitHub issue JSON schema (from `gh issue list --json`)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhIssue {
    number: u64,
    title: String,
    body: Option<String>,
    state: String,
    labels: Vec<GhLabel>,
    assignees: Vec<GhUser>,
    author: GhUser,
    created_at: String,
    updated_at: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct GhLabel {
    name: String,
    color: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhUser {
    login: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_github_provider_creation() {
        let provider = GitHubProvider::new("owner".to_string(), "repo".to_string());
        assert_eq!(provider.provider_name(), "GitHub");
        assert_eq!(provider.owner, "owner");
        assert_eq!(provider.repo, "repo");
    }

    #[test]
    fn test_parse_issue() {
        let provider = GitHubProvider::new("owner".to_string(), "repo".to_string());

        let gh_issue = GhIssue {
            number: 42,
            title: "Test issue".to_string(),
            body: Some("Body text".to_string()),
            state: "OPEN".to_string(),
            labels: vec![GhLabel {
                name: "bug".to_string(),
                color: Some("ff0000".to_string()),
            }],
            assignees: vec![GhUser {
                login: "user1".to_string(),
            }],
            author: GhUser {
                login: "author1".to_string(),
            },
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            url: "https://github.com/owner/repo/issues/42".to_string(),
        };

        let issue = provider.parse_issue(&gh_issue);

        assert_eq!(issue.number, 42);
        assert_eq!(issue.title, "Test issue");
        assert_eq!(issue.state, IssueState::Open);
        assert_eq!(issue.labels.len(), 1);
        assert_eq!(issue.labels[0].name, "bug");
        assert_eq!(issue.assignees.len(), 1);
        assert_eq!(issue.assignees[0], "user1");
        assert_eq!(issue.author, "author1");
    }

    #[test]
    fn test_parse_issue_closed() {
        let provider = GitHubProvider::new("owner".to_string(), "repo".to_string());

        let gh_issue = GhIssue {
            number: 1,
            title: "Closed issue".to_string(),
            body: None,
            state: "CLOSED".to_string(),
            labels: vec![],
            assignees: vec![],
            author: GhUser {
                login: "author".to_string(),
            },
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            url: "https://github.com/owner/repo/issues/1".to_string(),
        };

        let issue = provider.parse_issue(&gh_issue);
        assert_eq!(issue.state, IssueState::Closed);
    }
}
