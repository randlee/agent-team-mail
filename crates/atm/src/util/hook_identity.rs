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

use crate::util::settings::get_home_dir;

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

/// Data stored in a session file.
#[derive(Debug, Deserialize)]
pub struct SessionFileData {
    pub session_id: String,
    pub team: String,
    pub identity: String,
    #[allow(dead_code)]
    pub pid: Option<u32>,
    pub created_at: f64,
    pub updated_at: Option<f64>,
}

/// Maximum age in seconds before a session file is considered stale (24 hours).
const SESSION_FILE_TTL_SECS: f64 = 86400.0;

/// Scan session files for a matching team+identity.
///
/// Ambiguity handling:
/// - 0 matches → Ok(None)
/// - 1 match → Ok(Some(session_id))
/// - >1 matches → Err with instruction to set CLAUDE_SESSION_ID
pub fn read_session_file(team: &str, identity: &str) -> Result<Option<String>> {
    let home = get_home_dir()?;
    let sessions_dir = home.join(".claude/sessions");
    if !sessions_dir.is_dir() {
        return Ok(None);
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let mut matches: Vec<SessionFileData> = Vec::new();
    let entries = match std::fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        // Ownership check (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let Ok(meta) = std::fs::metadata(&path) {
                let current_uid = unsafe { libc::getuid() };
                if meta.uid() != current_uid {
                    continue;
                }
            }
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let data: SessionFileData = match serde_json::from_str(&content) {
            Ok(d) => d,
            Err(_) => continue,
        };

        // TTL check: use updated_at if present, else created_at
        let timestamp = data.updated_at.unwrap_or(data.created_at);
        if timestamp <= 0.0 {
            continue; // Invalid timestamp
        }
        let age = now - timestamp;
        if age > SESSION_FILE_TTL_SECS {
            continue; // Stale
        }

        if data.team == team && data.identity == identity {
            matches.push(data);
        }
    }

    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.into_iter().next().unwrap().session_id)),
        n => anyhow::bail!(
            "Ambiguous: {n} active sessions for {identity}@{team}. \
             Export CLAUDE_SESSION_ID=<your-session-id> to disambiguate."
        ),
    }
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
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    #[serial]
    fn test_read_hook_file_missing() {
        // All hook_identity tests share the same path (atm-hook-<ppid>.json) so
        // they MUST all be #[serial] to prevent concurrent interference.
        let ppid = get_parent_pid();
        let hook_path = std::env::temp_dir().join(format!("atm-hook-{ppid}.json"));

        // Pre-test cleanup: remove any file left by a prior test or run.
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

        // Pre-test cleanup.
        let _ = std::fs::remove_file(&hook_path);

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

        assert!(
            result.is_err(),
            "expected Err for stale file, got {:?}",
            result
        );
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("stale"),
            "error should mention 'stale': {err_str}"
        );
    }

    #[test]
    #[serial]
    fn test_read_hook_file_valid() {
        let ppid = get_parent_pid();
        let hook_path = std::env::temp_dir().join(format!("atm-hook-{ppid}.json"));

        // Pre-test cleanup.
        let _ = std::fs::remove_file(&hook_path);

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

        let data = result
            .expect("expected Ok(Some(...)) for fresh file")
            .expect("expected Some");
        assert_eq!(data.agent_name.as_deref(), Some("team-lead"));
        assert_eq!(data.session_id, "test-session-fresh");
    }

    #[test]
    #[serial]
    fn test_read_hook_file_malformed_json() {
        let ppid = get_parent_pid();
        let hook_path = std::env::temp_dir().join(format!("atm-hook-{ppid}.json"));

        // Pre-test cleanup: ensure no leftover valid file from a prior test.
        let _ = std::fs::remove_file(&hook_path);

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
    #[serial]
    fn test_read_hook_file_identity_none_when_missing() {
        let ppid = get_parent_pid();
        let hook_path = std::env::temp_dir().join(format!("atm-hook-{ppid}.json"));
        // Pre-test cleanup.
        let _ = std::fs::remove_file(&hook_path);

        let result = read_hook_file_identity();
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    // ── Session file tests ──────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_read_session_file_no_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let old_home = std::env::var("ATM_HOME").ok();
        unsafe { std::env::set_var("ATM_HOME", dir.path()); }

        let result = read_session_file("test-team", "team-lead");

        unsafe {
            match old_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }

        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    #[serial]
    fn test_read_session_file_single_match() {
        let dir = tempfile::TempDir::new().unwrap();
        let sessions_dir = dir.path().join(".claude/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
        let data = serde_json::json!({
            "session_id": "test-session-123",
            "team": "test-team",
            "identity": "team-lead",
            "pid": 12345,
            "created_at": now,
        });
        std::fs::write(
            sessions_dir.join("test-session-123.json"),
            serde_json::to_string(&data).unwrap(),
        ).unwrap();

        let old_home = std::env::var("ATM_HOME").ok();
        unsafe { std::env::set_var("ATM_HOME", dir.path()); }

        let result = read_session_file("test-team", "team-lead");

        unsafe {
            match old_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }

        let sid = result.unwrap().unwrap();
        assert_eq!(sid, "test-session-123");
    }

    #[test]
    #[serial]
    fn test_read_session_file_ambiguous() {
        let dir = tempfile::TempDir::new().unwrap();
        let sessions_dir = dir.path().join(".claude/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
        for i in 1..=2 {
            let data = serde_json::json!({
                "session_id": format!("session-{i}"),
                "team": "test-team",
                "identity": "team-lead",
                "pid": 12345 + i,
                "created_at": now,
            });
            std::fs::write(
                sessions_dir.join(format!("session-{i}.json")),
                serde_json::to_string(&data).unwrap(),
            ).unwrap();
        }

        let old_home = std::env::var("ATM_HOME").ok();
        unsafe { std::env::set_var("ATM_HOME", dir.path()); }

        let result = read_session_file("test-team", "team-lead");

        unsafe {
            match old_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Ambiguous"), "error should mention ambiguity: {err}");
        assert!(err.contains("CLAUDE_SESSION_ID"), "error should mention env var: {err}");
    }

    #[test]
    #[serial]
    fn test_read_session_file_stale_skipped() {
        let dir = tempfile::TempDir::new().unwrap();
        let sessions_dir = dir.path().join(".claude/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
        let stale_time = now - 100_000.0; // Well beyond 24h TTL
        let data = serde_json::json!({
            "session_id": "stale-session",
            "team": "test-team",
            "identity": "team-lead",
            "pid": 12345,
            "created_at": stale_time,
        });
        std::fs::write(
            sessions_dir.join("stale-session.json"),
            serde_json::to_string(&data).unwrap(),
        ).unwrap();

        let old_home = std::env::var("ATM_HOME").ok();
        unsafe { std::env::set_var("ATM_HOME", dir.path()); }

        let result = read_session_file("test-team", "team-lead");

        unsafe {
            match old_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }

        assert!(result.is_ok());
        assert!(result.unwrap().is_none(), "stale session should be skipped");
    }

    #[test]
    #[serial]
    fn test_read_session_file_updated_at_extends_freshness() {
        let dir = tempfile::TempDir::new().unwrap();
        let sessions_dir = dir.path().join(".claude/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
        // created_at is stale but updated_at is fresh
        let data = serde_json::json!({
            "session_id": "refreshed-session",
            "team": "test-team",
            "identity": "team-lead",
            "pid": 12345,
            "created_at": now - 100_000.0,
            "updated_at": now - 10.0,
        });
        std::fs::write(
            sessions_dir.join("refreshed-session.json"),
            serde_json::to_string(&data).unwrap(),
        ).unwrap();

        let old_home = std::env::var("ATM_HOME").ok();
        unsafe { std::env::set_var("ATM_HOME", dir.path()); }

        let result = read_session_file("test-team", "team-lead");

        unsafe {
            match old_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }

        let sid = result.unwrap().unwrap();
        assert_eq!(sid, "refreshed-session");
    }

    #[test]
    #[serial]
    fn test_read_session_file_custom_atm_home() {
        let dir = tempfile::TempDir::new().unwrap();
        let sessions_dir = dir.path().join(".claude/sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
        let data = serde_json::json!({
            "session_id": "custom-home-session",
            "team": "test-team",
            "identity": "team-lead",
            "pid": 12345,
            "created_at": now,
        });
        std::fs::write(
            sessions_dir.join("custom-home-session.json"),
            serde_json::to_string(&data).unwrap(),
        ).unwrap();

        let old_home = std::env::var("ATM_HOME").ok();
        unsafe { std::env::set_var("ATM_HOME", dir.path()); }

        let result = read_session_file("test-team", "team-lead");

        unsafe {
            match old_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }

        let sid = result.unwrap().unwrap();
        assert_eq!(sid, "custom-home-session");
    }
}
