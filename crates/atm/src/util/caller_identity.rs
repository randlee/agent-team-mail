//! Shared caller/session identity resolution for ATM command paths.
//!
//! This module centralizes caller session lookup so send/read/register/doctor
//! follow one contract and emit stable ambiguity/unresolved error codes.

use anyhow::{Result, bail};

use crate::util::hook_identity::{read_hook_file, read_session_file};

pub const CALLER_AMBIGUOUS: &str = "CALLER_AMBIGUOUS";
pub const CALLER_UNRESOLVED: &str = "CALLER_UNRESOLVED";

fn env_var_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn daemon_resolution_enabled() -> bool {
    // Unit tests should be deterministic and not depend on an externally running daemon.
    if cfg!(test) {
        return false;
    }
    true
}

fn explicit_session_override() -> Option<String> {
    // Unit tests should not pick up ambient shell session variables.
    if cfg!(test) {
        return None;
    }
    env_var_nonempty("ATM_SESSION_ID")
}

fn classify_session_file_error(
    team: Option<&str>,
    identity: Option<&str>,
    err: anyhow::Error,
) -> Result<()> {
    let rendered = err.to_string();
    if rendered.contains("Ambiguous:") {
        let team_txt = team.unwrap_or("<unknown-team>");
        let identity_txt = identity.unwrap_or("<unknown-identity>");
        bail!(
            "{}: multiple active sessions found for {}@{}. \
             Set ATM_SESSION_ID=<full-session-id> (or runtime session env) and retry. \
             Details: {}",
            CALLER_AMBIGUOUS,
            identity_txt,
            team_txt,
            rendered
        );
    }
    // Treat non-ambiguity session-file failures as non-authoritative bootstrap failures.
    Ok(())
}

/// Resolve caller session id, returning `None` only when no usable session id was found.
///
/// Precedence:
/// 1) `ATM_SESSION_ID`
/// 2) daemon session registry (team+identity scoped, alive only)
/// 3) hook file session id
/// 4) runtime env ids (`CLAUDE_SESSION_ID`, `CODEX_THREAD_ID`)
/// 5) session file bootstrap (team+identity scoped)
pub fn resolve_caller_session_id_optional(
    team: Option<&str>,
    identity: Option<&str>,
) -> Result<Option<String>> {
    if let Some(v) = explicit_session_override() {
        return Ok(Some(v));
    }

    if daemon_resolution_enabled()
        && let (Some(t), Some(id)) = (team, identity)
        && let Ok(Some(info)) = agent_team_mail_core::daemon_client::query_session_for_team(t, id)
        && info.alive
    {
        let sid = info.session_id.trim();
        if !sid.is_empty() {
            return Ok(Some(sid.to_string()));
        }
    }

    if let Ok(Some(hook)) = read_hook_file() {
        let sid = hook.session_id.trim();
        if !sid.is_empty() {
            return Ok(Some(sid.to_string()));
        }
    }

    if let Some(v) = env_var_nonempty("CLAUDE_SESSION_ID") {
        return Ok(Some(v));
    }
    if !cfg!(test)
        && let Some(v) = env_var_nonempty("CODEX_THREAD_ID")
    {
        return Ok(Some(v));
    }

    if let (Some(t), Some(id)) = (team, identity) {
        match read_session_file(t, id) {
            Ok(Some(v)) => return Ok(Some(v)),
            Ok(None) => {}
            Err(e) => classify_session_file_error(team, identity, e)?,
        }
    }

    Ok(None)
}

/// Resolve caller session id and fail deterministically if unresolved.
pub fn resolve_caller_session_id_required(
    team: Option<&str>,
    identity: Option<&str>,
) -> Result<String> {
    if let Some(sid) = resolve_caller_session_id_optional(team, identity)? {
        return Ok(sid);
    }

    let team_txt = team.unwrap_or("<unknown-team>");
    let identity_txt = identity.unwrap_or("<unknown-identity>");
    bail!(
        "{}: unable to resolve caller session for {}@{}. \
         Run from a managed session with hooks enabled or set ATM_SESSION_ID=<full-session-id>.",
        CALLER_UNRESOLVED,
        identity_txt,
        team_txt
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::TempDir;

    fn current_ppid_hook_path() -> std::path::PathBuf {
        let ppid = crate::util::hook_identity::get_parent_pid();
        std::env::temp_dir().join(format!("atm-hook-{ppid}.json"))
    }

    fn write_session_file(home: &std::path::Path, team: &str, identity: &str, session_id: &str) {
        let sessions_dir = home
            .join(".claude")
            .join("teams")
            .join(team)
            .join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let data = serde_json::json!({
            "session_id": session_id,
            "team": team,
            "identity": identity,
            "pid": 12345,
            "created_at": now,
            "updated_at": now,
        });
        std::fs::write(
            sessions_dir.join(format!("{session_id}.json")),
            serde_json::to_string(&data).unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[serial]
    fn required_unresolved_returns_stable_code() {
        let temp = TempDir::new().unwrap();
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);
        // SAFETY: test-local environment mutation.
        unsafe {
            std::env::set_var("ATM_HOME", temp.path());
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        let err = resolve_caller_session_id_required(Some("atm-dev"), Some("team-lead"))
            .expect_err("expected unresolved caller session");

        // SAFETY: test-local environment mutation cleanup.
        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        assert!(err.to_string().contains(CALLER_UNRESOLVED));
    }

    #[test]
    #[serial]
    fn required_ambiguous_returns_stable_code() {
        let temp = TempDir::new().unwrap();
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);

        write_session_file(temp.path(), "atm-dev", "team-lead", "sid-a");
        write_session_file(temp.path(), "atm-dev", "team-lead", "sid-b");

        // SAFETY: test-local environment mutation.
        unsafe {
            std::env::set_var("ATM_HOME", temp.path());
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        let err = resolve_caller_session_id_required(Some("atm-dev"), Some("team-lead"))
            .expect_err("expected ambiguity");

        // SAFETY: test-local environment mutation cleanup.
        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        assert!(err.to_string().contains(CALLER_AMBIGUOUS));
    }
}
