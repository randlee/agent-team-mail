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
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::util::settings::{config_team_dir_for, get_os_home_dir};
use agent_team_mail_core::consts::SESSION_FILE_TTL_SECS;

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

/// Data stored in a session file written by `session-start.py`.
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

fn session_file_timestamp(data: &SessionFileData) -> Option<f64> {
    let timestamp = data.updated_at.unwrap_or(data.created_at);
    (timestamp > 0.0).then_some(timestamp)
}

fn remove_session_file_best_effort(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn session_file_owned_by_current_user(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        // SAFETY: getuid() has no preconditions and always succeeds on Unix.
        let current_uid = unsafe { libc::getuid() };
        meta.uid() == current_uid
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

/// Scan session files for a matching `team` + `identity`.
///
/// Session files are stored at `{ATM_HOME}/.claude/teams/<team>/sessions/<session_id>.json`.
///
/// Returns:
/// - `Ok(None)` — directory absent or no matching non-stale files found.
/// - `Ok(Some(session_id))` — exactly one active match found.
/// - `Err(e)` — ambiguous (>1 active match); error message instructs the user to
///   set `CLAUDE_SESSION_ID` explicitly.
pub fn read_session_file(team: &str, identity: &str) -> Result<Option<String>> {
    let home = get_os_home_dir()?;
    let sessions_dir = config_team_dir_for(&home, team).join("sessions");
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

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        // Ownership check (Unix only).
        if !session_file_owned_by_current_user(&path) {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => {
                remove_session_file_best_effort(&path);
                continue;
            }
        };
        let data: SessionFileData = match serde_json::from_str(&content) {
            Ok(d) => d,
            Err(_) => {
                remove_session_file_best_effort(&path);
                continue;
            }
        };

        // TTL check: use updated_at if present, else fall back to created_at.
        let Some(timestamp) = session_file_timestamp(&data) else {
            remove_session_file_best_effort(&path);
            continue;
        };
        if (now - timestamp) > SESSION_FILE_TTL_SECS {
            remove_session_file_best_effort(&path);
            continue;
        }

        if data.session_id.trim().is_empty() {
            remove_session_file_best_effort(&path);
            continue;
        }

        if let Some(pid) = data.pid
            && !agent_team_mail_core::pid::is_pid_alive(pid)
        {
            remove_session_file_best_effort(&path);
            continue;
        }

        if data.team == team && data.identity == identity {
            matches.push(data);
        }
    }

    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.into_iter().next().unwrap().session_id)),
        n => {
            let ids: Vec<String> = matches
                .iter()
                .map(|m| {
                    let short = &m.session_id[..m.session_id.len().min(8)];
                    format!("{short}...")
                })
                .collect();
            anyhow::bail!(
                "Ambiguous: {n} active sessions for {identity}@{team}: [{}]. \
                 Export CLAUDE_SESSION_ID=<session-id> to disambiguate.",
                ids.join(", ")
            )
        }
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

    fn set_home_envs(path: &Path) {
        unsafe {
            std::env::set_var("HOME", path);
            std::env::set_var("USERPROFILE", path);
        }
    }

    fn clear_home_envs() {
        unsafe {
            std::env::remove_var("HOME");
            std::env::remove_var("USERPROFILE");
        }
    }

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

    // -----------------------------------------------------------------------
    // read_session_file tests
    // -----------------------------------------------------------------------

    /// Helper: write a session file under the given home directory.
    fn write_session_file(
        home: &std::path::Path,
        team: &str,
        session_id: &str,
        identity: &str,
        created_at: f64,
        updated_at: Option<f64>,
    ) {
        let sessions_dir = home
            .join(".claude")
            .join("teams")
            .join(team)
            .join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let data = serde_json::json!({
            "session_id": session_id,
            "team": team,
            "identity": identity,
            "pid": std::process::id(),
            "created_at": created_at,
            "updated_at": updated_at,
        });
        std::fs::write(
            sessions_dir.join(format!("{session_id}.json")),
            serde_json::to_string(&data).unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[serial]
    fn test_read_session_file_missing_directory_returns_none() {
        let home = tempfile::TempDir::new().unwrap();
        unsafe {
            set_home_envs(home.path());
            std::env::set_var("ATM_HOME", home.path());
        }

        let result = read_session_file("no-team", "agent");
        unsafe {
            clear_home_envs();
            std::env::remove_var("ATM_HOME");
        }

        assert!(
            result.is_ok(),
            "expected Ok when directory absent: {result:?}"
        );
        assert!(result.unwrap().is_none());
    }

    #[test]
    #[serial]
    fn test_read_session_file_single_match_returns_session_id() {
        let home = tempfile::TempDir::new().unwrap();
        unsafe {
            set_home_envs(home.path());
            std::env::set_var("ATM_HOME", home.path());
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        write_session_file(
            home.path(),
            "atm-dev",
            "sid-001",
            "team-lead",
            now,
            Some(now),
        );

        let result = read_session_file("atm-dev", "team-lead");
        unsafe {
            clear_home_envs();
            std::env::remove_var("ATM_HOME");
        }

        let sid = result.expect("expected Ok").expect("expected Some");
        assert_eq!(sid, "sid-001");
    }

    #[test]
    #[serial]
    fn test_read_session_file_stale_file_returns_none() {
        let home = tempfile::TempDir::new().unwrap();
        unsafe {
            set_home_envs(home.path());
            std::env::set_var("ATM_HOME", home.path());
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        // Write a file that is 25 hours old — well beyond the 24h TTL.
        let stale_ts = now - 90_000.0;
        write_session_file(
            home.path(),
            "atm-dev",
            "sid-stale",
            "team-lead",
            stale_ts,
            Some(stale_ts),
        );

        let result = read_session_file("atm-dev", "team-lead");
        unsafe {
            clear_home_envs();
            std::env::remove_var("ATM_HOME");
        }

        assert!(result.is_ok(), "expected Ok: {result:?}");
        assert!(result.unwrap().is_none(), "stale file should be skipped");
    }

    #[test]
    #[serial]
    fn test_read_session_file_updated_at_refreshes_ttl() {
        let home = tempfile::TempDir::new().unwrap();
        unsafe {
            set_home_envs(home.path());
            std::env::set_var("ATM_HOME", home.path());
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        // created_at is 25 hours ago but updated_at is fresh — should not be stale.
        let old_created = now - 90_000.0;
        write_session_file(
            home.path(),
            "atm-dev",
            "sid-fresh",
            "team-lead",
            old_created,
            Some(now),
        );

        let result = read_session_file("atm-dev", "team-lead");
        unsafe {
            clear_home_envs();
            std::env::remove_var("ATM_HOME");
        }

        let sid = result.expect("expected Ok").expect("expected Some");
        assert_eq!(sid, "sid-fresh", "fresh updated_at should keep file alive");
    }

    #[test]
    #[serial]
    fn test_read_session_file_ambiguous_returns_err() {
        let home = tempfile::TempDir::new().unwrap();
        unsafe {
            set_home_envs(home.path());
            std::env::set_var("ATM_HOME", home.path());
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        write_session_file(
            home.path(),
            "atm-dev",
            "sid-aaa111",
            "team-lead",
            now,
            Some(now),
        );
        write_session_file(
            home.path(),
            "atm-dev",
            "sid-bbb222",
            "team-lead",
            now,
            Some(now),
        );

        let result = read_session_file("atm-dev", "team-lead");
        unsafe {
            clear_home_envs();
            std::env::remove_var("ATM_HOME");
        }

        assert!(result.is_err(), "expected Err for ambiguous sessions");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Ambiguous"),
            "error should mention 'Ambiguous': {msg}"
        );
        assert!(
            msg.contains("CLAUDE_SESSION_ID"),
            "error should mention CLAUDE_SESSION_ID: {msg}"
        );
    }

    #[test]
    #[serial]
    fn test_read_session_file_wrong_identity_returns_none() {
        let home = tempfile::TempDir::new().unwrap();
        unsafe {
            set_home_envs(home.path());
            std::env::set_var("ATM_HOME", home.path());
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        write_session_file(
            home.path(),
            "atm-dev",
            "sid-xyz",
            "other-agent",
            now,
            Some(now),
        );

        let result = read_session_file("atm-dev", "team-lead");
        unsafe {
            clear_home_envs();
            std::env::remove_var("ATM_HOME");
        }

        assert!(result.is_ok(), "expected Ok: {result:?}");
        assert!(
            result.unwrap().is_none(),
            "file for different identity should not match"
        );
    }
}
