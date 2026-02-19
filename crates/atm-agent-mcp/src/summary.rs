//! Session summary read/write utilities (FR-6, FR-7).
//!
//! Summary files are Markdown documents stored at
//! `{sessions_dir}/{team}/{identity}/{backend_id}/summary.md`.
//!
//! During graceful shutdown the proxy writes a compacted summary for each
//! active thread (FR-7.1). On `--resume` startup the summary is loaded and
//! prepended to the first `developer-instructions` injection (FR-6.1).
//!
//! All I/O errors are treated as non-fatal: writes that fail are logged and
//! skipped, and missing summaries result in `None` rather than errors.

use std::path::PathBuf;

/// Return the sessions directory root, delegating to [`crate::lock::sessions_dir()`].
fn sessions_root() -> PathBuf {
    crate::lock::sessions_dir()
}

/// Return the directory for a specific session's summary.
pub fn summary_dir(team: &str, identity: &str, backend_id: &str) -> PathBuf {
    sessions_root()
        .join(team)
        .join(identity)
        .join(backend_id)
}

/// Return the full path to a session's summary file.
pub fn summary_path(team: &str, identity: &str, backend_id: &str) -> PathBuf {
    summary_dir(team, identity, backend_id).join("summary.md")
}

/// Write a session summary to disk.
///
/// Creates parent directories if needed. Returns `Ok(())` on success or
/// the I/O error on failure (callers should treat failures as non-fatal).
pub async fn write_summary(
    team: &str,
    identity: &str,
    backend_id: &str,
    content: &str,
) -> std::io::Result<()> {
    let path = summary_path(team, identity, backend_id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, content).await
}

/// Read a session summary from disk.
///
/// Returns `Some(content)` if the file exists and is readable, `None` otherwise.
/// Logs a warning if the file exists but cannot be read.
pub async fn read_summary(team: &str, identity: &str, backend_id: &str) -> Option<String> {
    let path = summary_path(team, identity, backend_id);
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => Some(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "failed to read session summary"
            );
            None
        }
    }
}

/// Format a summary for prepending to `developer-instructions` on resume.
///
/// Wraps the summary in delimiters so the Codex agent can identify prior
/// session context.
///
/// # Arguments
///
/// * `identity` -- ATM identity of the session being resumed.
/// * `repo_name` -- Repository name, or `None` if unavailable.
/// * `branch` -- Git branch, or `None` if unavailable.
/// * `summary` -- The summary text from the previous session.
pub fn format_resume_context(
    identity: &str,
    repo_name: Option<&str>,
    branch: Option<&str>,
    summary: &str,
) -> String {
    let location = match (repo_name, branch) {
        (Some(repo), Some(br)) => format!("{repo}/{br}"),
        (Some(repo), None) => repo.to_string(),
        (None, Some(br)) => br.to_string(),
        (None, None) => "unknown".to_string(),
    };
    format!(
        "[Previous session \u{2014} {identity} on {location}]\n{summary}\n[End of previous session]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    fn setup_atm_home(dir: &TempDir) {
        // SAFETY: tests are serialised via #[serial]; no concurrent env mutation.
        unsafe { std::env::set_var("ATM_HOME", dir.path().to_str().unwrap()) };
    }

    fn teardown_atm_home() {
        unsafe { std::env::remove_var("ATM_HOME") };
    }

    #[tokio::test]
    #[serial]
    async fn test_write_read_roundtrip() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        let content = "## Summary\n- Did some work\n- State is good";
        write_summary("team", "dev", "thread-abc", content)
            .await
            .unwrap();
        let read_back = read_summary("team", "dev", "thread-abc").await;

        teardown_atm_home();

        assert_eq!(read_back, Some(content.to_string()));
    }

    #[tokio::test]
    #[serial]
    async fn test_read_returns_none_when_missing() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        let result = read_summary("no-team", "no-id", "no-thread").await;

        teardown_atm_home();

        assert!(result.is_none());
    }

    #[tokio::test]
    #[serial]
    async fn test_summary_path_construction() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        let path = summary_path("atm-dev", "arch-ctm", "thread-123");

        teardown_atm_home();

        // The path should contain all three components in order
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("atm-dev"));
        assert!(path_str.contains("arch-ctm"));
        assert!(path_str.contains("thread-123"));
        assert!(path_str.ends_with("summary.md"));
    }

    #[test]
    fn test_format_resume_context_contains_identity() {
        let result = format_resume_context("arch-ctm", Some("my-repo"), Some("main"), "some summary");
        assert!(result.contains("arch-ctm"));
    }

    #[test]
    fn test_format_resume_context_contains_summary() {
        let summary_text = "Working on feature X, step 3 complete.";
        let result = format_resume_context("dev", Some("repo"), Some("main"), summary_text);
        assert!(result.contains(summary_text));
        assert!(result.contains("[Previous session"));
        assert!(result.contains("[End of previous session]"));
    }

    #[tokio::test]
    #[serial]
    async fn test_write_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        // Deeply nested path should be created automatically
        let result = write_summary("deep-team", "deep-id", "deep-thread", "content").await;

        teardown_atm_home();

        assert!(result.is_ok());
    }

    #[test]
    fn test_format_resume_context_no_repo() {
        let result = format_resume_context("dev", None, None, "summary text");
        assert!(result.contains("unknown"), "should handle None repo/branch gracefully");
        assert!(result.contains("summary text"));
    }

    #[tokio::test]
    #[serial]
    async fn test_write_summary_overwrites_existing() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        write_summary("team", "id", "tid", "first content")
            .await
            .unwrap();
        write_summary("team", "id", "tid", "second content")
            .await
            .unwrap();

        let result = read_summary("team", "id", "tid").await;

        teardown_atm_home();

        assert_eq!(result, Some("second content".to_string()));
    }

    #[tokio::test]
    #[serial]
    async fn test_read_summary_different_combinations() {
        let dir = TempDir::new().unwrap();
        setup_atm_home(&dir);

        write_summary("t1", "i1", "b1", "content-1").await.unwrap();
        write_summary("t2", "i2", "b2", "content-2").await.unwrap();

        let r1 = read_summary("t1", "i1", "b1").await;
        let r2 = read_summary("t2", "i2", "b2").await;
        let r3 = read_summary("t1", "i2", "b1").await; // wrong identity

        teardown_atm_home();

        assert_eq!(r1, Some("content-1".to_string()));
        assert_eq!(r2, Some("content-2".to_string()));
        assert!(r3.is_none());
    }
}
