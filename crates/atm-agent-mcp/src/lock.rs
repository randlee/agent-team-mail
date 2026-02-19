//! Cross-process identity lock files.
//!
//! Each active agent session holds a lock file at
//! `<sessions_dir>/<team>/<identity>.lock` containing a JSON payload:
//!
//! ```json
//! {"pid": 12345, "agent_id": "codex:uuid-here"}
//! ```
//!
//! On startup (or when attempting to register an identity) the lock file is
//! inspected: if the recorded PID is still alive the lock is live; if the
//! process is dead the lock is stale and is silently removed.
//!
//! # Sessions directory
//!
//! The root directory is resolved by [`sessions_dir`]:
//!
//! 1. `ATM_HOME` env var (used in tests for isolation)
//! 2. `dirs::config_dir()` / `atm` / `agent-sessions`
//! 3. `/tmp/atm/agent-sessions` (fallback)
//!
//! # Cross-platform PID liveness
//!
//! On Unix we call `libc::kill(pid, 0)` — signal 0 does not deliver a signal
//! but checks whether the process exists. On Windows we conservatively return
//! `false` (treat all stale locks as dead), which is safe for an MVP.
//!
//! # Same-process coordination
//!
//! Lock files use the OS PID to coordinate *between* processes. Within a single
//! process, the in-memory [`SessionRegistry`] provides the authoritative view.
//! When a lock file's PID matches the current process PID, we consult an
//! in-process registry of actively-held locks to determine whether the lock is
//! live or stale (left over from a previous `ProxyServer` instance in the same
//! process).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use agent_team_mail_core::home::get_home_dir;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// In-process set of `"<team>/<identity>"` strings for actively-held locks.
///
/// This is the authoritative record of which identities this process currently
/// owns. Used to distinguish live same-PID locks from stale ones.
static IN_PROCESS_LOCKS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn in_process_locks() -> &'static Mutex<HashSet<String>> {
    IN_PROCESS_LOCKS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn lock_key(team: &str, identity: &str) -> String {
    format!("{team}/{identity}")
}

/// JSON payload stored in each lock file.
#[derive(Debug, Serialize, Deserialize)]
struct LockPayload {
    pid: u32,
    agent_id: String,
}

/// Return the root directory used for session lock files.
///
/// Priority:
/// 1. `$ATM_HOME/.config/atm/agent-sessions` (set in tests for isolation;
///    handled by [`get_home_dir`])
/// 2. `<home_dir>/.config/atm/agent-sessions` (FR-20.1)
/// 3. `/tmp/.config/atm/agent-sessions` (last-resort fallback)
pub fn sessions_dir() -> PathBuf {
    // get_home_dir() already handles ATM_HOME → platform home priority.
    // FR-20.1 specifies ~/.config/atm/agent-sessions as the canonical path.
    get_home_dir()
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join(".config")
        .join("atm")
        .join("agent-sessions")
}

/// Compute the lock file path for `(team, identity)`.
fn lock_path(team: &str, identity: &str) -> PathBuf {
    sessions_dir().join(team).join(format!("{identity}.lock"))
}

/// Acquire a lock file for `identity` in `team`.
///
/// Creates the lock file atomically (write-and-rename) with the current
/// process PID and `agent_id`. Returns an error if another live process
/// already holds the lock, or if this process already holds the lock for
/// this identity (detected via the in-process lock set).
///
/// # Errors
///
/// Returns `Err` when:
/// - A live process (including this one) already holds the lock.
/// - Filesystem I/O fails (permissions, disk full, etc.).
pub async fn acquire_lock(team: &str, identity: &str, agent_id: &str) -> anyhow::Result<()> {
    let path = lock_path(team, identity);
    let key = lock_key(team, identity);

    // Check in-process lock first (same-process conflict detection)
    {
        let guard = in_process_locks().lock().unwrap();
        if guard.contains(&key) {
            anyhow::bail!("identity '{}' is already locked by this process", identity);
        }
    }

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let payload = LockPayload {
        pid: std::process::id(),
        agent_id: agent_id.to_string(),
    };
    let json = serde_json::to_string(&payload)?;

    // Attempt exclusive create. If the file already exists, reconcile stale-vs-live
    // lock state and retry once after cleaning up stale data.
    for _ in 0..2 {
        match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await
        {
            Ok(mut file) => {
                if let Err(e) = file.write_all(json.as_bytes()).await {
                    let _ = fs::remove_file(&path).await;
                    return Err(e.into());
                }
                if let Err(e) = file.flush().await {
                    let _ = fs::remove_file(&path).await;
                    return Err(e.into());
                }
                // Register in the in-process lock set only after durable write.
                in_process_locks().lock().unwrap().insert(key);
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Some((pid, existing_id)) = check_lock(team, identity).await {
                    anyhow::bail!(
                        "identity '{}' already locked by PID {} (agent_id: {})",
                        identity,
                        pid,
                        existing_id
                    );
                }
                // check_lock() may treat malformed files as stale but cannot remove them.
                // Clean up and retry once.
                let _ = fs::remove_file(&path).await;
            }
            Err(e) => return Err(e.into()),
        }
    }

    anyhow::bail!("failed to acquire lock for identity '{}'", identity)
}

/// Remove the lock file for `identity` in `team`.
///
/// Also removes the entry from the in-process lock set.
/// Silently ignores `NotFound` errors (lock already removed).
pub async fn release_lock(team: &str, identity: &str) -> anyhow::Result<()> {
    let key = lock_key(team, identity);
    in_process_locks().lock().unwrap().remove(&key);

    let path = lock_path(team, identity);
    match fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Check whether a live **external** process holds the lock for `identity` in `team`.
///
/// Returns:
/// - `None` — no lock file exists, or the recorded PID is dead (stale lock
///   is automatically deleted).
/// - `Some((pid, agent_id))` — a live **other** process holds the lock.
///
/// Same-process locks (where the lock file's PID matches the current process)
/// are resolved by consulting the in-process lock set: if this process actively
/// holds the lock the key is in the set and the file is considered live; if the
/// key is absent (left from a prior `ProxyServer` that didn't release cleanly)
/// the lock is stale and is automatically cleaned up.
pub async fn check_lock(team: &str, identity: &str) -> Option<(u32, String)> {
    let path = lock_path(team, identity);

    let contents = fs::read_to_string(&path).await.ok()?;
    let payload: LockPayload = serde_json::from_str(&contents).ok()?;

    let our_pid = std::process::id();
    if payload.pid == our_pid {
        // Consult in-process set to distinguish live vs. stale same-PID locks
        let key = lock_key(team, identity);
        let is_active = in_process_locks().lock().unwrap().contains(&key);
        if is_active {
            // This process actively holds the lock — report as live
            return Some((payload.pid, payload.agent_id));
        }
        // Stale same-PID lock — clean it up
        let _ = fs::remove_file(&path).await;
        return None;
    }

    if is_pid_alive(payload.pid) {
        Some((payload.pid, payload.agent_id))
    } else {
        // Stale lock from a dead process — clean it up
        let _ = fs::remove_file(&path).await;
        None
    }
}

/// Check whether process `pid` is currently alive.
///
/// On Unix sends signal 0 (`kill(pid, 0)`), which tests existence without
/// delivering a signal. On Windows always returns `false` (MVP behaviour:
/// treat all stale locks as dead).
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: kill(pid, 0) does not deliver a signal; it only checks
        // whether the calling process has permission to signal `pid`. A
        // return value of 0 means the process exists.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Run a test with an isolated ATM_HOME temp directory, cleaning up after.
    async fn with_temp_atm_home<F, Fut>(f: F)
    where
        F: FnOnce(tempfile::TempDir) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().to_string();
        // SAFETY: Tests are serialised; no concurrent env-var mutation.
        unsafe { std::env::set_var("ATM_HOME", &path) };
        f(dir).await;
        unsafe { std::env::remove_var("ATM_HOME") };
    }

    #[tokio::test]
    #[serial]
    async fn acquire_and_release_lock() {
        with_temp_atm_home(|_dir| async {
            acquire_lock("test-team", "agent-x", "codex:abc-123")
                .await
                .unwrap();
            let info = check_lock("test-team", "agent-x").await;
            assert!(info.is_some());
            let (_, agent_id) = info.unwrap();
            assert_eq!(agent_id, "codex:abc-123");

            release_lock("test-team", "agent-x").await.unwrap();
            assert!(check_lock("test-team", "agent-x").await.is_none());
        })
        .await;
    }

    #[tokio::test]
    #[serial]
    async fn check_lock_returns_none_for_missing_lock() {
        with_temp_atm_home(|_dir| async {
            let result = check_lock("team-none", "nobody").await;
            assert!(result.is_none());
        })
        .await;
    }

    #[tokio::test]
    #[serial]
    async fn check_lock_reclaims_dead_pid_lock() {
        with_temp_atm_home(|_dir| async {
            // Write a lock file with a definitely-dead PID (PID 0 is never a
            // user process; on Unix kill(0, 0) checks the whole process group
            // which may succeed, so we use a high bogus PID instead).
            let path = lock_path("dead-team", "ghost");
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).await.unwrap();
            }
            // PID 4194304 is beyond the Linux PID range — guaranteed dead.
            let payload = serde_json::json!({"pid": 4_194_304u32, "agent_id": "codex:dead"});
            fs::write(&path, payload.to_string()).await.unwrap();

            let result = check_lock("dead-team", "ghost").await;
            // Should be None (stale, cleaned up)
            assert!(result.is_none());
            // Lock file should be removed
            assert!(!path.exists());
        })
        .await;
    }

    #[tokio::test]
    #[serial]
    async fn acquire_live_lock_fails() {
        with_temp_atm_home(|_dir| async {
            acquire_lock("team-live", "live-agent", "codex:first")
                .await
                .unwrap();
            // Second acquire on same identity should fail
            let result = acquire_lock("team-live", "live-agent", "codex:second").await;
            assert!(result.is_err());
            release_lock("team-live", "live-agent").await.unwrap();
        })
        .await;
    }

    #[tokio::test]
    #[serial]
    async fn release_nonexistent_lock_is_ok() {
        with_temp_atm_home(|_dir| async {
            // Should not error
            release_lock("ghost-team", "ghost-agent").await.unwrap();
        })
        .await;
    }

    #[test]
    #[serial]
    fn sessions_dir_uses_atm_home() {
        // This test manipulates env; run it without parallelism via serial_test.
        // When ATM_HOME is set, sessions_dir() returns
        // ATM_HOME/.config/atm/agent-sessions (FR-20.1).
        let dir = "/tmp/test-atm-home-lock";
        // SAFETY: serialised by `#[serial]`
        unsafe { std::env::set_var("ATM_HOME", dir) };
        let path = sessions_dir();
        unsafe { std::env::remove_var("ATM_HOME") };
        assert_eq!(
            path,
            PathBuf::from(dir)
                .join(".config")
                .join("atm")
                .join("agent-sessions")
        );
    }

    #[test]
    fn is_pid_alive_self() {
        // The current process is definitely alive
        let alive = is_pid_alive(std::process::id());
        // On Unix this must be true; on Windows we always return false (MVP)
        #[cfg(unix)]
        assert!(alive, "current process should be alive");
        #[cfg(not(unix))]
        let _ = alive;
    }
}
