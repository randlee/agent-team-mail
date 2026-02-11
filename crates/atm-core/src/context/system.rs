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
    /// Claude Code version string
    pub claude_version: String,
    /// Schema version (from Sprint 1.2)
    ///
    /// TODO: Replace Option with actual SchemaVersion when Sprint 1.2 completes
    pub schema_version: Option<()>,
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
        claude_version: String,
        default_team: String,
    ) -> Self {
        Self {
            hostname,
            platform,
            claude_root,
            claude_version,
            schema_version: None, // TODO: Populate when Sprint 1.2 completes
            repo: None,
            default_team,
        }
    }

    /// Set the repository context
    pub fn with_repo(mut self, repo: RepoContext) -> Self {
        self.repo = Some(repo);
        self
    }

    /// Set the schema version (placeholder for Sprint 1.2)
    pub fn with_schema_version(mut self, _version: ()) -> Self {
        self.schema_version = Some(());
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
            "2.1.39".to_string(),
            "default-team".to_string(),
        );

        assert_eq!(ctx.hostname, "test-host");
        assert_eq!(ctx.platform, Platform::Linux);
        assert_eq!(ctx.claude_root, PathBuf::from("/home/user/.claude"));
        assert_eq!(ctx.claude_version, "2.1.39");
        assert_eq!(ctx.default_team, "default-team");
        assert!(ctx.repo.is_none());
        assert!(ctx.schema_version.is_none());
    }

    #[test]
    fn test_system_context_with_repo() {
        let repo = RepoContext::new(
            "test-repo".to_string(),
            PathBuf::from("/path/to/repo"),
        );

        let ctx = SystemContext::new(
            "test-host".to_string(),
            Platform::Linux,
            PathBuf::from("/home/user/.claude"),
            "2.1.39".to_string(),
            "default-team".to_string(),
        )
        .with_repo(repo);

        assert!(ctx.repo.is_some());
        assert_eq!(ctx.repo.unwrap().name, "test-repo");
    }
}
