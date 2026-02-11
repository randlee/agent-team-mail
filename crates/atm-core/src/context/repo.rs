//! Repository context and git provider detection

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Git provider identification (parsed from remote URLs)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitProvider {
    /// GitHub
    GitHub { owner: String, repo: String },
    /// Azure DevOps
    AzureDevOps {
        org: String,
        project: String,
        repo: String,
    },
    /// GitLab
    GitLab { namespace: String, repo: String },
    /// Bitbucket
    Bitbucket { workspace: String, repo: String },
    /// Unknown git host
    Unknown { host: String },
}

/// Repository context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoContext {
    /// Repository name (derived from path or remote)
    pub name: String,
    /// Absolute path to repository root
    pub path: PathBuf,
    /// Raw git remote URL (if available)
    pub remote_url: Option<String>,
    /// Detected git provider (if remote URL present)
    pub provider: Option<GitProvider>,
}

impl RepoContext {
    /// Create a new RepoContext from a path
    ///
    /// This does NOT read git config or perform I/O. The caller must provide
    /// the remote URL separately using `with_remote()`.
    pub fn new(name: String, path: PathBuf) -> Self {
        Self {
            name,
            path,
            remote_url: None,
            provider: None,
        }
    }

    /// Set the remote URL and detect the provider
    pub fn with_remote(mut self, remote_url: String) -> Self {
        self.provider = Some(GitProvider::detect_from_url(&remote_url));
        self.remote_url = Some(remote_url);
        self
    }
}

impl GitProvider {
    /// Detect git provider from a remote URL
    ///
    /// Supports both SSH and HTTPS formats for:
    /// - GitHub
    /// - Azure DevOps
    /// - GitLab
    /// - Bitbucket
    ///
    /// Returns `Unknown` for unrecognized hosts.
    pub fn detect_from_url(url: &str) -> Self {
        // Try parsing SSH format first (git@host:path)
        if let Some(provider) = Self::parse_ssh_url(url) {
            return provider;
        }

        // Try parsing HTTPS format
        if let Some(provider) = Self::parse_https_url(url) {
            return provider;
        }

        // Fallback: extract host from URL
        if let Ok(parsed) = url::Url::parse(url)
            && let Some(host) = parsed.host_str()
        {
            return GitProvider::Unknown {
                host: host.to_string(),
            };
        }

        // Last resort: try to extract from SSH URL manually
        if let Some(host) = Self::extract_host_from_ssh(url) {
            return GitProvider::Unknown { host };
        }

        GitProvider::Unknown {
            host: "unknown".to_string(),
        }
    }

    /// Parse SSH-style URLs: git@host:path/to/repo.git
    fn parse_ssh_url(url: &str) -> Option<Self> {
        if !url.contains('@') || !url.contains(':') {
            return None;
        }

        let parts: Vec<&str> = url.split('@').collect();
        if parts.len() != 2 {
            return None;
        }

        let host_and_path: Vec<&str> = parts[1].splitn(2, ':').collect();
        if host_and_path.len() != 2 {
            return None;
        }

        let host = host_and_path[0];
        let path = host_and_path[1].trim_end_matches(".git");

        match host {
            "github.com" => Self::parse_github_path(path),
            h if h.contains("dev.azure.com") => Self::parse_azure_ssh_path(path),
            h if h.contains("vs-ssh.visualstudio.com") => Self::parse_azure_ssh_path(path),
            "gitlab.com" => Self::parse_gitlab_path(path),
            h if h.starts_with("gitlab.") => Self::parse_gitlab_path(path),
            "bitbucket.org" => Self::parse_bitbucket_path(path),
            _ => Some(GitProvider::Unknown {
                host: host.to_string(),
            }),
        }
    }

    /// Parse HTTPS URLs: https://host/path/to/repo.git
    fn parse_https_url(url: &str) -> Option<Self> {
        let parsed = url::Url::parse(url).ok()?;
        let host = parsed.host_str()?;
        let path = parsed.path().trim_start_matches('/').trim_end_matches(".git");

        match host {
            "github.com" => Self::parse_github_path(path),
            h if h.contains("dev.azure.com") => Self::parse_azure_https_path(path),
            h if h.contains("visualstudio.com") => Self::parse_azure_https_path(path),
            "gitlab.com" => Self::parse_gitlab_path(path),
            h if h.starts_with("gitlab.") => Self::parse_gitlab_path(path),
            "bitbucket.org" => Self::parse_bitbucket_path(path),
            _ => Some(GitProvider::Unknown {
                host: host.to_string(),
            }),
        }
    }

    /// Parse GitHub path: owner/repo
    fn parse_github_path(path: &str) -> Option<Self> {
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() == 2 {
            Some(GitProvider::GitHub {
                owner: parts[0].to_string(),
                repo: parts[1].to_string(),
            })
        } else {
            None
        }
    }

    /// Parse Azure DevOps HTTPS path: org/project/_git/repo
    fn parse_azure_https_path(path: &str) -> Option<Self> {
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() >= 4 && parts[2] == "_git" {
            Some(GitProvider::AzureDevOps {
                org: parts[0].to_string(),
                project: parts[1].to_string(),
                repo: parts[3].to_string(),
            })
        } else if parts.len() == 3 {
            // Old format: org/repo (no project)
            Some(GitProvider::AzureDevOps {
                org: parts[0].to_string(),
                project: parts[1].to_string(),
                repo: parts[2].to_string(),
            })
        } else {
            None
        }
    }

    /// Parse Azure DevOps SSH path: v3/org/project/repo
    fn parse_azure_ssh_path(path: &str) -> Option<Self> {
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() >= 4 && parts[0] == "v3" {
            Some(GitProvider::AzureDevOps {
                org: parts[1].to_string(),
                project: parts[2].to_string(),
                repo: parts[3].to_string(),
            })
        } else {
            None
        }
    }

    /// Parse GitLab path: namespace/repo (or nested: group/subgroup/repo)
    fn parse_gitlab_path(path: &str) -> Option<Self> {
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() >= 2 {
            let repo = parts.last()?.to_string();
            let namespace = parts[..parts.len() - 1].join("/");
            Some(GitProvider::GitLab { namespace, repo })
        } else {
            None
        }
    }

    /// Parse Bitbucket path: workspace/repo
    fn parse_bitbucket_path(path: &str) -> Option<Self> {
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() == 2 {
            Some(GitProvider::Bitbucket {
                workspace: parts[0].to_string(),
                repo: parts[1].to_string(),
            })
        } else {
            None
        }
    }

    /// Extract host from malformed SSH URL
    fn extract_host_from_ssh(url: &str) -> Option<String> {
        if let Some(at_pos) = url.find('@')
            && let Some(colon_pos) = url[at_pos..].find(':')
        {
            let host = &url[at_pos + 1..at_pos + colon_pos];
            return Some(host.to_string());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // GitHub SSH URLs
    #[test]
    fn test_github_ssh() {
        let url = "git@github.com:owner/repo.git";
        let provider = GitProvider::detect_from_url(url);
        assert_eq!(
            provider,
            GitProvider::GitHub {
                owner: "owner".to_string(),
                repo: "repo".to_string()
            }
        );
    }

    #[test]
    fn test_github_ssh_no_extension() {
        let url = "git@github.com:owner/repo";
        let provider = GitProvider::detect_from_url(url);
        assert_eq!(
            provider,
            GitProvider::GitHub {
                owner: "owner".to_string(),
                repo: "repo".to_string()
            }
        );
    }

    // GitHub HTTPS URLs
    #[test]
    fn test_github_https() {
        let url = "https://github.com/owner/repo.git";
        let provider = GitProvider::detect_from_url(url);
        assert_eq!(
            provider,
            GitProvider::GitHub {
                owner: "owner".to_string(),
                repo: "repo".to_string()
            }
        );
    }

    #[test]
    fn test_github_https_no_extension() {
        let url = "https://github.com/owner/repo";
        let provider = GitProvider::detect_from_url(url);
        assert_eq!(
            provider,
            GitProvider::GitHub {
                owner: "owner".to_string(),
                repo: "repo".to_string()
            }
        );
    }

    // Azure DevOps URLs
    #[test]
    fn test_azure_devops_https() {
        let url = "https://dev.azure.com/org/project/_git/repo";
        let provider = GitProvider::detect_from_url(url);
        assert_eq!(
            provider,
            GitProvider::AzureDevOps {
                org: "org".to_string(),
                project: "project".to_string(),
                repo: "repo".to_string()
            }
        );
    }

    #[test]
    fn test_azure_devops_ssh() {
        let url = "git@ssh.dev.azure.com:v3/org/project/repo";
        let provider = GitProvider::detect_from_url(url);
        assert_eq!(
            provider,
            GitProvider::AzureDevOps {
                org: "org".to_string(),
                project: "project".to_string(),
                repo: "repo".to_string()
            }
        );
    }

    #[test]
    fn test_azure_visualstudio_ssh() {
        let url = "git@vs-ssh.visualstudio.com:v3/org/project/repo";
        let provider = GitProvider::detect_from_url(url);
        assert_eq!(
            provider,
            GitProvider::AzureDevOps {
                org: "org".to_string(),
                project: "project".to_string(),
                repo: "repo".to_string()
            }
        );
    }

    // GitLab URLs
    #[test]
    fn test_gitlab_ssh() {
        let url = "git@gitlab.com:namespace/repo.git";
        let provider = GitProvider::detect_from_url(url);
        assert_eq!(
            provider,
            GitProvider::GitLab {
                namespace: "namespace".to_string(),
                repo: "repo".to_string()
            }
        );
    }

    #[test]
    fn test_gitlab_https() {
        let url = "https://gitlab.com/namespace/repo.git";
        let provider = GitProvider::detect_from_url(url);
        assert_eq!(
            provider,
            GitProvider::GitLab {
                namespace: "namespace".to_string(),
                repo: "repo".to_string()
            }
        );
    }

    #[test]
    fn test_gitlab_nested_namespace() {
        let url = "git@gitlab.com:group/subgroup/repo.git";
        let provider = GitProvider::detect_from_url(url);
        assert_eq!(
            provider,
            GitProvider::GitLab {
                namespace: "group/subgroup".to_string(),
                repo: "repo".to_string()
            }
        );
    }

    #[test]
    fn test_gitlab_self_hosted() {
        let url = "git@gitlab.example.com:namespace/repo.git";
        let provider = GitProvider::detect_from_url(url);
        // Self-hosted GitLab should still parse as GitLab
        assert!(matches!(provider, GitProvider::GitLab { .. }));
    }

    // Bitbucket URLs
    #[test]
    fn test_bitbucket_ssh() {
        let url = "git@bitbucket.org:workspace/repo.git";
        let provider = GitProvider::detect_from_url(url);
        assert_eq!(
            provider,
            GitProvider::Bitbucket {
                workspace: "workspace".to_string(),
                repo: "repo".to_string()
            }
        );
    }

    #[test]
    fn test_bitbucket_https() {
        let url = "https://bitbucket.org/workspace/repo.git";
        let provider = GitProvider::detect_from_url(url);
        assert_eq!(
            provider,
            GitProvider::Bitbucket {
                workspace: "workspace".to_string(),
                repo: "repo".to_string()
            }
        );
    }

    // Unknown/Edge cases
    #[test]
    fn test_unknown_host() {
        let url = "git@example.com:owner/repo.git";
        let provider = GitProvider::detect_from_url(url);
        assert!(matches!(provider, GitProvider::Unknown { .. }));
        if let GitProvider::Unknown { host } = provider {
            assert_eq!(host, "example.com");
        }
    }

    #[test]
    fn test_malformed_url() {
        let url = "not-a-valid-url";
        let provider = GitProvider::detect_from_url(url);
        assert!(matches!(provider, GitProvider::Unknown { .. }));
    }

    #[test]
    fn test_empty_url() {
        let url = "";
        let provider = GitProvider::detect_from_url(url);
        assert!(matches!(provider, GitProvider::Unknown { .. }));
    }

    // RepoContext tests
    #[test]
    fn test_repo_context_new() {
        let ctx = RepoContext::new(
            "test-repo".to_string(),
            PathBuf::from("/path/to/repo"),
        );
        assert_eq!(ctx.name, "test-repo");
        assert_eq!(ctx.path, PathBuf::from("/path/to/repo"));
        assert!(ctx.remote_url.is_none());
        assert!(ctx.provider.is_none());
    }

    #[test]
    fn test_repo_context_with_remote() {
        let ctx = RepoContext::new(
            "test-repo".to_string(),
            PathBuf::from("/path/to/repo"),
        )
        .with_remote("git@github.com:owner/repo.git".to_string());

        assert_eq!(ctx.remote_url, Some("git@github.com:owner/repo.git".to_string()));
        assert!(ctx.provider.is_some());

        if let Some(GitProvider::GitHub { owner, repo }) = ctx.provider {
            assert_eq!(owner, "owner");
            assert_eq!(repo, "repo");
        } else {
            panic!("Expected GitHub provider");
        }
    }
}
