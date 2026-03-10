//! Shared caller/session identity resolution for ATM command paths.
//!
//! This module centralizes caller session lookup so send/read/register/doctor
//! follow one contract and emit stable ambiguity/unresolved error codes.

use agent_team_mail_core::daemon_client::SessionQueryResult;
use anyhow::{Result, bail};

use crate::util::hook_identity::{read_hook_file, read_session_file};

pub const CALLER_AMBIGUOUS: &str = "CALLER_AMBIGUOUS";
pub const CALLER_UNRESOLVED: &str = "CALLER_UNRESOLVED";

fn normalize_session_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let is_alias = |lhs: &str| {
        matches!(
            lhs.trim(),
            "thread-id" | "thread_id" | "agent-id" | "agent_id" | "session-id" | "session_id"
        )
    };

    for sep in [':', '='] {
        if let Some((lhs, rhs)) = trimmed.split_once(sep)
            && is_alias(lhs)
        {
            let rhs = rhs.trim();
            if !rhs.is_empty() {
                return Some(rhs.to_string());
            }
        }
    }

    Some(trimmed.to_string())
}

fn env_var_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .and_then(|v| normalize_session_id(&v))
}

fn classify_ambiguity_error(
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
    // Treat non-ambiguity failures as non-authoritative bootstrap failures.
    Ok(())
}

fn explicit_session_override() -> Option<String> {
    env_var_nonempty("ATM_SESSION_ID")
}

fn session_from_daemon(info: &SessionQueryResult) -> Option<String> {
    info.runtime_session_id
        .as_deref()
        .and_then(normalize_session_id)
        .or_else(|| normalize_session_id(&info.session_id))
}

fn default_query_daemon_session(team: &str, identity: &str) -> Result<Option<SessionQueryResult>> {
    agent_team_mail_core::daemon_client::query_session_for_team(team, identity)
}

fn resolve_caller_session_id_optional_with_query<F>(
    team: Option<&str>,
    identity: Option<&str>,
    mut query_daemon_session: F,
) -> Result<Option<String>>
where
    F: FnMut(&str, &str) -> Result<Option<SessionQueryResult>>,
{
    if let Some(v) = explicit_session_override() {
        return Ok(Some(v));
    }

    if let (Some(t), Some(id)) = (team, identity) {
        match query_daemon_session(t, id) {
            Ok(Some(info)) if info.alive => {
                if let Some(sid) = session_from_daemon(&info) {
                    return Ok(Some(sid));
                }
            }
            Ok(_) => {}
            Err(e) => classify_ambiguity_error(team, identity, e)?,
        }
    }

    if let Ok(Some(hook)) = read_hook_file()
        && let Some(sid) = normalize_session_id(&hook.session_id)
    {
        return Ok(Some(sid));
    }

    if let Some(v) = env_var_nonempty("CLAUDE_SESSION_ID") {
        return Ok(Some(v));
    }
    if let Some(v) = env_var_nonempty("CODEX_THREAD_ID") {
        return Ok(Some(v));
    }

    if let (Some(t), Some(id)) = (team, identity) {
        match read_session_file(t, id) {
            Ok(Some(v)) => return Ok(normalize_session_id(&v)),
            Ok(None) => {}
            Err(e) => classify_ambiguity_error(team, identity, e)?,
        }
    }

    Ok(None)
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
    resolve_caller_session_id_optional_with_query(team, identity, default_query_daemon_session)
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
    fn normalize_runtime_aliases_to_canonical_session_id() {
        assert_eq!(
            normalize_session_id("thread-id:abc-123").as_deref(),
            Some("abc-123")
        );
        assert_eq!(
            normalize_session_id("agent_id = xyz-789").as_deref(),
            Some("xyz-789")
        );
        assert_eq!(
            normalize_session_id("session_id: sid-0001").as_deref(),
            Some("sid-0001")
        );
    }

    #[test]
    #[serial]
    fn explicit_atm_session_override_wins() {
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);
        unsafe {
            std::env::set_var("ATM_SESSION_ID", "thread-id:override-123");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        let resolved = resolve_caller_session_id_optional_with_query(
            Some("atm-dev"),
            Some("team-lead"),
            |_team, _identity| Ok(None),
        )
        .unwrap();

        unsafe {
            std::env::remove_var("ATM_SESSION_ID");
        }

        assert_eq!(resolved.as_deref(), Some("override-123"));
    }

    #[test]
    #[serial]
    fn daemon_session_is_authoritative_when_alive() {
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);
        unsafe {
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        let resolved = resolve_caller_session_id_optional_with_query(
            Some("atm-dev"),
            Some("team-lead"),
            |_team, _identity| {
                Ok(Some(SessionQueryResult {
                    session_id: "session_id:daemon-session-1".to_string(),
                    process_id: std::process::id(),
                    alive: true,
                    runtime: Some("codex".to_string()),
                    runtime_session_id: Some("thread-id:codex-thread-1".to_string()),
                    pane_id: None,
                    runtime_home: None,
                }))
            },
        )
        .unwrap();

        assert_eq!(resolved.as_deref(), Some("codex-thread-1"));
    }

    #[test]
    #[serial]
    fn daemon_ambiguity_surfaces_stable_code() {
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);
        unsafe {
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        let err = resolve_caller_session_id_optional_with_query(
            Some("atm-dev"),
            Some("team-lead"),
            |_team, _identity| bail!("Ambiguous: daemon found multiple sessions"),
        )
        .expect_err("expected ambiguity");

        assert!(err.to_string().contains(CALLER_AMBIGUOUS));
    }

    #[test]
    #[serial]
    fn required_unresolved_returns_stable_code() {
        let temp = TempDir::new().unwrap();
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);
        unsafe {
            std::env::set_var("ATM_HOME", temp.path());
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        let err = resolve_caller_session_id_required(Some("atm-dev"), Some("team-lead"))
            .expect_err("expected unresolved caller session");

        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("ATM_SESSION_ID");
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

        unsafe {
            std::env::set_var("ATM_HOME", temp.path());
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        let err = resolve_caller_session_id_required(Some("atm-dev"), Some("team-lead"))
            .expect_err("expected ambiguity");

        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        assert!(err.to_string().contains(CALLER_AMBIGUOUS));
    }
}
