//! Shared caller/session identity resolution for ATM command paths.
//!
//! This module centralizes caller session lookup so send/read/register/doctor
//! follow one contract and emit stable ambiguity/unresolved error codes.

use agent_team_mail_core::daemon_client::SessionQueryResult;
use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::util::hook_identity::{read_hook_file, read_session_file};
use crate::util::settings::get_home_dir;

/// Returned when multiple caller/session resolution candidates remain valid
/// and ATM cannot choose one canonical caller identity automatically.
pub const CALLER_AMBIGUOUS: &str = "CALLER_AMBIGUOUS";
/// Returned when caller/session resolution exhausts all supported sources
/// without finding a canonical caller identity.
pub const CALLER_UNRESOLVED: &str = "CALLER_UNRESOLVED";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallerRuntime {
    Claude,
    Codex,
    Gemini,
    Opencode,
    Unknown,
}

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

fn nonempty_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_runtime(value: &str) -> CallerRuntime {
    match value.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => CallerRuntime::Claude,
        "codex" | "codex-cli" => CallerRuntime::Codex,
        "gemini" | "gemini-cli" => CallerRuntime::Gemini,
        "opencode" => CallerRuntime::Opencode,
        _ => CallerRuntime::Unknown,
    }
}

fn runtime_from_process_observation(comm: &str, args: &str) -> CallerRuntime {
    let comm = comm
        .rsplit('/')
        .next()
        .unwrap_or(comm)
        .trim()
        .to_ascii_lowercase();
    let args = args.trim().to_ascii_lowercase();
    match comm.as_str() {
        "claude" => CallerRuntime::Claude,
        "codex" => CallerRuntime::Codex,
        "node" if args.contains("gemini") => CallerRuntime::Gemini,
        _ => CallerRuntime::Unknown,
    }
}

fn runtime_from_process_tree() -> CallerRuntime {
    use sysinfo::{Pid, System};

    let sys = System::new_all();
    let mut cursor = Pid::from_u32(std::process::id());

    for _ in 0..16 {
        let Some(proc_info) = sys.process(cursor) else {
            break;
        };

        let comm = proc_info.name().to_string_lossy();
        let args = proc_info
            .cmd()
            .iter()
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        let runtime = runtime_from_process_observation(&comm, &args);
        if runtime != CallerRuntime::Unknown {
            return runtime;
        }

        let Some(parent) = proc_info.parent() else {
            break;
        };
        cursor = parent;
    }

    CallerRuntime::Unknown
}

fn daemon_resolution_enabled() -> bool {
    // Unit tests should be deterministic and not depend on an externally running daemon.
    !running_test_harness()
}

fn explicit_session_override() -> Option<String> {
    // Unit tests should not pick up ambient shell session variables.
    if running_test_harness() {
        return None;
    }
    env_var_nonempty("ATM_SESSION_ID")
}

fn running_test_harness() -> bool {
    if let Ok(exe) = std::env::current_exe()
        && exe
            .components()
            .any(|component| component.as_os_str() == "deps")
    {
        return true;
    }
    false
}

fn query_daemon_session(team: Option<&str>, identity: Option<&str>) -> Option<SessionQueryResult> {
    if !daemon_resolution_enabled() {
        return None;
    }
    let (Some(t), Some(id)) = (team, identity) else {
        return None;
    };

    agent_team_mail_core::daemon_client::query_session_for_team(t, id)
        .ok()
        .flatten()
}

fn runtime_hint(daemon: Option<&SessionQueryResult>) -> CallerRuntime {
    let _ = daemon;
    // Direct runtime signals are more specific than an ambient ancestor process.
    if let Some(runtime) = env_var_nonempty("ATM_RUNTIME") {
        let parsed = parse_runtime(&runtime);
        if parsed != CallerRuntime::Unknown {
            return parsed;
        }
    }

    if env_var_nonempty("CODEX_THREAD_ID").is_some() {
        return CallerRuntime::Codex;
    }

    let traced = runtime_from_process_tree();
    if traced != CallerRuntime::Unknown {
        return traced;
    }

    if env_var_nonempty("CLAUDE_SESSION_ID").is_some() {
        return CallerRuntime::Claude;
    }

    CallerRuntime::Unknown
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
    Ok(())
}

fn resolve_from_daemon(
    daemon: Option<&SessionQueryResult>,
    runtime: CallerRuntime,
) -> Option<String> {
    let info = daemon?;
    if !info.alive {
        return None;
    }

    if matches!(
        runtime,
        CallerRuntime::Codex | CallerRuntime::Gemini | CallerRuntime::Opencode
    ) && let Some(runtime_sid) = info
        .runtime_session_id
        .as_deref()
        .and_then(nonempty_trimmed)
    {
        return Some(runtime_sid);
    }

    nonempty_trimmed(&info.session_id)
}

fn resolve_from_session_file(team: Option<&str>, identity: Option<&str>) -> Result<Option<String>> {
    if let (Some(t), Some(id)) = (team, identity) {
        match read_session_file_scoped(t, id) {
            Ok(Some(v)) => return Ok(Some(v)),
            Ok(None) => {}
            Err(e) => classify_ambiguity_error(team, identity, e)?,
        }
    }
    Ok(None)
}

#[cfg(not(test))]
fn read_session_file_scoped(team: &str, identity: &str) -> Result<Option<String>> {
    read_session_file(team, identity)
}

#[cfg(test)]
fn read_session_file_scoped(team: &str, identity: &str) -> Result<Option<String>> {
    let Some(test_home) = env_var_nonempty("ATM_TEST_HOME") else {
        return read_session_file(team, identity);
    };

    let original_home = std::env::var("ATM_HOME").ok();
    // SAFETY: test-only temporary process-local override for deterministic fixture lookup.
    unsafe {
        std::env::set_var("ATM_HOME", test_home);
    }
    let result = read_session_file(team, identity);
    // SAFETY: restoring prior process-local env value after scoped read.
    unsafe {
        match original_home {
            Some(v) => std::env::set_var("ATM_HOME", v),
            None => std::env::remove_var("ATM_HOME"),
        }
    }
    result
}

fn resolve_runtime_native_env(runtime: CallerRuntime) -> Option<String> {
    match runtime {
        // Claude: do NOT short-circuit here — hook file takes priority over
        // CLAUDE_SESSION_ID and is resolved inside resolve_claude_session().
        CallerRuntime::Claude => None,
        CallerRuntime::Codex => env_var_nonempty("CODEX_THREAD_ID"),
        CallerRuntime::Gemini | CallerRuntime::Opencode => None,
        CallerRuntime::Unknown => None,
    }
}

fn resolve_from_hook() -> Option<String> {
    if running_test_harness() && env_var_nonempty("ATM_TEST_ENABLE_HOOK_RESOLUTION").is_none() {
        return None;
    }
    read_hook_file()
        .ok()
        .flatten()
        .and_then(|hook| nonempty_trimmed(&hook.session_id))
}

fn resolve_codex_session(team: Option<&str>, identity: Option<&str>) -> Result<Option<String>> {
    if let Some(thread_id) = env_var_nonempty("CODEX_THREAD_ID") {
        return Ok(Some(thread_id));
    }

    resolve_from_session_file(team, identity)
}

fn resolve_gemini_session(team: Option<&str>, identity: Option<&str>) -> Result<Option<String>> {
    if let Some(hook_session) = resolve_from_hook() {
        return Ok(Some(hook_session));
    }

    if let Some(cli_session) = resolve_gemini_session_from_cli()? {
        return Ok(Some(cli_session));
    }

    match resolve_gemini_session_from_files() {
        Ok(Some(session_id)) => return Ok(Some(session_id)),
        Ok(None) => {}
        Err(e) => classify_ambiguity_error(team, identity, e)?,
    }

    resolve_from_session_file(team, identity)
}

fn resolve_claude_session(team: Option<&str>, identity: Option<&str>) -> Result<Option<String>> {
    if let Some(hook_session) = resolve_from_hook() {
        return Ok(Some(hook_session));
    }

    if let Some(env_session) = env_var_nonempty("CLAUDE_SESSION_ID") {
        return Ok(Some(env_session));
    }

    resolve_from_session_file(team, identity)
}

fn resolve_unknown_runtime_session(
    team: Option<&str>,
    identity: Option<&str>,
) -> Result<Option<String>> {
    if let Some(hook_session) = resolve_from_hook() {
        return Ok(Some(hook_session));
    }

    if let Some(env_session) = env_var_nonempty("CLAUDE_SESSION_ID") {
        return Ok(Some(env_session));
    }

    let from_file = resolve_from_session_file(team, identity)?;
    Ok(from_file)
}

fn resolve_gemini_session_from_cli() -> Result<Option<String>> {
    if cfg!(test) {
        return Ok(None);
    }

    let output = match Command::new("gemini").arg("--list-sessions").output() {
        Ok(out) => out,
        Err(_) => return Ok(None),
    };

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_gemini_list_sessions_output(&stdout))
}

fn parse_gemini_list_sessions_output(output: &str) -> Option<String> {
    let uuid_re = Regex::new(
        r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b",
    )
    .expect("uuid regex");
    let hash_re = Regex::new(r"\b[0-9a-fA-F]{8,64}\b").expect("hash regex");

    for line in output.lines() {
        if let Some(found) = uuid_re.find(line) {
            return Some(found.as_str().to_string());
        }
    }

    for line in output.lines() {
        for m in hash_re.find_iter(line) {
            let token = m.as_str();
            if token.chars().any(|c| c.is_ascii_alphabetic()) {
                return Some(token.to_string());
            }
        }
    }

    None
}

fn resolve_gemini_session_from_files() -> Result<Option<String>> {
    let Some(tmp_root) = gemini_tmp_root() else {
        return Ok(None);
    };

    let candidates = gemini_project_candidates(&tmp_root)?;
    if candidates.is_empty() {
        return Ok(None);
    }

    for project_dir in candidates {
        if !project_dir.is_dir() {
            continue;
        }

        if let Some(session_id) = parse_gemini_logs_session(&project_dir)? {
            return Ok(Some(session_id));
        }

        if let Some(session_id) = parse_gemini_chat_sessions(&project_dir)? {
            return Ok(Some(session_id));
        }
    }

    Ok(None)
}

fn gemini_tmp_root() -> Option<PathBuf> {
    if let Some(path) = env_var_nonempty("ATM_GEMINI_TMP_DIR") {
        return Some(PathBuf::from(path));
    }

    let home = get_home_dir().ok()?;
    Some(home.join(".gemini").join("tmp"))
}

fn gemini_project_candidates(tmp_root: &Path) -> Result<Vec<PathBuf>> {
    let project_dir = if let Some(project) = env_var_nonempty("ATM_PROJECT_DIR") {
        PathBuf::from(project)
    } else {
        std::env::current_dir().context("failed to determine current directory")?
    };

    let mut candidates = Vec::new();

    if let Some(name) = project_dir.file_name().and_then(|s| s.to_str())
        && !name.trim().is_empty()
    {
        candidates.push(tmp_root.join(name.trim()));
    }

    let sanitized = sanitize_project_path(&project_dir);
    if !sanitized.is_empty() {
        candidates.push(tmp_root.join(&sanitized));
    }

    candidates.sort();
    candidates.dedup();

    Ok(candidates)
}

fn sanitize_project_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn parse_gemini_logs_session(project_dir: &Path) -> Result<Option<String>> {
    let logs_path = project_dir.join("logs.json");
    if !logs_path.is_file() {
        return Ok(None);
    }

    let text = std::fs::read_to_string(&logs_path)
        .with_context(|| format!("failed to read {}", logs_path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", logs_path.display()))?;

    let Some(entries) = value.as_array() else {
        return Ok(None);
    };

    for entry in entries.iter().rev() {
        if let Some(session_id) = entry.get("sessionId").and_then(Value::as_str)
            && let Some(trimmed) = nonempty_trimmed(session_id)
        {
            return Ok(Some(trimmed));
        }
    }

    Ok(None)
}

fn parse_gemini_chat_sessions(project_dir: &Path) -> Result<Option<String>> {
    let chats_dir = project_dir.join("chats");
    if !chats_dir.is_dir() {
        return Ok(None);
    }

    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&chats_dir)
        .with_context(|| format!("failed to read {}", chats_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !file_name.starts_with("session-") || !file_name.ends_with(".json") {
            continue;
        }

        let modified = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        entries.push((modified, path));
    }

    entries.sort_by(|a, b| b.0.cmp(&a.0));

    let mut found: Vec<String> = Vec::new();
    for (_, path) in entries {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(session_id) = value.get("sessionId").and_then(Value::as_str)
            && let Some(trimmed) = nonempty_trimmed(session_id)
            && !found.contains(&trimmed)
        {
            found.push(trimmed);
        }
    }

    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => {
            let listed = found
                .iter()
                .map(|sid| sid.chars().take(8).collect::<String>())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "Ambiguous: multiple Gemini sessions found in {}: [{}]",
                project_dir.display(),
                listed
            )
        }
    }
}

/// Testable variant of resolve_caller_session_id_optional that accepts an injected daemon query function.
#[cfg(test)]
fn resolve_caller_session_id_optional_with_query<F>(
    team: Option<&str>,
    identity: Option<&str>,
    mut query_daemon_session: F,
) -> Result<Option<String>>
where
    F: FnMut(&str, &str) -> Result<Option<SessionQueryResult>>,
{
    if let Some(v) = env_var_nonempty("ATM_SESSION_ID") {
        return Ok(Some(v));
    }

    let runtime = runtime_hint(None);
    if let Some(sid) = resolve_runtime_native_env(runtime) {
        return Ok(Some(sid));
    }

    let runtime_specific = match runtime {
        CallerRuntime::Claude => resolve_claude_session(team, identity)?,
        CallerRuntime::Codex => resolve_codex_session(team, identity)?,
        CallerRuntime::Gemini => resolve_gemini_session(team, identity)?,
        CallerRuntime::Opencode => None,
        CallerRuntime::Unknown => resolve_unknown_runtime_session(team, identity)?,
    };
    if runtime_specific.is_some() {
        return Ok(runtime_specific);
    }

    if let (Some(t), Some(id)) = (team, identity) {
        match query_daemon_session(t, id) {
            Ok(Some(info)) if info.alive => {
                if let Some(sid) = info
                    .runtime_session_id
                    .as_deref()
                    .and_then(normalize_session_id)
                    .or_else(|| normalize_session_id(&info.session_id))
                {
                    return Ok(Some(sid));
                }
            }
            Ok(_) => {}
            Err(e) => classify_ambiguity_error(team, identity, e)?,
        }
    }

    Ok(None)
}

/// Resolve caller session id, returning `None` only when no usable session id was found.
///
/// Precedence:
/// 1) `ATM_SESSION_ID`
/// 2) runtime-native env for non-Claude runtimes (`CODEX_THREAD_ID`)
/// 3) runtime-specific resolution path (`hook > CLAUDE_SESSION_ID > session file` for Claude)
/// 4) daemon session registry (team+identity scoped, alive only)
pub fn resolve_caller_session_id_optional(
    team: Option<&str>,
    identity: Option<&str>,
) -> Result<Option<String>> {
    if let Some(v) = explicit_session_override() {
        return Ok(Some(v));
    }

    let runtime = runtime_hint(None);
    if let Some(sid) = resolve_runtime_native_env(runtime) {
        return Ok(Some(sid));
    }

    let runtime_specific = match runtime {
        CallerRuntime::Claude => resolve_claude_session(team, identity)?,
        CallerRuntime::Codex => resolve_codex_session(team, identity)?,
        CallerRuntime::Gemini => resolve_gemini_session(team, identity)?,
        CallerRuntime::Opencode => None,
        CallerRuntime::Unknown => resolve_unknown_runtime_session(team, identity)?,
    };
    if runtime_specific.is_some() {
        return Ok(runtime_specific);
    }

    let daemon_session = query_daemon_session(team, identity);
    if let Some(sid) = resolve_from_daemon(daemon_session.as_ref(), runtime) {
        return Ok(Some(sid));
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
         Run from a managed runtime session with hooks enabled or set ATM_SESSION_ID=<full-session-id>.",
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
            "pid": std::process::id(),
            "created_at": now,
            "updated_at": now,
        });
        std::fs::write(
            sessions_dir.join(format!("{session_id}.json")),
            serde_json::to_string(&data).unwrap(),
        )
        .unwrap();
    }

    fn write_gemini_logs(project_dir: &std::path::Path, session_ids: &[&str]) {
        std::fs::create_dir_all(project_dir).unwrap();
        let entries = session_ids
            .iter()
            .enumerate()
            .map(|(idx, session_id)| {
                serde_json::json!({
                    "sessionId": session_id,
                    "messageId": idx,
                    "type": "user",
                    "message": "test",
                    "timestamp": "2026-03-09T00:00:00.000Z"
                })
            })
            .collect::<Vec<_>>();
        std::fs::write(
            project_dir.join("logs.json"),
            serde_json::to_string(&entries).unwrap(),
        )
        .unwrap();
    }

    fn write_gemini_chat(project_dir: &std::path::Path, file_name: &str, session_id: &str) {
        let chats_dir = project_dir.join("chats");
        std::fs::create_dir_all(&chats_dir).unwrap();
        let path = chats_dir.join(file_name);
        let data = serde_json::json!({
            "sessionId": session_id,
            "messages": []
        });
        std::fs::write(&path, serde_json::to_string(&data).unwrap()).unwrap();
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
            std::env::set_var("ATM_RUNTIME", "opencode");
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
                    last_seen_at: None,
                    runtime: Some("codex".to_string()),
                    runtime_session_id: Some("thread-id:codex-thread-1".to_string()),
                    pane_id: None,
                    runtime_home: None,
                }))
            },
        )
        .unwrap();

        unsafe {
            std::env::remove_var("ATM_RUNTIME");
        }

        assert_eq!(resolved.as_deref(), Some("codex-thread-1"));
    }

    #[test]
    #[serial]
    fn daemon_ambiguity_surfaces_stable_code() {
        let temp = TempDir::new().unwrap();
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);
        unsafe {
            std::env::set_var("ATM_HOME", temp.path());
            std::env::set_var("ATM_TEST_HOME", temp.path());
            std::env::remove_var("ATM_RUNTIME");
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

        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("ATM_TEST_HOME");
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

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
            std::env::set_var("ATM_TEST_HOME", temp.path());
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("ATM_GEMINI_TMP_DIR");
        }

        let err = resolve_caller_session_id_required(Some("atm-dev"), Some("team-lead"))
            .expect_err("expected unresolved caller session");

        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("ATM_TEST_HOME");
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("ATM_GEMINI_TMP_DIR");
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
            std::env::set_var("ATM_TEST_HOME", temp.path());
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        let err = resolve_caller_session_id_required(Some("atm-dev"), Some("team-lead"))
            .expect_err("expected ambiguity");

        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("ATM_TEST_HOME");
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        assert!(err.to_string().contains(CALLER_AMBIGUOUS));
    }

    #[test]
    #[serial]
    fn codex_runtime_uses_codex_thread_id_only() {
        let temp = TempDir::new().unwrap();
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);

        // SAFETY: test-local environment mutation.
        unsafe {
            std::env::set_var("ATM_HOME", temp.path());
            std::env::set_var("ATM_TEST_HOME", temp.path());
            std::env::set_var("ATM_RUNTIME", "codex");
            std::env::set_var("CODEX_THREAD_ID", "codex-thread-123");
            std::env::set_var("CLAUDE_SESSION_ID", "claude-should-not-win");
        }

        let resolved = resolve_caller_session_id_optional(Some("atm-dev"), Some("arch-ctm"))
            .expect("resolve")
            .expect("session id");

        // SAFETY: test-local environment mutation cleanup.
        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("ATM_TEST_HOME");
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        assert_eq!(resolved, "codex-thread-123");
    }

    #[test]
    #[serial]
    fn codex_runtime_env_wins_over_live_daemon_session() {
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);

        unsafe {
            std::env::set_var("ATM_RUNTIME", "codex");
            std::env::set_var("CODEX_THREAD_ID", "codex-env-123");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        let mut query_called = false;
        let resolved = resolve_caller_session_id_optional_with_query(
            Some("atm-dev"),
            Some("arch-ctm"),
            |_team, _identity| {
                query_called = true;
                Ok(Some(SessionQueryResult {
                    session_id: "session_id:daemon-should-not-win".to_string(),
                    process_id: std::process::id(),
                    alive: true,
                    last_seen_at: None,
                    runtime: Some("codex".to_string()),
                    runtime_session_id: Some("thread-id:daemon-thread-999".to_string()),
                    pane_id: None,
                    runtime_home: None,
                }))
            },
        )
        .expect("resolve");

        unsafe {
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        assert_eq!(resolved.as_deref(), Some("codex-env-123"));
        assert!(
            !query_called,
            "daemon query should not run when CODEX_THREAD_ID is set"
        );
    }

    #[test]
    #[serial]
    fn codex_thread_id_implies_codex_runtime_without_atm_runtime() {
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);

        unsafe {
            std::env::remove_var("ATM_RUNTIME");
            std::env::set_var("CODEX_THREAD_ID", "codex-env-implicit-123");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        let mut query_called = false;
        let resolved = resolve_caller_session_id_optional_with_query(
            Some("atm-dev"),
            Some("arch-ctm"),
            |_team, _identity| {
                query_called = true;
                Ok(None)
            },
        )
        .expect("resolve");

        unsafe {
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        assert_eq!(resolved.as_deref(), Some("codex-env-implicit-123"));
        assert!(
            !query_called,
            "daemon query should not run when CODEX_THREAD_ID implies runtime"
        );
    }

    #[test]
    #[serial]
    fn codex_thread_id_precedence_holds_even_when_live_daemon_session_exists() {
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);

        unsafe {
            std::env::remove_var("ATM_RUNTIME");
            std::env::set_var("CODEX_THREAD_ID", "codex-env-live-123");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        let mut query_called = false;
        let resolved = resolve_caller_session_id_optional_with_query(
            Some("atm-dev"),
            Some("arch-ctm"),
            |_team, _identity| {
                query_called = true;
                Ok(Some(SessionQueryResult {
                    session_id: "session_id:daemon-live-session".to_string(),
                    process_id: std::process::id(),
                    alive: true,
                    last_seen_at: None,
                    runtime: Some("codex".to_string()),
                    runtime_session_id: Some("thread-id:daemon-live-thread".to_string()),
                    pane_id: None,
                    runtime_home: None,
                }))
            },
        )
        .expect("resolve");

        unsafe {
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("CODEX_THREAD_ID");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CLAUDE_SESSION_ID");
        }

        assert_eq!(resolved.as_deref(), Some("codex-env-live-123"));
        assert!(
            !query_called,
            "daemon query should not run when CODEX_THREAD_ID already resolves the caller session"
        );
    }

    #[test]
    #[serial]
    fn claude_runtime_env_wins_over_live_daemon_session() {
        let hook_path = current_ppid_hook_path();
        let _ = std::fs::remove_file(&hook_path);

        unsafe {
            std::env::set_var("ATM_RUNTIME", "claude");
            std::env::set_var("CLAUDE_SESSION_ID", "claude-env-abc");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        let mut query_called = false;
        let resolved = resolve_caller_session_id_optional_with_query(
            Some("atm-dev"),
            Some("team-lead"),
            |_team, _identity| {
                query_called = true;
                Ok(Some(SessionQueryResult {
                    session_id: "session_id:daemon-should-not-win".to_string(),
                    process_id: std::process::id(),
                    alive: true,
                    last_seen_at: None,
                    runtime: Some("claude".to_string()),
                    runtime_session_id: Some("session_id:daemon-claude-999".to_string()),
                    pane_id: None,
                    runtime_home: None,
                }))
            },
        )
        .expect("resolve");

        unsafe {
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("ATM_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        assert_eq!(resolved.as_deref(), Some("claude-env-abc"));
        assert!(
            !query_called,
            "daemon query should not run when CLAUDE_SESSION_ID is set"
        );
    }

    #[test]
    fn parse_gemini_list_sessions_extracts_uuid_first() {
        let output = r#"
Index Summary Last Active Session ID
1 Fix auth bug 2m ago d98410cc-6d6a-41ac-ac39-a2ce39b9e503
2 Older item 3h ago 93cbf813-b25a-4c3e-a3e0-1597417f7222
"#;
        let resolved = parse_gemini_list_sessions_output(output).expect("uuid expected");
        assert_eq!(resolved, "d98410cc-6d6a-41ac-ac39-a2ce39b9e503");
    }

    #[test]
    #[serial]
    fn gemini_file_fallback_prefers_logs_json() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("agent-team-mail");
        write_gemini_logs(
            &project_dir,
            &[
                "11111111-1111-4111-8111-111111111111",
                "22222222-2222-4222-8222-222222222222",
            ],
        );
        write_gemini_chat(
            &project_dir,
            "session-2026-03-09T00-00-aaaa1111.json",
            "33333333-3333-4333-8333-333333333333",
        );

        // SAFETY: test-local environment mutation.
        unsafe {
            std::env::set_var("ATM_RUNTIME", "gemini");
            std::env::set_var("ATM_GEMINI_TMP_DIR", temp.path());
            std::env::set_var("ATM_PROJECT_DIR", project_dir.to_string_lossy().to_string());
        }

        let resolved = resolve_caller_session_id_optional(Some("atm-dev"), Some("arch-gtm"))
            .expect("resolve")
            .expect("session id");

        // SAFETY: test-local environment mutation cleanup.
        unsafe {
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("ATM_GEMINI_TMP_DIR");
            std::env::remove_var("ATM_PROJECT_DIR");
        }

        assert_eq!(resolved, "22222222-2222-4222-8222-222222222222");
    }

    #[test]
    fn gemini_file_fallback_reports_ambiguity_when_multiple_chats_and_no_logs() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("agent-team-mail");
        write_gemini_chat(
            &project_dir,
            "session-2026-03-09T00-00-aaaa1111.json",
            "33333333-3333-4333-8333-333333333333",
        );
        write_gemini_chat(
            &project_dir,
            "session-2026-03-09T00-01-bbbb2222.json",
            "44444444-4444-4444-8444-444444444444",
        );

        let err = parse_gemini_chat_sessions(&project_dir).expect_err("ambiguous expected");
        assert!(err.to_string().contains("Ambiguous:"));
    }

    #[test]
    #[serial]
    fn opencode_runtime_is_explicitly_unresolved() {
        // SAFETY: test-local environment mutation.
        unsafe {
            std::env::set_var("ATM_RUNTIME", "opencode");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        let resolved = resolve_caller_session_id_optional(Some("atm-dev"), Some("opencode-bot"))
            .expect("resolve");

        // SAFETY: test-local environment mutation cleanup.
        unsafe {
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("CLAUDE_SESSION_ID");
            std::env::remove_var("CODEX_THREAD_ID");
        }

        assert!(resolved.is_none());
    }
}
