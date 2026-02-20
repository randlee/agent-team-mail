//! Git context detection for per-turn context injection.
//!
//! [`detect_context`] probes the working directory for git metadata (repo
//! root, remote-derived name, current branch) using async child processes.
//! When the directory is not inside a git repository, the git fields are
//! `None`; the `cwd` field is always populated.
//!
//! # Cross-platform notes
//!
//! `git` must be on `PATH`. The commands executed are:
//! - `git rev-parse --show-toplevel` — absolute path of the repository root
//! - `git rev-parse --abbrev-ref HEAD` — current branch name
//! - `git remote get-url origin` — remote URL for name derivation
//!
//! Any failure (non-zero exit, parse error) causes the git fields to be set
//! to `None` rather than propagating an error.

use tokio::process::Command;

/// Runtime git context captured per-turn.
///
/// When not inside a git repository `repo_root`, `repo_name`, and `branch`
/// are all `None`.
#[derive(Debug, Clone)]
pub struct TurnContext {
    /// Absolute path of the git repository root, or `None`.
    pub repo_root: Option<String>,
    /// Repository name derived from the remote URL (last path component,
    /// `.git` suffix stripped), or directory name if no remote, or `None`
    /// when not in a git repository.
    pub repo_name: Option<String>,
    /// Current git branch, or `None`.
    pub branch: Option<String>,
    /// Effective working directory (always set).
    pub cwd: String,
}

/// Detect git context from `cwd`.
///
/// Runs git subcommands asynchronously. When any git command fails the
/// function returns a [`TurnContext`] with `repo_root`, `repo_name`, and
/// `branch` all set to `None`.
///
/// # Notes
///
/// This function never returns an error — git failures are silently treated
/// as "not in a git repository".
///
/// # Examples
///
/// ```no_run
/// use atm_agent_mcp::context::detect_context;
///
/// # async fn example() {
/// let ctx = detect_context("/tmp").await;
/// // /tmp is not a git repo, so git fields are None
/// assert!(ctx.repo_root.is_none());
/// # }
/// ```
pub async fn detect_context(cwd: &str) -> TurnContext {
    // Canonicalise cwd — fall back to the raw string on error
    let effective_cwd = tokio::fs::canonicalize(cwd)
        .await
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| cwd.to_string());

    let repo_root = git_toplevel(&effective_cwd).await;

    if repo_root.is_none() {
        return TurnContext {
            repo_root: None,
            repo_name: None,
            branch: None,
            cwd: effective_cwd,
        };
    }

    let root = repo_root.clone().unwrap();
    let branch = git_branch(&effective_cwd).await;
    let repo_name = git_repo_name(&effective_cwd, &root).await;

    TurnContext {
        repo_root,
        repo_name,
        branch,
        cwd: effective_cwd,
    }
}

/// Run `git rev-parse --show-toplevel` in `cwd`.
///
/// Returns `None` on any failure (not a git repo, git not found, etc.).
async fn git_toplevel(cwd: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let s = String::from_utf8(output.stdout).ok()?;
    let trimmed = s.trim().to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}

/// Run `git branch --show-current` in `cwd`, falling back to
/// `git rev-parse --abbrev-ref HEAD` when the first command returns an empty
/// string (detached HEAD state on GitHub Actions and similar CI environments).
///
/// Returns `None` on any failure.  In detached HEAD mode the fallback returns
/// the literal string `"HEAD"`, which is wrapped in `Some("HEAD")` so callers
/// can tell "inside a git repo but detached" from "not a git repo at all".
async fn git_branch(cwd: &str) -> Option<String> {
    // Primary: git branch --show-current (empty in detached HEAD)
    let show_current = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output()
        .await
        .ok();

    if let Some(out) = show_current {
        if out.status.success()
            && let Ok(s) = String::from_utf8(out.stdout)
        {
            let trimmed = s.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }

    // Fallback: git rev-parse --abbrev-ref HEAD
    // Returns "HEAD" in detached mode — keep it as Some("HEAD") so the caller
    // can distinguish "inside repo, detached" from "not a repo at all".
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let s = String::from_utf8(output.stdout).ok()?;
    let trimmed = s.trim().to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}

/// Derive a human-readable repository name.
///
/// Tries `git remote get-url origin` first; if that fails, falls back to the
/// last component of `repo_root`. Returns `None` only when `repo_root` is
/// also an unusable path.
async fn git_repo_name(cwd: &str, repo_root: &str) -> Option<String> {
    // Try to get the remote URL
    if let Some(name) = git_name_from_remote(cwd).await {
        return Some(name);
    }

    // Fall back to directory name
    std::path::Path::new(repo_root)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
}

/// Extract a repo name from `git remote get-url origin`.
async fn git_name_from_remote(cwd: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(cwd)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let url = String::from_utf8(output.stdout).ok()?;
    repo_name_from_url(url.trim())
}

/// Extract the repository name from a git remote URL.
///
/// Strips the `.git` suffix and returns the last path component.
///
/// ```
/// // Internal helper — not public API
/// ```
fn repo_name_from_url(url: &str) -> Option<String> {
    // Strip trailing slashes
    let url = url.trim_end_matches('/');
    // Get the last path component
    let last = url.split('/').next_back()?;
    // Strip .git suffix
    let name = last.strip_suffix(".git").unwrap_or(last);
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── repo_name_from_url ──────────────────────────────────────────────────

    #[test]
    fn repo_name_from_https_url_with_git_suffix() {
        assert_eq!(
            repo_name_from_url("https://github.com/user/my-repo.git"),
            Some("my-repo".to_string())
        );
    }

    #[test]
    fn repo_name_from_ssh_url() {
        assert_eq!(
            repo_name_from_url("git@github.com:user/my-repo.git"),
            Some("my-repo".to_string())
        );
    }

    #[test]
    fn repo_name_from_url_without_git_suffix() {
        assert_eq!(
            repo_name_from_url("https://github.com/user/my-repo"),
            Some("my-repo".to_string())
        );
    }

    #[test]
    fn repo_name_from_url_with_trailing_slash() {
        assert_eq!(
            repo_name_from_url("https://github.com/user/my-repo/"),
            Some("my-repo".to_string())
        );
    }

    // ─── detect_context — async tests ────────────────────────────────────────

    #[tokio::test]
    async fn detect_context_non_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = detect_context(dir.path().to_str().unwrap()).await;
        assert!(ctx.repo_root.is_none(), "tmp dir should not be a git repo");
        assert!(ctx.repo_name.is_none());
        assert!(ctx.branch.is_none());
        assert!(!ctx.cwd.is_empty());
    }

    #[tokio::test]
    async fn detect_context_repo_root_null_outside_git() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = detect_context(dir.path().to_str().unwrap()).await;
        // CRITICAL: must be None, not fall back to cwd
        assert!(ctx.repo_root.is_none());
    }

    #[tokio::test]
    async fn detect_context_in_git_repo() {
        // CARGO_MANIFEST_DIR is the crate directory, which is inside the git repo
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let ctx = detect_context(manifest_dir).await;
        // Should detect git repo
        assert!(
            ctx.repo_root.is_some(),
            "crate manifest dir should be detected as a git repo"
        );
    }

    #[tokio::test]
    async fn detect_context_repo_root_not_null_in_git_repo() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let ctx = detect_context(manifest_dir).await;
        assert!(
            ctx.repo_root.is_some(),
            "repo_root must be Some inside a git repo"
        );
    }

    #[tokio::test]
    async fn detect_context_cwd_always_set() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = detect_context(dir.path().to_str().unwrap()).await;
        assert!(!ctx.cwd.is_empty());
    }

    #[tokio::test]
    async fn detect_context_branch_some_in_git_repo() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let ctx = detect_context(manifest_dir).await;
        // Should have a branch in a checked-out repo
        assert!(
            ctx.branch.is_some(),
            "branch should be detected in a git repo"
        );
    }

    #[tokio::test]
    async fn detect_context_repo_name_some_in_git_repo() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let ctx = detect_context(manifest_dir).await;
        // May come from remote or directory name — either way should be Some
        assert!(
            ctx.repo_name.is_some(),
            "repo_name should be Some in a git repo"
        );
    }
}
