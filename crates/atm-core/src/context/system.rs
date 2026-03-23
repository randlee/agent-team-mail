//! System context

use super::{Platform, RepoContext};
use std::path::PathBuf;

/// System context resolved at startup
///
/// Contains information about the runtime environment, Claude installation,
/// and current repository (if any).
#[derive(Debug, Clone)]
pub struct SystemContext {
    /// System hostname
    pub hostname: String,
    /// Operating system platform
    pub platform: Platform,
    /// Path to Claude root directory (~/.claude/)
    pub claude_root: PathBuf,
    /// Path to ATM runtime home (ATM_HOME)
    pub runtime_home: PathBuf,
    /// Claude Code version string
    pub claude_version: String,
    /// Repository context (if running in a git repository)
    pub repo: Option<RepoContext>,
    /// Default team name
    pub default_team: String,
}

impl SystemContext {
    /// Create a new SystemContext with minimal required fields
    ///
    /// This is a low-level constructor. Most code should use a builder or
    /// resolution function that populates all fields from the environment.
    pub fn new(
        hostname: String,
        platform: Platform,
        claude_root: PathBuf,
        runtime_home: PathBuf,
        claude_version: String,
        default_team: String,
    ) -> Self {
        Self {
            hostname,
            platform,
            claude_root,
            runtime_home,
            claude_version,
            repo: None,
            default_team,
        }
    }

    /// Set the repository context
    pub fn with_repo(mut self, repo: RepoContext) -> Self {
        self.repo = Some(repo);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_context_new() {
        let ctx = SystemContext::new(
            "test-host".to_string(),
            Platform::Linux,
            PathBuf::from("/home/user/.claude"),
            PathBuf::from("/home/user"),
            "2.1.39".to_string(),
            "default-team".to_string(),
        );

        assert_eq!(ctx.hostname, "test-host");
        assert_eq!(ctx.platform, Platform::Linux);
        assert_eq!(ctx.claude_root, PathBuf::from("/home/user/.claude"));
        assert_eq!(ctx.runtime_home, PathBuf::from("/home/user"));
        assert_eq!(ctx.claude_version, "2.1.39");
        assert_eq!(ctx.default_team, "default-team");
        assert!(ctx.repo.is_none());
    }

    #[test]
    fn test_system_context_with_repo() {
        let repo = RepoContext::new("test-repo".to_string(), PathBuf::from("/path/to/repo"));

        let ctx = SystemContext::new(
            "test-host".to_string(),
            Platform::Linux,
            PathBuf::from("/home/user/.claude"),
            PathBuf::from("/home/user"),
            "2.1.39".to_string(),
            "default-team".to_string(),
        )
        .with_repo(repo);

        assert!(ctx.repo.is_some());
        assert_eq!(ctx.repo.unwrap().name, "test-repo");
    }
}
