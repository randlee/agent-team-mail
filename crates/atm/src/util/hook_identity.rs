//! PID-based hook identity file reader.
//!
//! The PreToolUse Bash hook (`atm-identity-write.py`) writes a small JSON file
//! to the system temp directory keyed by its own PID:
//!
//! ```text
//! $TMPDIR/atm-hook-<hook_pid>.json
//! ```
//!
//! Because the hook process and the subsequent Bash shell share the same PID
//! (they are siblings launched by the same agent process), `atm`'s parent PID
//! (`getppid()`) equals the hook file's name suffix.  This module encapsulates
//! that lookup with appropriate validation (TTL, file ownership on Unix).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum age in seconds before a hook file is considered stale.
const HOOK_FILE_TTL_SECS: f64 = 5.0;

/// Data stored in the PID-based hook identity file.
#[derive(Debug, Deserialize)]
pub struct HookFileData {
    /// Parent PID at the time the hook ran (informational only).
    ///
    /// This field is public so callers can log or cross-reference the PID for
    /// debugging.  It is not required for identity resolution itself.
    #[allow(dead_code)]
    pub pid: u32,
    /// Claude Code session ID from the hook payload.
    pub session_id: String,
    /// Agent name resolved from `.atm.toml` identity at hook time.
    pub agent_name: Option<String>,
    /// Unix timestamp (seconds since epoch, fractional) when the file was written.
    pub created_at: f64,
}

/// Return the parent PID of the current process.
///
/// On Unix this calls `libc::getppid()`.  On Windows it queries `sysinfo`.
/// Returns 0 if the PID cannot be determined.
pub fn get_parent_pid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getppid() has no preconditions and always succeeds on Unix.
        (unsafe { libc::getppid() }) as u32
    }

    #[cfg(windows)]
    {
        use sysinfo::{Pid, System};
        let sys = System::new_all();
        let current = std::process::id();
        sys.process(Pid::from_u32(current))
            .and_then(|p| p.parent())
            .map(|pid| pid.as_u32())
            .unwrap_or(0)
    }

    #[cfg(not(any(unix, windows)))]
    {
        0
    }
}

/// Read the PID-based hook identity file written by `atm-identity-write.py`.
///
/// Returns:
/// - `Ok(None)` — file does not exist (hook not configured or non-Bash invocation).
/// - `Ok(Some(data))` — file found and passed all validation checks.
/// - `Err(e)` — file exists but is stale, wrong owner, or unparseable.
pub fn read_hook_file() -> Result<Option<HookFileData>> {
    let ppid = get_parent_pid();
    let hook_path = std::env::temp_dir().join(format!("atm-hook-{ppid}.json"));

    if !hook_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&hook_path)
        .with_context(|| format!("failed to read hook file {}", hook_path.display()))?;

    let data: HookFileData = serde_json::from_str(&content)
        .with_context(|| format!("hook file {} contains invalid JSON", hook_path.display()))?;

    // Staleness check.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let age = now - data.created_at;
    if age > HOOK_FILE_TTL_SECS {
        anyhow::bail!(
            "hook file {} is stale ({:.1}s old, TTL is {:.1}s)",
            hook_path.display(),
            age,
            HOOK_FILE_TTL_SECS
        );
    }

    // Ownership check (Unix only).
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(&hook_path)
            .with_context(|| format!("cannot stat hook file {}", hook_path.display()))?;
        // SAFETY: getuid() has no preconditions and always succeeds on Unix.
        let current_uid = unsafe { libc::getuid() };
        if meta.uid() != current_uid {
            anyhow::bail!(
                "hook file {} is owned by uid {} but current uid is {}; ignoring",
                hook_path.display(),
                meta.uid(),
                current_uid
            );
        }
    }

    Ok(Some(data))
}

/// Read the agent identity from the PID-based hook file.
///
/// Returns:
/// - `Ok(None)` — hook file does not exist (non-Bash invocation or hook not configured).
/// - `Ok(Some(name))` — identity successfully resolved from hook file.
/// - `Err(e)` — hook file exists but failed validation (stale, wrong owner, corrupt JSON).
pub fn read_hook_file_identity() -> Result<Option<String>> {
    match read_hook_file()? {
        None => Ok(None),
        Some(data) => Ok(data.agent_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Write a hook file with a given JSON payload, then swap it into the expected
    /// path for `read_hook_file()` to find.  Returns the temp dir guard so it lives
    /// for the duration of the test.
    fn write_hook_file_for_ppid(data: &serde_json::Value) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let ppid = get_parent_pid();
        let path = dir.path().join(format!("atm-hook-{ppid}.json"));
        std::fs::write(&path, serde_json::to_string(data).unwrap()).unwrap();
        (dir, path)
    }

    #[test]
    fn test_read_hook_file_missing() {
        // Point temp dir somewhere empty — no file should exist.
        // We test indirectly: if get_parent_pid() returns a valid PID, the file
        // under the real temp dir simply won't exist (we haven't created it).
        // This test just ensures the function returns Ok(None) rather than Err.
        //
        // We cannot easily override temp_dir(), so we rely on the file not
        // existing at the standard path.  If by some coincidence a stale hook
        // file exists, the staleness check will convert it to Err; that is also
        // acceptable (the file would be legitimately stale for a test).
        let ppid = get_parent_pid();
        let hook_path = std::env::temp_dir().join(format!("atm-hook-{ppid}.json"));

        // Remove any pre-existing file from a previous run to keep test clean.
        let _ = std::fs::remove_file(&hook_path);

        let result = read_hook_file();
        assert!(
            result.is_ok(),
            "expected Ok(None) when file missing, got {:?}",
            result
        );
        assert!(result.unwrap().is_none());
    }

    #[test]
    #[serial]
    fn test_read_hook_file_stale() {
        let ppid = get_parent_pid();
        let hook_path = std::env::temp_dir().join(format!("atm-hook-{ppid}.json"));

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let stale_data = serde_json::json!({
            "pid": ppid,
            "session_id": "test-session",
            "agent_name": "team-lead",
            "created_at": now - 60.0,  // 60 seconds ago → well beyond TTL
        });
        std::fs::write(&hook_path, serde_json::to_string(&stale_data).unwrap()).unwrap();

        let result = read_hook_file();
        // Clean up before asserting so we don't leave the file around.
        let _ = std::fs::remove_file(&hook_path);

        assert!(result.is_err(), "expected Err for stale file, got {:?}", result);
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("stale"), "error should mention 'stale': {err_str}");
    }

    #[test]
    #[serial]
    fn test_read_hook_file_valid() {
        let ppid = get_parent_pid();
        let hook_path = std::env::temp_dir().join(format!("atm-hook-{ppid}.json"));

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let fresh_data = serde_json::json!({
            "pid": ppid,
            "session_id": "test-session-fresh",
            "agent_name": "team-lead",
            "created_at": now,
        });
        std::fs::write(&hook_path, serde_json::to_string(&fresh_data).unwrap()).unwrap();

        let result = read_hook_file();
        let _ = std::fs::remove_file(&hook_path);

        let data = result.expect("expected Ok(Some(...)) for fresh file").expect("expected Some");
        assert_eq!(data.agent_name.as_deref(), Some("team-lead"));
        assert_eq!(data.session_id, "test-session-fresh");
    }

    #[test]
    #[serial]
    fn test_read_hook_file_malformed_json() {
        let ppid = get_parent_pid();
        let hook_path = std::env::temp_dir().join(format!("atm-hook-{ppid}.json"));

        std::fs::write(&hook_path, b"not valid json {{{").unwrap();

        let result = read_hook_file();
        let _ = std::fs::remove_file(&hook_path);

        assert!(result.is_err(), "expected Err for malformed JSON");
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("invalid JSON") || err_str.contains("JSON"),
            "error should mention JSON: {err_str}"
        );
    }

    #[test]
    fn test_read_hook_file_identity_none_when_missing() {
        let ppid = get_parent_pid();
        let hook_path = std::env::temp_dir().join(format!("atm-hook-{ppid}.json"));
        let _ = std::fs::remove_file(&hook_path);

        let result = read_hook_file_identity();
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
