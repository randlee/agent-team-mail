//! Stdin queue for non-destructive ATM message injection into a running Codex session.
//!
//! Messages are written as `{uuid}.json` files and atomically claimed via a
//! `{uuid}.lock` sentinel file before being written to the Codex process stdin.
//! This prevents double-delivery when multiple drainers race to inject the same
//! message concurrently.
//!
//! ## Claim protocol
//!
//! To claim `{uuid}.json`:
//! 1. Atomically create `{uuid}.lock` with `create_new(true)` — maps to `O_EXCL`
//!    on POSIX and `CREATE_NEW` on Windows, both of which are atomic kernel ops.
//! 2. If creation fails (file already exists) → skip; another drainer owns it.
//! 3. If creation succeeds → read `{uuid}.json`, write to stdin, delete
//!    `{uuid}.json`, delete `{uuid}.lock`.
//! 4. On write failure → delete `{uuid}.lock` only; leave `{uuid}.json` for retry.
//!
//! This replaces the earlier `rename`-based approach which is not atomic under
//! concurrent `spawn_blocking` on Windows (`MoveFileEx` without
//! `MOVEFILE_REPLACE_EXISTING` still races when both threads attempt the same
//! source path).
//!
//! ## Queue directory
//!
//! `{ATM_HOME}/.config/atm/agent-sessions/{team}/{agent_id}/stdin_queue/`
//! (uses [`agent_team_mail_core::home::get_home_dir`] for `ATM_HOME` — cross-platform,
//! no raw `HOME`/`USERPROFILE`).
//!
//! ## Drain triggers
//!
//! Drain is triggered either:
//! - When an `idle` JSONL event is detected on the Codex stdout stream, or
//! - When a 30-second timeout fires without a prior drain.
//!
//! ## TTL cleanup
//!
//! Entries (`.json` and `.lock`) older than 10 minutes are deleted on drain.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::io::AsyncWrite;
use tokio::sync::Mutex;

use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};

/// Returns the queue directory for the given team and agent.
///
/// Uses [`agent_team_mail_core::home::get_home_dir`] for cross-platform home dir.
pub fn queue_dir(team: &str, agent_id: &str) -> anyhow::Result<PathBuf> {
    let home = agent_team_mail_core::home::get_home_dir()?;
    Ok(home
        .join(".config/atm/agent-sessions")
        .join(team)
        .join(agent_id)
        .join("stdin_queue"))
}

/// Write a message to the queue as `{uuid}.json`.
///
/// Creates the queue directory if it does not exist.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined or if file I/O fails.
pub async fn enqueue(team: &str, agent_id: &str, content: &str) -> anyhow::Result<()> {
    let dir = queue_dir(team, agent_id)?;
    tokio::fs::create_dir_all(&dir).await?;

    let id = uuid::Uuid::new_v4();
    let path = dir.join(format!("{id}.json"));
    tokio::fs::write(&path, content.as_bytes()).await?;

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm-agent-mcp",
        action: "stdin_queue_enqueue",
        team: Some(team.to_string()),
        result: Some(format!("agent:{agent_id}")),
        ..Default::default()
    });

    Ok(())
}

/// Drain all unclaimed `*.json` files from the queue.
///
/// For each `{uuid}.json` file:
/// 1. Attempt to atomically create `{uuid}.lock` with `create_new(true)`.  If
///    the lock file already exists, another drainer owns this entry — skip it.
/// 2. Read `{uuid}.json`.
/// 3. Write content + `\n` to the provided stdin writer.
/// 4. On success: delete `{uuid}.json` then `{uuid}.lock`.
/// 5. On write failure: delete `{uuid}.lock` only; leave `{uuid}.json` so the
///    next drain cycle can retry.
///
/// Also deletes any files (`.json` or `.lock`) older than `ttl`.
///
/// Returns the number of messages drained.
///
/// # Errors
///
/// Returns an error only on fatal I/O errors (directory read failure).
/// Individual file claim/write failures are logged and skipped.
pub async fn drain(
    team: &str,
    agent_id: &str,
    stdin: &Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    ttl: Duration,
) -> anyhow::Result<usize> {
    let dir = queue_dir(team, agent_id)?;

    if !dir.exists() {
        return Ok(0);
    }

    // Clean up stale entries first
    let _ = cleanup_ttl(&dir, ttl).await;

    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };

    let mut drained = 0usize;

    // Collect .json files first, then process them
    let mut json_files = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            json_files.push(path);
        }
    }

    for path in json_files {
        let lock_path = path.with_extension("lock");

        // Atomically claim this entry by creating the lock file with O_CREAT|O_EXCL
        // (create_new(true)). On both POSIX and Windows this is a single atomic
        // kernel operation — exactly one concurrent caller will succeed.
        let claim_result = tokio::task::spawn_blocking({
            let lock_path = lock_path.clone();
            move || {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&lock_path)
            }
        })
        .await;

        match claim_result {
            Ok(Ok(_file)) => {
                // Lock acquired — _file is intentionally dropped here; the lock is
                // the file's *existence*, not a held descriptor.
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Another drainer holds the lock for this entry.
                continue;
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    path = %lock_path.display(),
                    error = %e,
                    "unexpected error creating stdin queue lock file; skipping"
                );
                continue;
            }
            Err(join_err) => {
                tracing::warn!(
                    path = %lock_path.display(),
                    error = %join_err,
                    "spawn_blocking panicked creating stdin queue lock; skipping"
                );
                continue;
            }
        }

        // Lock is ours.  Read the source file.
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to read stdin queue file; releasing lock"
                );
                let _ = tokio::fs::remove_file(&lock_path).await;
                continue;
            }
        };

        // Write content to stdin.
        let write_result = {
            let mut guard = stdin.lock().await;
            crate::framing::write_newline_delimited(&mut **guard, content.trim()).await
        };

        match write_result {
            Ok(()) => {
                // Success: remove the source file then the lock.
                let _ = tokio::fs::remove_file(&path).await;
                let _ = tokio::fs::remove_file(&lock_path).await;
                drained += 1;
            }
            Err(e) => {
                // Write failed: release the lock only, leave {uuid}.json for retry.
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "stdin queue write failed; releasing lock for retry"
                );
                let _ = tokio::fs::remove_file(&lock_path).await;
            }
        }
    }

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm-agent-mcp",
        action: "stdin_queue_drain",
        team: Some(team.to_string()),
        result: Some(format!("drained:{drained}")),
        ..Default::default()
    });

    Ok(drained)
}

/// Delete all entries in the queue older than `ttl`.
///
/// Removes files with `.json` or `.lock` extensions whose modification time
/// predates `now - ttl`.  Stale `.lock` files indicate a drainer that crashed
/// after acquiring the lock but before completing delivery.
async fn cleanup_ttl(dir: &Path, ttl: Duration) -> anyhow::Result<usize> {
    let cutoff = SystemTime::now()
        .checked_sub(ttl)
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return Ok(0),
    };

    let mut removed = 0usize;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("json") && ext != Some("lock") {
            continue;
        }

        let metadata = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(_) => continue,
        };

        let mtime = match metadata.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };

        if mtime < cutoff && tokio::fs::remove_file(&path).await.is_ok() {
            removed += 1;
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Helper: create a stdin_queue directory inside a tempdir and set ATM_HOME
    /// to redirect `get_home_dir()`.
    fn setup_env(tmp: &tempfile::TempDir) -> (String, String) {
        let team = "test-team";
        let agent_id = "test-agent";
        // Set ATM_HOME so get_home_dir() returns the tempdir.
        // SAFETY: these tests run serially (via #[serial_test::serial]) so
        // no other thread reads ATM_HOME concurrently.
        unsafe {
            std::env::set_var("ATM_HOME", tmp.path());
        }
        (team.to_string(), agent_id.to_string())
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn enqueue_creates_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (team, agent_id) = setup_env(&tmp);

        enqueue(&team, &agent_id, r#"{"hello":"world"}"#)
            .await
            .unwrap();

        let dir = queue_dir(&team, &agent_id).unwrap();
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        let mut count = 0;
        while let Ok(Some(entry)) = entries.next_entry().await {
            assert_eq!(
                entry.path().extension().and_then(|e| e.to_str()),
                Some("json")
            );
            count += 1;
        }
        assert_eq!(count, 1);
    }

    /// A shared-buffer capture writer: wraps an `Arc<std::sync::Mutex<Vec<u8>>>` so the test
    /// can inspect written bytes without going through the `Box<dyn AsyncWrite>` indirection.
    struct SharedCapWriter(Arc<std::sync::Mutex<Vec<u8>>>);

    impl SharedCapWriter {
        fn new() -> (Self, Arc<std::sync::Mutex<Vec<u8>>>) {
            let buf = Arc::new(std::sync::Mutex::new(Vec::new()));
            (Self(Arc::clone(&buf)), buf)
        }
    }

    impl AsyncWrite for SharedCapWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.0.lock().unwrap().extend_from_slice(buf);
            std::task::Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn enqueue_drain_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (team, agent_id) = setup_env(&tmp);

        let msg = r#"{"type":"tool_result","data":"hello"}"#;
        enqueue(&team, &agent_id, msg).await.unwrap();
        enqueue(&team, &agent_id, msg).await.unwrap();

        let (writer, captured) = SharedCapWriter::new();
        let stdin: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>> =
            Arc::new(Mutex::new(Box::new(writer)));

        let count = drain(&team, &agent_id, &stdin, Duration::from_secs(600))
            .await
            .unwrap();
        assert_eq!(count, 2);

        let output = captured.lock().unwrap().clone();
        let text = String::from_utf8_lossy(&output);
        // Each message should appear with a trailing newline
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            assert!(line.contains("tool_result"));
        }

        // Queue should be empty after drain
        let dir = queue_dir(&team, &agent_id).unwrap();
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        let mut remaining = 0;
        while let Ok(Some(_)) = entries.next_entry().await {
            remaining += 1;
        }
        assert_eq!(remaining, 0, "all queue files should be removed after drain");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn drain_empty_queue_returns_zero() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (team, agent_id) = setup_env(&tmp);

        let (writer, _captured) = SharedCapWriter::new();
        let stdin: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>> =
            Arc::new(Mutex::new(Box::new(writer)));

        // Queue dir doesn't exist yet -- should return 0, not error
        let count = drain(&team, &agent_id, &stdin, Duration::from_secs(600))
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn concurrent_drain_no_double_delivery() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (team, agent_id) = setup_env(&tmp);

        // Enqueue 5 messages
        for i in 0..5 {
            enqueue(&team, &agent_id, &format!(r#"{{"msg":{i}}}"#))
                .await
                .unwrap();
        }

        let (writer1, cap1) = SharedCapWriter::new();
        let stdin1: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>> =
            Arc::new(Mutex::new(Box::new(writer1)));

        let (writer2, cap2) = SharedCapWriter::new();
        let stdin2: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>> =
            Arc::new(Mutex::new(Box::new(writer2)));

        let team_a = team.clone();
        let agent_a = agent_id.clone();
        let stdin_a = Arc::clone(&stdin1);

        let team_b = team.clone();
        let agent_b = agent_id.clone();
        let stdin_b = Arc::clone(&stdin2);

        let (count_a, count_b) = tokio::join!(
            drain(&team_a, &agent_a, &stdin_a, Duration::from_secs(600)),
            drain(&team_b, &agent_b, &stdin_b, Duration::from_secs(600)),
        );

        let total = count_a.unwrap() + count_b.unwrap();
        assert_eq!(total, 5, "total drained should be exactly 5 (no double delivery)");

        // Verify the captured content has exactly 5 messages across both writers
        let out1 = cap1.lock().unwrap().clone();
        let out2 = cap2.lock().unwrap().clone();
        let text1 = String::from_utf8_lossy(&out1);
        let text2 = String::from_utf8_lossy(&out2);
        let lines1: Vec<&str> = text1.lines().filter(|l| !l.is_empty()).collect();
        let lines2: Vec<&str> = text2.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines1.len() + lines2.len(),
            5,
            "exactly 5 messages should be delivered across both drains"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn ttl_cleanup_removes_old_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (team, agent_id) = setup_env(&tmp);

        // Enqueue a message
        enqueue(&team, &agent_id, r#"{"old":"message"}"#)
            .await
            .unwrap();

        let dir = queue_dir(&team, &agent_id).unwrap();

        // Manually set mtime to the past by creating a file with old timestamp
        // We simulate "old" by using a TTL of 0 seconds
        let removed = cleanup_ttl(&dir, Duration::from_secs(0)).await.unwrap();
        assert_eq!(removed, 1, "file should be removed with 0-second TTL");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn ttl_cleanup_removes_stale_lock_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (team, agent_id) = setup_env(&tmp);

        let dir = queue_dir(&team, &agent_id).unwrap();
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // Simulate a stale lock file left by a crashed drainer
        let lock_path = dir.join("00000000-0000-0000-0000-000000000001.lock");
        std::fs::write(&lock_path, b"").unwrap();

        let removed = cleanup_ttl(&dir, Duration::from_secs(0)).await.unwrap();
        assert_eq!(removed, 1, "stale lock file should be removed with 0-second TTL");
    }
}
