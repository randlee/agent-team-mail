//! ATM synthetic tool handlers for the MCP proxy.
//!
//! This module implements the ATM communication tools and session management
//! tools exposed by the proxy:
//!
//! - [`handle_atm_send`] — send a message to a specific agent's inbox
//! - [`handle_atm_read`] — read messages from the caller's inbox
//! - [`handle_atm_broadcast`] — send a message to all team members
//! - [`handle_atm_pending_count`] — count unread messages without marking them read
//! - [`handle_agent_sessions`] — list all sessions with their status (FR-10.1)
//! - [`handle_agent_status`] — summarise proxy status (FR-10.2)
//!
//! The ATM communication handlers operate synchronously using `std::fs`.
//! The session management handlers are `async` because they must acquire the
//! `Arc<Mutex<SessionRegistry>>` lock.
//!
//! Each function constructs a valid MCP result response (JSON-RPC 2.0) on
//! success or uses `isError: true` in the result content on failure.
//!
//! # Identity Resolution
//!
//! Callers may pass an explicit `"identity"` field in tool arguments.  If absent
//! the proxy config's `identity` is used.  When neither is available the tool
//! returns an error with code [`ERR_IDENTITY_REQUIRED`] (re-exported via
//! `proxy.rs`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use agent_team_mail_core::InboxMessage;
use agent_team_mail_core::home::get_home_dir;
use agent_team_mail_core::io::inbox_append;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::session::{SessionRegistry, SessionStatus};

/// Maximum allowed message length in characters (FR-8.4).
const MAX_MESSAGE_LEN: usize = 4096;

/// Truncation suffix appended when a message is cut to [`MAX_MESSAGE_LEN`].
const TRUNCATION_SUFFIX: &str = " [...truncated]";

/// Default maximum number of messages returned by [`handle_atm_read`] when
/// the caller does not provide a `limit` parameter.
const DEFAULT_READ_LIMIT: usize = 10;

// ---------------------------------------------------------------------------
// Identity resolution
// ---------------------------------------------------------------------------

/// Resolve the effective caller identity for an ATM tool call.
///
/// Precedence:
/// 1. `args["identity"]` — explicit per-call override
/// 2. `config_identity` — proxy-level default from `AgentMcpConfig.identity`
///
/// Returns `None` when neither source provides a value, which must cause
/// the caller to return [`ERR_IDENTITY_REQUIRED`].
pub fn resolve_identity(args: &Value, config_identity: Option<&str>) -> Option<String> {
    if let Some(id) = args.get("identity").and_then(|v| v.as_str()) {
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    config_identity.map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Truncate `text` to [`MAX_MESSAGE_LEN`] and append [`TRUNCATION_SUFFIX`] when truncated.
fn maybe_truncate(text: &str) -> String {
    if text.len() <= MAX_MESSAGE_LEN {
        text.to_string()
    } else {
        let mut truncated = text[..MAX_MESSAGE_LEN].to_string();
        truncated.push_str(TRUNCATION_SUFFIX);
        truncated
    }
}

/// Auto-generate a summary from the first 60 characters of a message.
fn auto_summary(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.len() <= 60 {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..60])
    }
}

/// Parse the `to` field into `(agent, team)`.
///
/// `"arch-ctm@atm-dev"` → `("arch-ctm", "atm-dev")`
/// `"arch-ctm"` → `("arch-ctm", default_team)`
fn parse_to(to: &str, default_team: &str) -> (String, String) {
    if let Some((agent, team)) = to.split_once('@') {
        (agent.to_string(), team.to_string())
    } else {
        (to.to_string(), default_team.to_string())
    }
}

/// Build the path to an agent's inbox file.
///
/// `<home>/.claude/teams/<team>/inboxes/<agent>.json`
fn inbox_path(home: &std::path::Path, team: &str, agent: &str) -> PathBuf {
    home.join(".claude")
        .join("teams")
        .join(team)
        .join("inboxes")
        .join(format!("{agent}.json"))
}

/// Build a UTC ISO 8601 timestamp for the current moment.
///
/// Uses a simple hand-formatted RFC 3339 string without pulling in `chrono` as
/// an additional dependency in this crate (the proxy already depends on uuid).
fn now_iso8601() -> String {
    // We use SystemTime → Duration since UNIX_EPOCH to avoid a chrono dependency.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Convert seconds to calendar components (UTC, no leap-second handling)
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400; // days since 1970-01-01
    let (y, mo, d) = days_to_ymd(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    (y, mo, d)
}

/// Build a new [`InboxMessage`] from parts.
fn build_message(from: &str, text: String, summary: Option<String>) -> InboxMessage {
    let message_id = Some(uuid::Uuid::new_v4().to_string());
    let auto_sum = auto_summary(&text);
    InboxMessage {
        from: from.to_string(),
        text,
        timestamp: now_iso8601(),
        read: false,
        summary: Some(summary.unwrap_or(auto_sum)),
        message_id,
        unknown_fields: HashMap::new(),
    }
}

/// Construct a successful MCP result response.
fn make_mcp_success(id: &Value, text: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{"type": "text", "text": text}]
        }
    })
}

/// Construct an MCP result response that signals an application-level error.
///
/// This uses `isError: true` inside the `result` (not a JSON-RPC `error` object)
/// so that callers can detect tool-level failures without treating them as
/// transport-level protocol errors.
pub fn make_mcp_error_result(id: &Value, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{"type": "text", "text": message}],
            "isError": true
        }
    })
}

// ---------------------------------------------------------------------------
// Public tool handlers
// ---------------------------------------------------------------------------

/// Handle an `atm_send` tool call.
///
/// Delivers a message to the target agent's inbox file.  The `to` parameter
/// supports `"agent"` or `"agent@team"` notation.  Messages exceeding
/// [`MAX_MESSAGE_LEN`] are truncated.
///
/// # Parameters (from `args`)
///
/// | Field     | Required | Description                            |
/// |-----------|----------|----------------------------------------|
/// | `to`      | yes      | Target agent, optionally `agent@team`  |
/// | `message` | yes      | Message body                           |
/// | `summary` | no       | Short summary (auto-generated if absent)|
///
/// # Returns
///
/// MCP result with `"Message sent to <agent>@<team>"` on success.
pub fn handle_atm_send(id: &Value, args: &Value, identity: &str, team: &str) -> Value {
    let to = match args.get("to").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return make_mcp_error_result(id, "atm_send: 'to' parameter is required"),
    };

    let raw_message = match args.get("message").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return make_mcp_error_result(id, "atm_send: 'message' parameter is required"),
    };

    let (agent, effective_team) = parse_to(to, team);
    let message_text = maybe_truncate(raw_message);
    let summary = args
        .get("summary")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let msg = build_message(identity, message_text, summary);

    let home = match get_home_dir() {
        Ok(h) => h,
        Err(e) => {
            return make_mcp_error_result(id, &format!("atm_send: cannot resolve home dir: {e}"))
        }
    };

    let path = inbox_path(&home, &effective_team, &agent);

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return make_mcp_error_result(
                id,
                &format!("atm_send: cannot create inbox directory: {e}"),
            );
        }
    }

    match inbox_append(&path, &msg, &effective_team, &agent) {
        Ok(_) => make_mcp_success(id, format!("Message sent to {agent}@{effective_team}")),
        Err(e) => make_mcp_error_result(id, &format!("atm_send: failed to write inbox: {e}")),
    }
}

/// Handle an `atm_read` tool call.
///
/// Reads messages from the caller's own inbox, with optional filtering.
///
/// # Parameters (from `args`)
///
/// | Field       | Required | Description                                     |
/// |-------------|----------|-------------------------------------------------|
/// | `all`       | no       | If `true`, include already-read messages         |
/// | `mark_read` | no       | If `false`, do not mark returned messages as read|
/// | `limit`     | no       | Max messages to return (default: 10)             |
/// | `since`     | no       | ISO 8601 timestamp; only messages after this     |
/// | `from`      | no       | Filter by sender identity                        |
///
/// # Returns
///
/// MCP result whose text is a JSON array of `{from, text, timestamp, message_id}` objects.
pub fn handle_atm_read(id: &Value, args: &Value, identity: &str, team: &str) -> Value {
    let home = match get_home_dir() {
        Ok(h) => h,
        Err(e) => {
            return make_mcp_error_result(id, &format!("atm_read: cannot resolve home dir: {e}"))
        }
    };

    let path = inbox_path(&home, team, identity);

    // If inbox doesn't exist, return empty array (not an error).
    if !path.exists() {
        return make_mcp_success(id, "[]".to_string());
    }

    // Read current messages
    let content = match std::fs::read(&path) {
        Ok(c) => c,
        Err(e) => {
            return make_mcp_error_result(id, &format!("atm_read: cannot read inbox: {e}"))
        }
    };
    let mut messages: Vec<InboxMessage> = match serde_json::from_slice(&content) {
        Ok(m) => m,
        Err(e) => {
            return make_mcp_error_result(id, &format!("atm_read: failed to parse inbox: {e}"))
        }
    };

    // Parse optional params
    let include_all = args
        .get("all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mark_read = args
        .get("mark_read")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_READ_LIMIT);
    let since = args
        .get("since")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let from_filter = args
        .get("from")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Apply filters
    let filtered: Vec<&InboxMessage> = messages
        .iter()
        .filter(|m| {
            // Unread filter
            if !include_all && m.read {
                return false;
            }
            // Since filter
            if let Some(ref since_ts) = since {
                if m.timestamp.as_str() < since_ts.as_str() {
                    return false;
                }
            }
            // From filter
            if let Some(ref sender) = from_filter {
                if &m.from != sender {
                    return false;
                }
            }
            true
        })
        .take(limit)
        .collect();

    // Collect message IDs to mark as read
    let ids_to_mark: Vec<String> = if mark_read {
        filtered
            .iter()
            .filter_map(|m| m.message_id.clone())
            .collect()
    } else {
        Vec::new()
    };

    // Build output before potentially mutating messages
    let output: Vec<Value> = filtered
        .iter()
        .map(|m| {
            json!({
                "from": m.from,
                "text": m.text,
                "timestamp": m.timestamp,
                "message_id": m.message_id,
            })
        })
        .collect();

    // Mark messages as read if requested
    if mark_read && !ids_to_mark.is_empty() {
        let ids_set: std::collections::HashSet<String> = ids_to_mark.into_iter().collect();
        // Also mark messages without a message_id that match the filtered set.
        // Collect timestamps+from for id-less messages.
        let id_less_keys: Vec<(String, String)> = filtered
            .iter()
            .filter(|m| m.message_id.is_none())
            .map(|m| (m.from.clone(), m.timestamp.clone()))
            .collect();

        for msg in messages.iter_mut() {
            let should_mark = if let Some(ref mid) = msg.message_id {
                ids_set.contains(mid)
            } else {
                id_less_keys
                    .iter()
                    .any(|(f, t)| f == &msg.from && t == &msg.timestamp)
            };
            if should_mark {
                msg.read = true;
            }
        }

        // Write back with marked messages
        let updated_content = match serde_json::to_vec_pretty(&messages) {
            Ok(c) => c,
            Err(e) => {
                // Return the output even if we failed to persist mark-read
                tracing::warn!("atm_read: failed to serialize updated inbox: {e}");
                let text = serde_json::to_string_pretty(&output).unwrap_or_default();
                return make_mcp_success(id, text);
            }
        };

        if let Err(e) = std::fs::write(&path, &updated_content) {
            tracing::warn!("atm_read: failed to persist mark-read: {e}");
        }
    }

    let text = serde_json::to_string_pretty(&output).unwrap_or_else(|_| "[]".to_string());
    make_mcp_success(id, text)
}

/// Handle an `atm_broadcast` tool call.
///
/// Sends a message to every member of the team except the caller.
///
/// # Parameters (from `args`)
///
/// | Field     | Required | Description                                     |
/// |-----------|----------|-------------------------------------------------|
/// | `message` | yes      | Message body                                    |
/// | `summary` | no       | Short summary (auto-generated if absent)         |
/// | `team`    | no       | Override team (defaults to proxy config team)    |
///
/// # Returns
///
/// MCP result with `"Broadcast sent to N members of <team>"` on success.
pub fn handle_atm_broadcast(id: &Value, args: &Value, identity: &str, team: &str) -> Value {
    let raw_message = match args.get("message").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return make_mcp_error_result(id, "atm_broadcast: 'message' parameter is required")
        }
    };

    let effective_team = args
        .get("team")
        .and_then(|v| v.as_str())
        .unwrap_or(team)
        .to_string();

    let message_text = maybe_truncate(raw_message);
    let summary = args
        .get("summary")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let home = match get_home_dir() {
        Ok(h) => h,
        Err(e) => {
            return make_mcp_error_result(
                id,
                &format!("atm_broadcast: cannot resolve home dir: {e}"),
            )
        }
    };

    // Read team config to find members
    let config_path = home
        .join(".claude")
        .join("teams")
        .join(&effective_team)
        .join("config.json");

    let config_content = match std::fs::read(&config_path) {
        Ok(c) => c,
        Err(e) => {
            return make_mcp_error_result(
                id,
                &format!(
                    "atm_broadcast: cannot read team config at '{}': {e}. \
                     Ensure the team '{effective_team}' exists.",
                    config_path.display()
                ),
            )
        }
    };

    let team_config: agent_team_mail_core::TeamConfig =
        match serde_json::from_slice(&config_content) {
            Ok(c) => c,
            Err(e) => {
                return make_mcp_error_result(
                    id,
                    &format!("atm_broadcast: failed to parse team config: {e}"),
                )
            }
        };

    // Send to all members except caller
    let recipients: Vec<String> = team_config
        .members
        .iter()
        .map(|m| m.name.clone())
        .filter(|name| name != identity)
        .collect();

    let mut sent_count = 0usize;
    for recipient in &recipients {
        let msg = build_message(identity, message_text.clone(), summary.clone());
        let path = inbox_path(&home, &effective_team, recipient);

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match inbox_append(&path, &msg, &effective_team, recipient) {
            Ok(_) => sent_count += 1,
            Err(e) => {
                tracing::warn!("atm_broadcast: failed to deliver to '{recipient}': {e}");
            }
        }
    }

    make_mcp_success(
        id,
        format!("Broadcast sent to {sent_count} members of {effective_team}"),
    )
}

/// Handle an `atm_pending_count` tool call.
///
/// Returns the number of unread messages in the caller's inbox without
/// marking any messages as read.
///
/// # Returns
///
/// MCP result whose text is `{"unread": N}`.
pub fn handle_atm_pending_count(id: &Value, _args: &Value, identity: &str, team: &str) -> Value {
    let home = match get_home_dir() {
        Ok(h) => h,
        Err(e) => {
            return make_mcp_error_result(
                id,
                &format!("atm_pending_count: cannot resolve home dir: {e}"),
            )
        }
    };

    let path = inbox_path(&home, team, identity);

    if !path.exists() {
        return make_mcp_success(id, r#"{"unread":0}"#.to_string());
    }

    let content = match std::fs::read(&path) {
        Ok(c) => c,
        Err(e) => {
            return make_mcp_error_result(
                id,
                &format!("atm_pending_count: cannot read inbox: {e}"),
            )
        }
    };

    let messages: Vec<InboxMessage> = match serde_json::from_slice(&content) {
        Ok(m) => m,
        Err(e) => {
            return make_mcp_error_result(
                id,
                &format!("atm_pending_count: failed to parse inbox: {e}"),
            )
        }
    };

    let unread = messages.iter().filter(|m| !m.read).count();
    make_mcp_success(id, format!(r#"{{"unread":{unread}}}"#))
}

// ---------------------------------------------------------------------------
// Session management tool handlers (FR-10.1, FR-10.2)
// ---------------------------------------------------------------------------

/// Handle an `agent_sessions` tool call (FR-10.1).
///
/// Returns a JSON array of all sessions currently tracked by the registry,
/// regardless of status. Each element includes `agent_id`, `backend`,
/// `backend_id` (Codex threadId), `team`, `identity`, `agent_name`,
/// `agent_source`, `tag`, `status`, `last_active`, and `resumable`.
///
/// A session is `resumable` when it is [`SessionStatus::Stale`] **and** has a
/// non-`None` `thread_id`, meaning the prior Codex thread may still be alive.
///
/// The `agent_name` field is derived from the file stem of `agent_source` when
/// present, falling back to `identity`.
///
/// # Returns
///
/// MCP result whose text is a pretty-printed JSON array of session objects.
pub async fn handle_agent_sessions(
    id: &Value,
    registry: Arc<Mutex<SessionRegistry>>,
) -> Value {
    let guard = registry.lock().await;
    let sessions: Vec<Value> = guard.list_all().iter().map(|e| {
        let status_str = match e.status {
            SessionStatus::Active => "active",
            SessionStatus::Stale => "stale",
            SessionStatus::Closed => "closed",
        };
        let resumable = e.status == SessionStatus::Stale && e.thread_id.is_some();
        let agent_name = e.agent_source.as_deref()
            .and_then(|p| std::path::Path::new(p).file_stem())
            .and_then(|s| s.to_str())
            .unwrap_or(&e.identity)
            .to_string();
        json!({
            "agent_id": e.agent_id,
            "backend": "codex",
            "backend_id": e.thread_id,
            "team": e.team,
            "identity": e.identity,
            "agent_name": agent_name,
            "agent_source": e.agent_source,
            "tag": e.tag,
            "status": status_str,
            "last_active": e.last_active,
            "resumable": resumable,
        })
    }).collect();

    let text = serde_json::to_string_pretty(&sessions).unwrap_or_else(|_| "[]".to_string());
    make_mcp_success(id, text)
}

/// Count the number of unread messages in an agent's inbox.
///
/// Returns `0` when the inbox file does not exist or cannot be parsed.
/// This is used by [`handle_agent_status`] (and its callers) to compute the
/// aggregate pending mail count across all active sessions.
pub fn count_unread_for_identity(identity: &str, team: &str, home: &std::path::Path) -> u64 {
    let path = inbox_path(home, team, identity);
    if !path.exists() {
        return 0;
    }
    let content = match std::fs::read(&path) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    let messages: Vec<agent_team_mail_core::InboxMessage> =
        match serde_json::from_slice(&content) {
            Ok(m) => m,
            Err(_) => return 0,
        };
    messages.iter().filter(|m| !m.read).count() as u64
}

/// Handle an `agent_status` tool call (FR-10.2).
///
/// Returns a JSON object summarising the proxy's runtime status: whether a
/// Codex child process is alive, the ATM team name, startup timestamp, uptime
/// in seconds, active thread count, aggregate unread mail count across all
/// active sessions, and the current identity→threadId map for active sessions.
///
/// # Parameters
///
/// * `pending_mail_count` — pre-computed total unread message count across all
///   active sessions; callers should compute this before acquiring the registry
///   lock to keep this function pure relative to the registry state.
///
/// # Returns
///
/// MCP result whose text is a pretty-printed JSON status object.
pub async fn handle_agent_status(
    id: &Value,
    registry: Arc<Mutex<SessionRegistry>>,
    child_alive: bool,
    team: &str,
    started_at: &str,
    uptime_secs: u64,
    pending_mail_count: u64,
) -> Value {
    let guard = registry.lock().await;
    let active_count = guard.active_count();
    let identity_map: serde_json::Map<String, Value> = guard
        .list_all()
        .iter()
        .filter(|e| e.status == SessionStatus::Active)
        .map(|e| {
            (
                e.identity.clone(),
                Value::String(e.thread_id.clone().unwrap_or_default()),
            )
        })
        .collect();

    let status = json!({
        "child_alive": child_alive,
        "team": team,
        "started_at": started_at,
        "uptime_secs": uptime_secs,
        "active_thread_count": active_count,
        "pending_mail_count": pending_mail_count,
        "identity_map": identity_map,
    });

    let text = serde_json::to_string_pretty(&status).unwrap_or_default();
    make_mcp_success(id, text)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use serial_test::serial;
    use std::fs;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // Helper utilities
    // -----------------------------------------------------------------------

    /// Set ATM_HOME to `dir` and return a cleanup guard.
    fn set_atm_home(dir: &TempDir) -> String {
        let p = dir.path().to_string_lossy().to_string();
        // SAFETY: single-threaded within a test function; serial attribute prevents races.
        unsafe { std::env::set_var("ATM_HOME", &p) };
        p
    }

    fn unset_atm_home() {
        unsafe { std::env::remove_var("ATM_HOME") };
    }

    /// Write a minimal team config with the given member names.
    fn write_team_config(home: &std::path::Path, team: &str, member_names: &[&str]) {
        let team_dir = home
            .join(".claude")
            .join("teams")
            .join(team);
        fs::create_dir_all(&team_dir).unwrap();

        let members: Vec<serde_json::Value> = member_names
            .iter()
            .map(|name| {
                json!({
                    "agentId": format!("{name}@{team}"),
                    "name": name,
                    "agentType": "general-purpose",
                    "model": "claude-sonnet-4-6",
                    "joinedAt": 1000000u64,
                    "cwd": "/tmp"
                })
            })
            .collect();

        let config = json!({
            "name": team,
            "createdAt": 1000000u64,
            "leadAgentId": format!("{}@{}", member_names[0], team),
            "leadSessionId": "test-session-id",
            "members": members
        });

        fs::write(
            team_dir.join("config.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();
    }

    /// Seed an inbox file with the provided messages.
    fn seed_inbox(home: &std::path::Path, team: &str, agent: &str, messages: &[InboxMessage]) {
        let dir = home
            .join(".claude")
            .join("teams")
            .join(team)
            .join("inboxes");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{agent}.json"));
        fs::write(
            &path,
            serde_json::to_string_pretty(messages).unwrap(),
        )
        .unwrap();
    }

    /// Build a minimal test InboxMessage.
    fn make_msg(from: &str, text: &str, read: bool, msg_id: Option<&str>) -> InboxMessage {
        InboxMessage {
            from: from.to_string(),
            text: text.to_string(),
            timestamp: "2026-02-18T10:00:00Z".to_string(),
            read,
            summary: None,
            message_id: msg_id.map(|s| s.to_string()),
            unknown_fields: HashMap::new(),
        }
    }

    /// Read and parse an inbox file for assertions.
    fn read_inbox(home: &std::path::Path, team: &str, agent: &str) -> Vec<InboxMessage> {
        let path = home
            .join(".claude")
            .join("teams")
            .join(team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        let content = fs::read_to_string(&path).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    // -----------------------------------------------------------------------
    // resolve_identity tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_identity_explicit_param() {
        let args = json!({"identity": "explicit-id"});
        let result = resolve_identity(&args, Some("config-id"));
        assert_eq!(result, Some("explicit-id".to_string()));
    }

    #[test]
    fn test_resolve_identity_config_fallback() {
        let args = json!({});
        let result = resolve_identity(&args, Some("config-id"));
        assert_eq!(result, Some("config-id".to_string()));
    }

    #[test]
    fn test_resolve_identity_returns_none_when_both_absent() {
        let args = json!({});
        let result = resolve_identity(&args, None);
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_identity_empty_string_falls_back_to_config() {
        let args = json!({"identity": ""});
        let result = resolve_identity(&args, Some("config-id"));
        assert_eq!(result, Some("config-id".to_string()));
    }

    // -----------------------------------------------------------------------
    // parse_to tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_atm_send_to_parsing_simple() {
        let (agent, team) = parse_to("arch-ctm", "default-team");
        assert_eq!(agent, "arch-ctm");
        assert_eq!(team, "default-team");
    }

    #[test]
    fn test_atm_send_to_parsing_at_notation() {
        let (agent, team) = parse_to("arch-ctm@atm-dev", "default-team");
        assert_eq!(agent, "arch-ctm");
        assert_eq!(team, "atm-dev");
    }

    // -----------------------------------------------------------------------
    // Truncation tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_message_truncation_at_limit() {
        let long_msg = "a".repeat(MAX_MESSAGE_LEN + 100);
        let result = maybe_truncate(&long_msg);
        assert!(result.len() <= MAX_MESSAGE_LEN + TRUNCATION_SUFFIX.len());
        assert!(result.ends_with(TRUNCATION_SUFFIX));
        assert_eq!(&result[..MAX_MESSAGE_LEN], &long_msg[..MAX_MESSAGE_LEN]);
    }

    #[test]
    fn test_message_no_truncation_under_limit() {
        let short_msg = "hello world";
        let result = maybe_truncate(short_msg);
        assert_eq!(result, short_msg);
    }

    #[test]
    fn test_message_exact_limit_not_truncated() {
        let exact_msg = "a".repeat(MAX_MESSAGE_LEN);
        let result = maybe_truncate(&exact_msg);
        assert_eq!(result, exact_msg);
        assert!(!result.contains("truncated"));
    }

    // -----------------------------------------------------------------------
    // Auto-summary tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_auto_summary_generation() {
        let msg = "This is a message longer than sixty characters for testing summary generation";
        let summary = auto_summary(msg);
        assert!(summary.ends_with("..."));
        assert_eq!(summary.len(), 63); // 60 chars + "..."
    }

    #[test]
    fn test_auto_summary_short_message() {
        let msg = "Short msg";
        let summary = auto_summary(msg);
        assert_eq!(summary, "Short msg");
        assert!(!summary.ends_with("..."));
    }

    // -----------------------------------------------------------------------
    // atm_send tests
    // -----------------------------------------------------------------------

    #[test]
    #[serial]
    fn test_atm_send_success() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        let id = json!(1);
        let args = json!({"to": "arch-ctm", "message": "Hello from test"});
        let resp = handle_atm_send(&id, &args, "team-lead", "atm-dev");

        unset_atm_home();

        assert!(resp.get("error").is_none(), "should not be an error response");
        assert_eq!(resp["result"]["isError"], Value::Null);

        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("arch-ctm"), "should mention recipient");

        // Verify inbox file was created
        let msgs = read_inbox(dir.path(), "atm-dev", "arch-ctm");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].from, "team-lead");
        assert_eq!(msgs[0].text, "Hello from test");
        assert!(!msgs[0].read);
        assert!(msgs[0].message_id.is_some());
    }

    #[test]
    #[serial]
    fn test_atm_send_at_notation_routes_to_correct_team() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        let id = json!(2);
        let args = json!({"to": "dev-agent@sprint-team", "message": "Cross-team message"});
        let resp = handle_atm_send(&id, &args, "team-lead", "atm-dev");

        unset_atm_home();

        assert!(resp.get("error").is_none());
        let msgs = read_inbox(dir.path(), "sprint-team", "dev-agent");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "Cross-team message");
    }

    #[test]
    #[serial]
    fn test_atm_send_truncates_long_message() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        let long_msg = "x".repeat(MAX_MESSAGE_LEN + 50);
        let id = json!(3);
        let args = json!({"to": "agent-a", "message": long_msg});
        handle_atm_send(&id, &args, "sender", "team");

        unset_atm_home();

        let msgs = read_inbox(dir.path(), "team", "agent-a");
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].text.ends_with(TRUNCATION_SUFFIX));
    }

    #[test]
    fn test_atm_send_missing_to_returns_error() {
        let id = json!(4);
        let args = json!({"message": "hello"});
        let resp = handle_atm_send(&id, &args, "sender", "team");
        assert_eq!(resp["result"]["isError"], json!(true));
    }

    #[test]
    fn test_atm_send_missing_message_returns_error() {
        let id = json!(5);
        let args = json!({"to": "agent"});
        let resp = handle_atm_send(&id, &args, "sender", "team");
        assert_eq!(resp["result"]["isError"], json!(true));
    }

    // -----------------------------------------------------------------------
    // atm_read tests
    // -----------------------------------------------------------------------

    #[test]
    #[serial]
    fn test_atm_read_empty_inbox() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        let id = json!(10);
        let args = json!({});
        let resp = handle_atm_read(&id, &args, "nobody", "team");

        unset_atm_home();

        // Missing inbox file is not an error
        assert!(resp.get("error").is_none());
        assert_ne!(resp["result"]["isError"], json!(true));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let msgs: Vec<Value> = serde_json::from_str(text).unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    #[serial]
    fn test_atm_read_filters_unread_by_default() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        seed_inbox(
            dir.path(),
            "team",
            "agent",
            &[
                make_msg("a", "unread1", false, Some("id-1")),
                make_msg("b", "already-read", true, Some("id-2")),
                make_msg("c", "unread2", false, Some("id-3")),
            ],
        );

        let id = json!(11);
        let args = json!({"mark_read": false});
        let resp = handle_atm_read(&id, &args, "agent", "team");

        unset_atm_home();

        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let msgs: Vec<Value> = serde_json::from_str(text).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs.iter().any(|m| m["text"] == "unread1"));
        assert!(msgs.iter().any(|m| m["text"] == "unread2"));
        assert!(!msgs.iter().any(|m| m["text"] == "already-read"));
    }

    #[test]
    #[serial]
    fn test_atm_read_all_flag() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        seed_inbox(
            dir.path(),
            "team",
            "agent",
            &[
                make_msg("a", "unread", false, Some("id-1")),
                make_msg("b", "read", true, Some("id-2")),
            ],
        );

        let id = json!(12);
        let args = json!({"all": true, "mark_read": false});
        let resp = handle_atm_read(&id, &args, "agent", "team");

        unset_atm_home();

        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let msgs: Vec<Value> = serde_json::from_str(text).unwrap();
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    #[serial]
    fn test_atm_read_marks_read_by_default() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        seed_inbox(
            dir.path(),
            "team",
            "agent",
            &[
                make_msg("a", "msg1", false, Some("id-1")),
                make_msg("b", "msg2", false, Some("id-2")),
            ],
        );

        let id = json!(13);
        let args = json!({});
        handle_atm_read(&id, &args, "agent", "team");

        let msgs = read_inbox(dir.path(), "team", "agent");
        unset_atm_home();

        assert!(msgs.iter().all(|m| m.read), "all messages should be marked read");
    }

    #[test]
    #[serial]
    fn test_atm_read_limit() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        let messages: Vec<InboxMessage> = (0..15)
            .map(|i| make_msg("sender", &format!("msg{i}"), false, Some(&format!("id-{i}"))))
            .collect();
        seed_inbox(dir.path(), "team", "agent", &messages);

        let id = json!(14);
        let args = json!({"limit": 5, "mark_read": false});
        let resp = handle_atm_read(&id, &args, "agent", "team");

        unset_atm_home();

        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let msgs: Vec<Value> = serde_json::from_str(text).unwrap();
        assert_eq!(msgs.len(), 5);
    }

    #[test]
    #[serial]
    fn test_atm_read_default_limit_is_ten() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        let messages: Vec<InboxMessage> = (0..20)
            .map(|i| make_msg("sender", &format!("msg{i}"), false, Some(&format!("id-{i}"))))
            .collect();
        seed_inbox(dir.path(), "team", "agent", &messages);

        let id = json!(14);
        let args = json!({"mark_read": false});
        let resp = handle_atm_read(&id, &args, "agent", "team");

        unset_atm_home();

        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let msgs: Vec<Value> = serde_json::from_str(text).unwrap();
        assert_eq!(msgs.len(), 10);
    }

    #[test]
    #[serial]
    fn test_atm_read_from_filter() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        seed_inbox(
            dir.path(),
            "team",
            "agent",
            &[
                make_msg("alice", "from alice", false, Some("id-1")),
                make_msg("bob", "from bob", false, Some("id-2")),
                make_msg("alice", "also alice", false, Some("id-3")),
            ],
        );

        let id = json!(15);
        let args = json!({"from": "alice", "mark_read": false});
        let resp = handle_atm_read(&id, &args, "agent", "team");

        unset_atm_home();

        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let msgs: Vec<Value> = serde_json::from_str(text).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs.iter().all(|m| m["from"] == "alice"));
    }

    #[test]
    #[serial]
    fn test_atm_read_since_filter() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        // Seed 3 messages with distinct timestamps
        let old_msg = InboxMessage {
            from: "sender".to_string(),
            text: "old message".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            read: false,
            summary: None,
            message_id: Some("id-old".to_string()),
            unknown_fields: HashMap::new(),
        };
        let middle_msg = InboxMessage {
            from: "sender".to_string(),
            text: "middle message".to_string(),
            timestamp: "2026-02-01T00:00:00Z".to_string(),
            read: false,
            summary: None,
            message_id: Some("id-middle".to_string()),
            unknown_fields: HashMap::new(),
        };
        let future_msg = InboxMessage {
            from: "sender".to_string(),
            text: "future message".to_string(),
            timestamp: "2026-03-01T00:00:00Z".to_string(),
            read: false,
            summary: None,
            message_id: Some("id-future".to_string()),
            unknown_fields: HashMap::new(),
        };
        seed_inbox(dir.path(), "team", "agent", &[old_msg, middle_msg, future_msg]);

        let id = json!(16);
        // since = "2026-02-01T00:00:00Z" — should include middle and future, exclude old
        let args = json!({"since": "2026-02-01T00:00:00Z", "mark_read": false, "limit": 10});
        let resp = handle_atm_read(&id, &args, "agent", "team");

        unset_atm_home();

        assert!(resp.get("error").is_none(), "should not be protocol error; got: {resp}");
        assert_ne!(resp["result"]["isError"], json!(true), "should not be isError; got: {resp}");

        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let msgs: Vec<Value> = serde_json::from_str(text).unwrap();
        assert_eq!(msgs.len(), 2, "should return only messages at or after the since timestamp");
        assert!(
            msgs.iter().any(|m| m["text"] == "middle message"),
            "middle message should be included"
        );
        assert!(
            msgs.iter().any(|m| m["text"] == "future message"),
            "future message should be included"
        );
        assert!(
            !msgs.iter().any(|m| m["text"] == "old message"),
            "old message should be excluded"
        );
    }

    // -----------------------------------------------------------------------
    // atm_pending_count tests
    // -----------------------------------------------------------------------

    #[test]
    #[serial]
    fn test_atm_pending_count_zero() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        let id = json!(20);
        let args = json!({});
        let resp = handle_atm_pending_count(&id, &args, "nobody", "team");

        unset_atm_home();

        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let v: Value = serde_json::from_str(text).unwrap();
        assert_eq!(v["unread"], json!(0));
    }

    #[test]
    #[serial]
    fn test_atm_pending_count_nonzero() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        seed_inbox(
            dir.path(),
            "team",
            "agent",
            &[
                make_msg("a", "msg1", false, Some("id-1")),
                make_msg("b", "msg2", true, Some("id-2")),
                make_msg("c", "msg3", false, Some("id-3")),
            ],
        );

        let id = json!(21);
        let args = json!({});
        let resp = handle_atm_pending_count(&id, &args, "agent", "team");

        unset_atm_home();

        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let v: Value = serde_json::from_str(text).unwrap();
        assert_eq!(v["unread"], json!(2));
    }

    #[test]
    #[serial]
    fn test_atm_pending_count_does_not_mark_read() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        seed_inbox(
            dir.path(),
            "team",
            "agent",
            &[
                make_msg("a", "msg1", false, Some("id-1")),
                make_msg("b", "msg2", false, Some("id-2")),
            ],
        );

        let id = json!(22);
        let args = json!({});
        handle_atm_pending_count(&id, &args, "agent", "team");

        let msgs = read_inbox(dir.path(), "team", "agent");
        unset_atm_home();

        assert!(
            msgs.iter().all(|m| !m.read),
            "pending_count must not mark messages as read"
        );
    }

    // -----------------------------------------------------------------------
    // atm_broadcast tests
    // -----------------------------------------------------------------------

    #[test]
    #[serial]
    fn test_atm_broadcast_reads_team_config() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        write_team_config(
            dir.path(),
            "atm-dev",
            &["team-lead", "arch-ctm", "dev-agent"],
        );

        let id = json!(30);
        let args = json!({"message": "broadcast test"});
        let resp = handle_atm_broadcast(&id, &args, "team-lead", "atm-dev");

        unset_atm_home();

        assert!(resp.get("error").is_none());
        assert_ne!(resp["result"]["isError"], json!(true));

        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("2 members"), "should send to 2 members (excluding self)");
    }

    #[test]
    #[serial]
    fn test_atm_broadcast_skips_caller() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        write_team_config(
            dir.path(),
            "team",
            &["sender", "recip-a", "recip-b"],
        );

        let id = json!(31);
        let args = json!({"message": "skips-me"});
        handle_atm_broadcast(&id, &args, "sender", "team");

        unset_atm_home();

        // sender's own inbox should NOT have the message
        let sender_inbox_path = dir
            .path()
            .join(".claude")
            .join("teams")
            .join("team")
            .join("inboxes")
            .join("sender.json");
        assert!(
            !sender_inbox_path.exists(),
            "sender should not receive their own broadcast"
        );

        // recip-a and recip-b should have the message
        let ra = read_inbox(dir.path(), "team", "recip-a");
        let rb = read_inbox(dir.path(), "team", "recip-b");
        assert_eq!(ra.len(), 1);
        assert_eq!(rb.len(), 1);
    }

    #[test]
    #[serial]
    fn test_atm_broadcast_missing_config_returns_error() {
        let dir = TempDir::new().unwrap();
        set_atm_home(&dir);

        let id = json!(32);
        let args = json!({"message": "hello"});
        let resp = handle_atm_broadcast(&id, &args, "team-lead", "nonexistent-team");

        unset_atm_home();

        assert_eq!(resp["result"]["isError"], json!(true));
    }

    // -----------------------------------------------------------------------
    // handle_agent_sessions tests
    // -----------------------------------------------------------------------

    fn make_test_registry(max: usize) -> Arc<Mutex<SessionRegistry>> {
        Arc::new(Mutex::new(SessionRegistry::new(max)))
    }

    #[tokio::test]
    async fn test_agent_sessions_empty_registry() {
        let reg = make_test_registry(10);
        let id = json!(100);
        let resp = handle_agent_sessions(&id, reg).await;
        assert!(resp.get("error").is_none());
        assert_ne!(resp["result"]["isError"], json!(true));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let sessions: Vec<Value> = serde_json::from_str(text).unwrap();
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn test_agent_sessions_active_session_listed() {
        let reg = make_test_registry(10);
        {
            let mut guard = reg.lock().await;
            guard
                .register(
                    "arch-ctm".to_string(),
                    "atm-dev".to_string(),
                    "/tmp".to_string(),
                    None,
                    None,
                    None,
                )
                .unwrap();
        }
        let id = json!(101);
        let resp = handle_agent_sessions(&id, reg).await;
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let sessions: Vec<Value> = serde_json::from_str(text).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["identity"], "arch-ctm");
        assert_eq!(sessions[0]["status"], "active");
        assert_eq!(sessions[0]["resumable"], json!(false));
    }

    #[tokio::test]
    async fn test_agent_sessions_stale_with_thread_id_is_resumable() {
        let reg = make_test_registry(10);
        let agent_id = {
            let mut guard = reg.lock().await;
            let e = guard
                .register(
                    "dev-agent".to_string(),
                    "team".to_string(),
                    "/tmp".to_string(),
                    None,
                    None,
                    None,
                )
                .unwrap();
            guard.set_thread_id(&e.agent_id, "thread-xyz".to_string());
            guard.mark_all_stale();
            e.agent_id.clone()
        };
        let id = json!(102);
        let resp = handle_agent_sessions(&id, Arc::clone(&reg)).await;
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let sessions: Vec<Value> = serde_json::from_str(text).unwrap();
        let session = sessions
            .iter()
            .find(|s| s["agent_id"] == agent_id)
            .unwrap();
        assert_eq!(session["status"], "stale");
        assert_eq!(session["resumable"], json!(true));
    }

    #[tokio::test]
    async fn test_agent_sessions_stale_without_thread_id_not_resumable() {
        let reg = make_test_registry(10);
        {
            let mut guard = reg.lock().await;
            guard
                .register(
                    "no-thread".to_string(),
                    "team".to_string(),
                    "/tmp".to_string(),
                    None,
                    None,
                    None,
                )
                .unwrap();
            guard.mark_all_stale();
        }
        let id = json!(103);
        let resp = handle_agent_sessions(&id, reg).await;
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let sessions: Vec<Value> = serde_json::from_str(text).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["resumable"], json!(false));
    }

    #[tokio::test]
    async fn test_agent_sessions_mixed_statuses() {
        let reg = make_test_registry(10);
        {
            let mut guard = reg.lock().await;
            let a = guard
                .register(
                    "active-agent".to_string(),
                    "team".to_string(),
                    "/tmp".to_string(),
                    None,
                    None,
                    None,
                )
                .unwrap();
            let c = guard
                .register(
                    "closed-agent".to_string(),
                    "team".to_string(),
                    "/tmp".to_string(),
                    None,
                    None,
                    None,
                )
                .unwrap();
            guard.close(&c.agent_id);
            guard
                .register(
                    "stale-agent".to_string(),
                    "team".to_string(),
                    "/tmp".to_string(),
                    None,
                    None,
                    None,
                )
                .unwrap();
            let _ = a;
        }
        // Mark non-active sessions stale (mark_all_stale makes ALL stale)
        // Instead close + keep one active, and use insert_stale for the third
        let reg2 = make_test_registry(10);
        {
            let mut guard = reg2.lock().await;
            guard
                .register(
                    "active-agent".to_string(),
                    "team".to_string(),
                    "/tmp".to_string(),
                    None,
                    None,
                    None,
                )
                .unwrap();
            let closed = guard
                .register(
                    "closed-agent".to_string(),
                    "team".to_string(),
                    "/tmp".to_string(),
                    None,
                    None,
                    None,
                )
                .unwrap();
            guard.close(&closed.agent_id);
        }
        let id = json!(104);
        let resp = handle_agent_sessions(&id, reg2).await;
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let sessions: Vec<Value> = serde_json::from_str(text).unwrap();
        assert_eq!(sessions.len(), 2);
        let statuses: Vec<&str> = sessions
            .iter()
            .map(|s| s["status"].as_str().unwrap())
            .collect();
        assert!(statuses.contains(&"active"));
        assert!(statuses.contains(&"closed"));
    }

    // -----------------------------------------------------------------------
    // handle_agent_status tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_agent_status_no_sessions() {
        let reg = make_test_registry(10);
        let id = json!(200);
        let resp = handle_agent_status(
            &id,
            reg,
            false,
            "atm-dev",
            "2026-02-18T00:00:00Z",
            42,
            0,
        )
        .await;
        assert!(resp.get("error").is_none());
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let status: Value = serde_json::from_str(text).unwrap();
        assert_eq!(status["child_alive"], json!(false));
        assert_eq!(status["team"], "atm-dev");
        assert_eq!(status["started_at"], "2026-02-18T00:00:00Z");
        assert_eq!(status["uptime_secs"], json!(42));
        assert_eq!(status["active_thread_count"], json!(0));
        assert_eq!(status["pending_mail_count"], json!(0));
        assert!(status["identity_map"].as_object().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_agent_status_with_active_session() {
        let reg = make_test_registry(10);
        let agent_id = {
            let mut guard = reg.lock().await;
            let e = guard
                .register(
                    "arch-ctm".to_string(),
                    "atm-dev".to_string(),
                    "/tmp".to_string(),
                    None,
                    None,
                    None,
                )
                .unwrap();
            guard.set_thread_id(&e.agent_id, "thread-abc".to_string());
            e.agent_id.clone()
        };
        let id = json!(201);
        let resp = handle_agent_status(
            &id,
            Arc::clone(&reg),
            true,
            "atm-dev",
            "2026-02-18T12:00:00Z",
            3600,
            0,
        )
        .await;
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let status: Value = serde_json::from_str(text).unwrap();
        assert_eq!(status["child_alive"], json!(true));
        assert_eq!(status["active_thread_count"], json!(1));
        let map = status["identity_map"].as_object().unwrap();
        assert_eq!(map.get("arch-ctm").and_then(|v| v.as_str()), Some("thread-abc"));
        let _ = agent_id;
    }

    #[tokio::test]
    async fn test_agent_status_stale_sessions_not_in_identity_map() {
        let reg = make_test_registry(10);
        {
            let mut guard = reg.lock().await;
            guard
                .register(
                    "stale-agent".to_string(),
                    "team".to_string(),
                    "/tmp".to_string(),
                    None,
                    None,
                    None,
                )
                .unwrap();
            guard.mark_all_stale();
        }
        let id = json!(202);
        let resp = handle_agent_status(
            &id,
            reg,
            false,
            "team",
            "2026-02-18T00:00:00Z",
            0,
            0,
        )
        .await;
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let status: Value = serde_json::from_str(text).unwrap();
        assert_eq!(status["active_thread_count"], json!(0));
        assert!(status["identity_map"].as_object().unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // Identity required error (proxy.rs constant is tested via integration)
    // -----------------------------------------------------------------------

    #[test]
    fn test_identity_required_error_code_value() {
        // Verify the constant value used in proxy.rs is correct
        assert_eq!(crate::proxy::ERR_IDENTITY_REQUIRED, -32009_i64);
    }

    // -----------------------------------------------------------------------
    // make_mcp_error_result shape
    // -----------------------------------------------------------------------

    #[test]
    fn test_make_mcp_error_result_shape() {
        let id = json!(42);
        let resp = make_mcp_error_result(&id, "something went wrong");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 42);
        assert_eq!(resp["result"]["isError"], json!(true));
        assert_eq!(
            resp["result"]["content"][0]["text"],
            "something went wrong"
        );
        // Must not be a JSON-RPC error response
        assert!(resp.get("error").is_none());
    }
}
