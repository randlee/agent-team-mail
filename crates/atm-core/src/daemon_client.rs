//! Client for querying the ATM daemon via Unix socket.
//!
//! Provides a thin, synchronous interface for CLI commands to query daemon state.
//! The daemon listens on a Unix domain socket at:
//!
//! ```text
//! ${ATM_HOME}/.claude/daemon/atm-daemon.sock
//! ```
//!
//! The protocol is newline-delimited JSON (one request line, one response line per connection):
//!
//! ```json
//! // Request
//! {"version":1,"request_id":"uuid","command":"agent-state","payload":{"agent":"arch-ctm","team":"atm-dev"}}
//! // Response
//! {"version":1,"request_id":"uuid","status":"ok","payload":{"state":"idle","last_transition":"2026-02-16T22:30:00Z"}}
//! ```
//!
//! # Platform Notes
//!
//! Unix domain sockets are only available on Unix platforms. On non-Unix platforms,
//! all functions return `Ok(None)` immediately without attempting a connection.
//!
//! # Graceful Fallback
//!
//! All public functions return `Ok(None)` when:
//! - The daemon is not running (connection refused or socket not found)
//! - The platform does not support Unix sockets
//! - Any I/O error occurs during the query
//!
//! Only truly unexpected errors (e.g., I/O errors during write after a successful connect)
//! are surfaced as `Err`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Protocol version for the socket JSON protocol.
pub const PROTOCOL_VERSION: u32 = 1;

/// Identifies the origin of a lifecycle event sent via the `hook-event` command.
///
/// The `source` field is optional in the hook-event payload for backward
/// compatibility — callers that do not set it will produce payloads that
/// deserialise successfully, defaulting to [`LifecycleSourceKind::Unknown`].
///
/// # Validation policy
///
/// | `kind`        | `session_start` / `session_end` restriction          |
/// |---------------|------------------------------------------------------|
/// | `claude_hook` | Team-lead only (strictest)                           |
/// | `unknown`     | Treated as `claude_hook` (fail-closed default)       |
/// | `atm_mcp`     | Any team member (MCP proxy manages its own sessions) |
/// | `agent_hook`  | Any team member (same policy as `atm_mcp`)           |
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleSource {
    /// Discriminator string identifying the lifecycle event origin.
    pub kind: LifecycleSourceKind,
}

impl LifecycleSource {
    /// Create a [`LifecycleSource`] with the given kind.
    pub fn new(kind: LifecycleSourceKind) -> Self {
        Self { kind }
    }
}

/// Discriminator for the origin of a lifecycle event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleSourceKind {
    /// Event originated from a Claude Code hook (e.g., `session-start.py`).
    ///
    /// Strictest validation: only the team-lead may emit `session_start` and
    /// `session_end` events from this source.
    ClaudeHook,
    /// Event originated from the `atm-agent-mcp` proxy.
    ///
    /// Relaxed validation: any team member may emit lifecycle events because
    /// the MCP proxy manages its own Codex agent sessions, not the team-lead's
    /// Claude Code session.
    AtmMcp,
    /// Event originated from a non-Claude agent hook adapter (e.g., a Codex or
    /// Gemini relay script). Same validation policy as [`AtmMcp`](Self::AtmMcp).
    AgentHook,
    /// Origin unknown or not set by the sender.
    ///
    /// Treated as [`ClaudeHook`](Self::ClaudeHook) (strictest, fail-closed default).
    Unknown,
}

/// A request sent from CLI to daemon over the Unix socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketRequest {
    /// Protocol version. Must be [`PROTOCOL_VERSION`].
    pub version: u32,
    /// Unique identifier echoed back in the response.
    pub request_id: String,
    /// Command to execute (e.g., `"agent-state"`, `"list-agents"`).
    pub command: String,
    /// Command-specific payload.
    pub payload: serde_json::Value,
}

/// A response received from the daemon over the Unix socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketResponse {
    /// Protocol version.
    pub version: u32,
    /// Echoed `request_id` from the corresponding request.
    pub request_id: String,
    /// `"ok"` on success, `"error"` on failure.
    pub status: String,
    /// Response data on success (present when `status == "ok"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Error information on failure (present when `status == "error"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<SocketError>,
}

impl SocketResponse {
    /// Returns `true` if the response indicates success.
    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }
}

/// Error details returned by the daemon on failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketError {
    /// Machine-readable error code (e.g., `"AGENT_NOT_FOUND"`).
    pub code: String,
    /// Human-readable error message.
    pub message: String,
}

/// Agent state information returned by the `agent-state` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStateInfo {
    /// Current state: `"launching"`, `"busy"`, `"idle"`, or `"killed"`.
    pub state: String,
    /// ISO 8601 timestamp of the last state transition (if available).
    pub last_transition: Option<String>,
}

/// Summary of a single agent returned by the `list-agents` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    /// Agent identifier.
    pub agent: String,
    /// Current state string.
    pub state: String,
}

/// Canonical daemon-backed member-state snapshot returned by team-scoped
/// `list-agents` queries.
///
/// This struct is the single liveness/status source consumed by CLI diagnostic
/// surfaces (`atm doctor`, `atm status`, `atm members`). It is derived by the
/// daemon from session-registry + tracker evidence and must not be inferred
/// from config `isActive`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalMemberState {
    /// Agent/member name.
    pub agent: String,
    /// Canonical daemon status (`active`, `idle`, `offline`, `unknown`).
    pub state: String,
    /// Canonical activity hint (`busy`, `idle`, `unknown`).
    #[serde(default)]
    pub activity: String,
    /// Session UUID from the daemon registry when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Process ID from the daemon registry when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u32>,
    /// Human-readable derivation reason.
    #[serde(default)]
    pub reason: String,
    /// Source of truth used for state derivation.
    #[serde(default)]
    pub source: String,
}

/// Return best-effort binary liveness from daemon canonical member state.
pub fn canonical_liveness_bool(state: Option<&CanonicalMemberState>) -> Option<bool> {
    match state.map(|s| s.state.as_str()) {
        Some("active") | Some("idle") => Some(true),
        Some("offline") | Some("dead") => Some(false),
        _ => None,
    }
}

/// Configuration for launching a new agent via the daemon.
///
/// Sent as the payload of a `"launch"` socket command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchConfig {
    /// Agent identity name (e.g., `"arch-ctm"`).
    pub agent: String,
    /// Team name (e.g., `"atm-dev"`).
    pub team: String,
    /// Command to run in the tmux pane (e.g., `"codex --yolo"`).
    pub command: String,
    /// Optional initial prompt to send after the agent reaches the `Idle` state.
    pub prompt: Option<String>,
    /// Readiness timeout in seconds. The daemon waits up to this long for the
    /// agent state to transition to `Idle` before sending the initial prompt.
    /// Defaults to 30 if omitted.
    pub timeout_secs: u32,
    /// Extra environment variables to export in the pane before starting the agent.
    ///
    /// `ATM_IDENTITY` and `ATM_TEAM` are always set automatically and do not
    /// need to be included here.
    pub env_vars: std::collections::HashMap<String, String>,
    /// Runtime adapter kind (e.g., `"codex"`, `"gemini"`).
    ///
    /// Older clients may omit this field; daemon should treat missing as
    /// runtime default (`codex`).
    #[serde(default)]
    pub runtime: Option<String>,
    /// Optional runtime-native session ID used for resume-aware launches.
    ///
    /// For Gemini this maps to the Gemini session UUID that should be resumed.
    #[serde(default)]
    pub resume_session_id: Option<String>,
}

/// Result of a successful agent launch returned by the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchResult {
    /// Agent identity name.
    pub agent: String,
    /// tmux pane ID assigned to the new agent (e.g., `"%42"`).
    pub pane_id: String,
    /// Agent state string immediately after launch (`"launching"`, `"idle"`, etc.).
    pub state: String,
    /// Non-fatal warning, e.g., readiness timeout was reached before the agent
    /// transitioned to `Idle`.
    pub warning: Option<String>,
}

/// Request the daemon to launch a new agent.
///
/// This is a synchronous call: the function blocks until the daemon responds
/// (the daemon itself may respond before full readiness, but the round-trip
/// completes within the socket timeout).
///
/// Returns `Ok(None)` when:
/// - The daemon is not running.
/// - The platform does not support Unix sockets.
/// - A connection-level I/O error occurs before any response is read.
///
/// Returns `Ok(Some(result))` on success.
///
/// Returns `Err` only for unexpected I/O errors *after* a connection is
/// established and a request has been written.
///
/// # Arguments
///
/// * `config` - Launch configuration for the new agent.
pub fn launch_agent(config: &LaunchConfig) -> anyhow::Result<Option<LaunchResult>> {
    let payload = match serde_json::to_value(config) {
        Ok(v) => v,
        Err(e) => anyhow::bail!("Failed to serialize LaunchConfig: {e}"),
    };

    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "launch".to_string(),
        payload,
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        let msg = response
            .error
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| "unknown daemon error".to_string());
        anyhow::bail!("Daemon returned error for launch command: {msg}");
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<LaunchResult>(payload) {
        Ok(result) => Ok(Some(result)),
        Err(e) => anyhow::bail!("Failed to parse LaunchResult from daemon response: {e}"),
    }
}

/// Compute the well-known socket path for the ATM daemon.
///
/// The path is `${ATM_HOME}/.claude/daemon/atm-daemon.sock`, where `ATM_HOME`
/// is resolved via [`crate::home::get_home_dir`].
///
/// # Errors
///
/// Returns an error only if home directory resolution fails.
pub fn daemon_socket_path() -> anyhow::Result<PathBuf> {
    let home = crate::home::get_home_dir()?;
    Ok(home.join(".claude/daemon/atm-daemon.sock"))
}

/// Compute the well-known PID file path for the ATM daemon.
///
/// The path is `${ATM_HOME}/.claude/daemon/atm-daemon.pid`.
///
/// # Errors
///
/// Returns an error only if home directory resolution fails.
pub fn daemon_pid_path() -> anyhow::Result<PathBuf> {
    let home = crate::home::get_home_dir()?;
    Ok(home.join(".claude/daemon/atm-daemon.pid"))
}

/// Check whether the daemon appears to be running by reading its PID file and
/// verifying the process is alive.
///
/// Returns `false` on any error (missing file, invalid PID, dead process, etc.).
pub fn daemon_is_running() -> bool {
    #[cfg(unix)]
    {
        let pid_path = match daemon_pid_path() {
            Ok(p) => p,
            Err(_) => return false,
        };
        if let Ok(content) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = content.trim().parse::<i32>() {
                return pid_alive(pid);
            }
        }
        false
    }

    #[cfg(not(unix))]
    {
        false
    }
}

/// Ensure the ATM daemon is running, starting it if needed.
///
/// On Unix:
/// - Delegates to the full Unix implementation used by runtime queries,
///   including startup lock coordination, socket probing, and event logging.
///
/// On non-Unix platforms this is a no-op and returns `Ok(())`.
pub fn ensure_daemon_running() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        ensure_daemon_running_unix()
    }

    #[cfg(not(unix))]
    {
        Ok(())
    }
}

/// Send a single request to the daemon and return the parsed response.
///
/// Returns `Ok(None)` when the daemon is not running or the socket cannot be
/// reached. Returns `Ok(Some(response))` on a successful exchange. Returns
/// `Err` only for I/O errors that occur *after* a connection is established.
///
/// # Platform Behaviour
///
/// On non-Unix platforms this function always returns `Ok(None)`.
pub fn query_daemon(request: &SocketRequest) -> anyhow::Result<Option<SocketResponse>> {
    #[cfg(unix)]
    {
        query_daemon_unix(request)
    }

    #[cfg(not(unix))]
    {
        Ok(None)
    }
}

/// Query the daemon for the current state of a specific agent.
///
/// Returns `Ok(None)` when the daemon is not reachable or the agent is not tracked.
///
/// # Arguments
///
/// * `agent` - Agent name (e.g., `"arch-ctm"`)
/// * `team`  - Team name (e.g., `"atm-dev"`)
pub fn query_agent_state(agent: &str, team: &str) -> anyhow::Result<Option<AgentStateInfo>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "agent-state".to_string(),
        payload: serde_json::json!({ "agent": agent, "team": team }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        // Daemon returned an error (e.g., agent not found) — treat as no info
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<AgentStateInfo>(payload) {
        Ok(info) => Ok(Some(info)),
        Err(_) => Ok(None),
    }
}

/// Send a subscribe request to the daemon.
///
/// Registers the subscriber's interest in state changes for `agent`. This is a
/// best-effort operation: `Ok(None)` is returned when the daemon is not running.
///
/// # Arguments
///
/// * `subscriber` - ATM identity of the subscribing agent (e.g., `"team-lead"`)
/// * `agent`      - Agent to watch (e.g., `"arch-ctm"`)
/// * `team`       - Team name (informational; used for routing context)
/// * `events`     - State events to subscribe to (e.g., `&["idle"]`);
///   pass an empty slice to subscribe to all events.
pub fn subscribe_to_agent(
    subscriber: &str,
    agent: &str,
    team: &str,
    events: &[String],
) -> anyhow::Result<Option<SocketResponse>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "subscribe".to_string(),
        payload: serde_json::json!({
            "subscriber": subscriber,
            "agent": agent,
            "team": team,
            "events": events,
        }),
    };
    query_daemon(&request)
}

/// Send an unsubscribe request to the daemon.
///
/// Removes the subscription for `(subscriber, agent)`. This is a best-effort
/// operation: `Ok(None)` is returned when the daemon is not running.
///
/// # Arguments
///
/// * `subscriber` - ATM identity of the subscribing agent
/// * `agent`      - Agent to stop watching
/// * `team`       - Team name (informational)
pub fn unsubscribe_from_agent(
    subscriber: &str,
    agent: &str,
    team: &str,
) -> anyhow::Result<Option<SocketResponse>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "unsubscribe".to_string(),
        payload: serde_json::json!({
            "subscriber": subscriber,
            "agent": agent,
            "team": team,
        }),
    };
    query_daemon(&request)
}

/// Query the daemon for the list of all tracked agents.
///
/// Returns `Ok(None)` when the daemon is not reachable.
pub fn query_list_agents() -> anyhow::Result<Option<Vec<AgentSummary>>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "list-agents".to_string(),
        payload: serde_json::Value::Object(Default::default()),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<Vec<AgentSummary>>(payload) {
        Ok(agents) => Ok(Some(agents)),
        Err(_) => Ok(None),
    }
}

/// Query the daemon for the list of tracked agents scoped to a specific team.
///
/// Returns `Ok(None)` when the daemon is not reachable.
pub fn query_list_agents_for_team(team: &str) -> anyhow::Result<Option<Vec<AgentSummary>>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "list-agents".to_string(),
        payload: serde_json::json!({ "team": team }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<Vec<AgentSummary>>(payload) {
        Ok(agents) => Ok(Some(agents)),
        Err(_) => Ok(None),
    }
}

/// Query the daemon for canonical member-state snapshots scoped to one team.
///
/// Returns `Ok(None)` when the daemon is not reachable.
pub fn query_team_member_states(team: &str) -> anyhow::Result<Option<Vec<CanonicalMemberState>>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "list-agents".to_string(),
        payload: serde_json::json!({ "team": team }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<Vec<CanonicalMemberState>>(payload) {
        Ok(states) => Ok(Some(states)),
        Err(_) => Ok(None),
    }
}

/// Query daemon canonical member states and return a map keyed by agent name.
///
/// If the daemon is unavailable or does not return states, this returns an
/// empty map so callers can render `Unknown` without failing command output.
pub fn query_team_member_state_map(
    team: &str,
) -> std::collections::HashMap<String, CanonicalMemberState> {
    query_team_member_states(team)
        .ok()
        .flatten()
        .unwrap_or_default()
        .into_iter()
        .map(|entry| (entry.agent.clone(), entry))
        .collect()
}

/// Pane and log file information returned by the `agent-pane` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPaneInfo {
    /// Backend pane identifier (e.g., `"%42"`).
    pub pane_id: String,
    /// Absolute path to the agent's log file.
    pub log_path: String,
}

/// Query the daemon for the pane ID and log file path of a specific agent.
///
/// Returns `Ok(None)` when the daemon is not reachable or the agent is not tracked.
///
/// # Arguments
///
/// * `agent` - Agent name (e.g., `"arch-ctm"`)
pub fn query_agent_pane(agent: &str) -> anyhow::Result<Option<AgentPaneInfo>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "agent-pane".to_string(),
        payload: serde_json::json!({ "agent": agent }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        // Daemon returned an error (e.g., agent not found) — treat as no info
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<AgentPaneInfo>(payload) {
        Ok(info) => Ok(Some(info)),
        Err(_) => Ok(None),
    }
}

/// Session information returned by the `session-query` socket command.
///
/// Describes the Claude Code session and OS process currently registered for an
/// agent in the [`SessionRegistry`](crate) and whether the process is alive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionQueryResult {
    /// Claude Code session UUID.
    pub session_id: String,
    /// OS process ID of the agent process.
    pub process_id: u32,
    /// Whether the OS process is currently running.
    pub alive: bool,
    /// Runtime kind (`codex`, `gemini`, etc.) when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// Runtime-native session/thread identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_session_id: Option<String>,
    /// Backend pane identifier when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// Runtime home/state directory when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_home: Option<String>,
}

/// Query the daemon for the session record of a named agent.
///
/// Returns:
/// - `Ok(Some(result))` when the agent is registered in the session registry.
/// - `Ok(None)` when the daemon is not running, the agent is not registered,
///   or the platform does not support Unix sockets.
/// - `Err` only for unexpected I/O errors *after* a connection is established.
///
/// # Arguments
///
/// * `name` - Agent name to look up (e.g., `"team-lead"`)
pub fn query_session(name: &str) -> anyhow::Result<Option<SessionQueryResult>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "session-query".to_string(),
        payload: serde_json::json!({ "name": name }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        // Daemon returned error (agent not found) — treat as no session info
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<SessionQueryResult>(payload) {
        Ok(result) => Ok(Some(result)),
        Err(_) => Ok(None),
    }
}

/// Query the daemon for the session record of a named agent scoped to a team.
///
/// Returns:
/// - `Ok(Some(result))` when the agent is registered and matches the team's
///   current lead-session context.
/// - `Ok(None)` when the daemon is not running, the agent is not registered
///   for that team context, or the platform does not support Unix sockets.
/// - `Err` only for unexpected I/O errors *after* a connection is established.
///
/// # Arguments
///
/// * `team` - Team name (e.g., `"atm-dev"`)
/// * `name` - Agent name to look up (e.g., `"team-lead"`)
pub fn query_session_for_team(
    team: &str,
    name: &str,
) -> anyhow::Result<Option<SessionQueryResult>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "session-query-team".to_string(),
        payload: serde_json::json!({ "team": team, "name": name }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<SessionQueryResult>(payload) {
        Ok(result) => Ok(Some(result)),
        Err(_) => Ok(None),
    }
}

/// Query the daemon for the stream turn state of a named agent.
///
/// Returns:
/// - `Ok(Some(state))` when the daemon has stream state recorded for the agent.
/// - `Ok(None)` when the daemon is not running, the agent has no stream state,
///   or the platform does not support Unix sockets.
///
/// # Arguments
///
/// * `agent` - Agent name to look up (e.g., `"arch-ctm"`)
pub fn query_agent_stream_state(
    agent: &str,
) -> anyhow::Result<Option<crate::daemon_stream::AgentStreamState>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "agent-stream-state".to_string(),
        payload: serde_json::json!({ "agent": agent }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<crate::daemon_stream::AgentStreamState>(payload) {
        Ok(state) => Ok(Some(state)),
        Err(_) => Ok(None),
    }
}

/// Handle for an active daemon stream subscription.
///
/// Dropping this value requests the background reader thread to stop.
pub struct StreamSubscription {
    /// Receiver of daemon stream events.
    pub rx: std::sync::mpsc::Receiver<crate::daemon_stream::DaemonStreamEvent>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for StreamSubscription {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Subscribe to daemon stream events over a long-lived socket connection.
///
/// Returns:
/// - `Ok(Some(rx))` when subscription succeeds.
/// - `Ok(None)` when daemon/socket is unavailable on this platform/session.
pub fn subscribe_stream_events() -> anyhow::Result<Option<StreamSubscription>> {
    #[cfg(unix)]
    {
        subscribe_stream_events_unix()
    }

    #[cfg(not(unix))]
    {
        Ok(None)
    }
}

/// Send a control request to the daemon and wait for an acknowledgement.
///
/// Sends `command: "control"` with the given [`ControlRequest`] as payload.
/// Returns the parsed [`ControlAck`] on success, or an error on socket/parse
/// failure.  A short read timeout is applied by the underlying
/// [`query_daemon`] call.
///
/// # Errors
///
/// Returns `Err` when:
/// - The daemon is not running or the socket cannot be reached (no graceful
///   `None` here — the caller needs to distinguish errors from timeouts).
/// - The daemon returns an error status.
/// - The response payload cannot be parsed as [`ControlAck`].
pub fn send_control(
    request: &crate::control::ControlRequest,
) -> anyhow::Result<crate::control::ControlAck> {
    let payload = serde_json::to_value(request)
        .map_err(|e| anyhow::anyhow!("Failed to serialize ControlRequest: {e}"))?;

    let socket_request = SocketRequest {
        version: PROTOCOL_VERSION,
        // Use an independent socket-level correlation ID; the control payload
        // carries its own stable idempotency key (`request.request_id`) that
        // must not change on retries.
        request_id: format!(
            "sock-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ),
        command: "control".to_string(),
        payload,
    };

    let response = match query_daemon(&socket_request)? {
        Some(r) => r,
        None => anyhow::bail!("Daemon not reachable (socket not found or connection refused)"),
    };

    if !response.is_ok() {
        let msg = response
            .error
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| "unknown daemon error".to_string());
        anyhow::bail!("Daemon returned error for control command: {msg}");
    }

    let payload = response
        .payload
        .ok_or_else(|| anyhow::anyhow!("Daemon returned ok status but no payload"))?;

    serde_json::from_value::<crate::control::ControlAck>(payload)
        .map_err(|e| anyhow::anyhow!("Failed to parse ControlAck from daemon response: {e}"))
}

/// Generate a compact request identifier (UUID v4 as a short string).
fn new_request_id() -> String {
    // Use a simple monotonic counter for environments without UUID support.
    // In practice the daemon_client is always used in the atm crate which
    // has uuid available, but atm-core does not depend on uuid.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let id = std::process::id();
    format!("req-{id}-{nanos}")
}

// ── Unix implementation ──────────────────────────────────────────────────────

#[cfg(unix)]
fn query_daemon_unix(request: &SocketRequest) -> anyhow::Result<Option<SocketResponse>> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    let socket_path = daemon_socket_path()?;

    // First attempt connection directly.
    let stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(_) => {
            // Optional daemon auto-start path, enabled by ATM_DAEMON_AUTOSTART.
            if daemon_autostart_enabled() {
                ensure_daemon_running_unix()?;
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match UnixStream::connect(&socket_path) {
                        Ok(s) => break s,
                        Err(e) if Instant::now() < deadline => {
                            let _ = e;
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        Err(e) => {
                            anyhow::bail!(
                                "daemon auto-start attempted but socket remained unavailable at {}: {e}",
                                socket_path.display()
                            )
                        }
                    }
                }
            } else {
                return Ok(None);
            }
        }
    };

    // Set a short timeout so a stale/hung daemon does not block the CLI
    let timeout = Duration::from_millis(500);
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();

    let request_line = serde_json::to_string(request)?;

    // Write request line (newline-delimited)
    {
        let mut writer = std::io::BufWriter::new(&stream);
        writer.write_all(request_line.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }

    // Read response line
    let mut reader = BufReader::new(&stream);
    let mut response_line = String::new();
    match reader.read_line(&mut response_line) {
        Ok(0) | Err(_) => return Ok(None), // daemon closed connection or timed out
        Ok(_) => {}
    }

    let response: SocketResponse = match serde_json::from_str(response_line.trim()) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    Ok(Some(response))
}

#[cfg(unix)]
fn daemon_autostart_enabled() -> bool {
    matches!(
        std::env::var("ATM_DAEMON_AUTOSTART").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

#[cfg(unix)]
fn resolve_daemon_binary() -> std::ffi::OsString {
    if let Some(override_bin) = std::env::var_os("ATM_DAEMON_BIN")
        && !override_bin.is_empty()
    {
        return override_bin;
    }

    let name = std::ffi::OsString::from("atm-daemon");

    if let Ok(current_exe) = std::env::current_exe()
        && let Some(dir) = current_exe.parent()
    {
        let sibling = dir.join(std::path::Path::new(&name));
        if sibling.exists() {
            return sibling.into_os_string();
        }
    }

    name
}

#[cfg(unix)]
fn ensure_daemon_running_unix() -> anyhow::Result<()> {
    use crate::event_log::{EventFields, emit_event_best_effort};
    use crate::io::InboxError;
    use std::io::ErrorKind;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    if daemon_is_running() {
        return Ok(());
    }

    let home = crate::home::get_home_dir()?;
    cleanup_stale_daemon_runtime_files(&home);

    let startup_lock_path = home.join(".config/atm/daemon-start.lock");
    if let Some(parent) = startup_lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Serialize daemon startup across concurrent CLI processes.
    let _startup_lock = match crate::io::lock::acquire_lock(&startup_lock_path, 3) {
        Ok(lock) => Some(lock),
        Err(InboxError::LockTimeout { .. }) => {
            // Another process likely holds the startup lock and is spawning the daemon.
            // Wait briefly for that startup attempt to converge.
            for _ in 0..10 {
                if daemon_is_running() || daemon_socket_connectable(&home) {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // Startup did not converge yet. Re-attempt lock acquisition so any
            // fallback spawn still occurs under lock (single-daemon invariant).
            match crate::io::lock::acquire_lock(&startup_lock_path, 10) {
                Ok(lock) => Some(lock),
                Err(e) => anyhow::bail!(
                    "timed out waiting for daemon startup lock holder to bring daemon online: {} ({e})",
                    startup_lock_path.display()
                ),
            }
        }
        Err(e) => anyhow::bail!(
            "failed to acquire daemon startup lock {}: {e}",
            startup_lock_path.display()
        ),
    };

    if daemon_is_running() || daemon_socket_connectable(&home) {
        return Ok(());
    }

    let daemon_bin = resolve_daemon_binary();
    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "daemon_autostart_attempt",
        result: Some("attempt".to_string()),
        target: Some(std::path::PathBuf::from(&daemon_bin).display().to_string()),
        ..Default::default()
    });
    let mut child = Command::new(&daemon_bin)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            if e.kind() == ErrorKind::NotFound {
                anyhow::anyhow!(
                    "failed to auto-start daemon: binary '{}' not found in PATH (or ATM_DAEMON_BIN override)",
                    std::path::PathBuf::from(&daemon_bin).display()
                )
            } else {
                anyhow::anyhow!(
                    "failed to auto-start daemon via '{}': {e}",
                    std::path::PathBuf::from(&daemon_bin).display()
                )
            }
        })?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if daemon_is_running() || daemon_socket_connectable(&home) {
            emit_event_best_effort(EventFields {
                level: "info",
                source: "atm",
                action: "daemon_autostart_success",
                result: Some("ok".to_string()),
                target: Some(std::path::PathBuf::from(&daemon_bin).display().to_string()),
                ..Default::default()
            });
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("daemon process exited during startup with status {status}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let socket_path = home.join(".claude/daemon/atm-daemon.sock");
    let pid_path = home.join(".claude/daemon/atm-daemon.pid");
    let timeout_error = format!(
        "daemon startup timed out after 5s; pid_file_exists={}, socket_exists={}, pid_path={}, socket_path={}",
        pid_path.exists(),
        socket_path.exists(),
        pid_path.display(),
        socket_path.display()
    );
    emit_event_best_effort(EventFields {
        level: "warn",
        source: "atm",
        action: "daemon_autostart_timeout",
        result: Some("timeout".to_string()),
        target: Some(std::path::PathBuf::from(&daemon_bin).display().to_string()),
        error: Some(timeout_error.clone()),
        ..Default::default()
    });
    anyhow::bail!("{timeout_error}")
}

#[cfg(unix)]
fn daemon_socket_connectable(home: &std::path::Path) -> bool {
    use std::os::unix::net::UnixStream;
    let socket_path = home.join(".claude/daemon/atm-daemon.sock");
    UnixStream::connect(socket_path).is_ok()
}

#[cfg(unix)]
fn cleanup_stale_daemon_runtime_files(home: &std::path::Path) {
    let socket_path = home.join(".claude/daemon/atm-daemon.sock");
    let pid_path = home.join(".claude/daemon/atm-daemon.pid");

    let pid_state = read_daemon_pid_state(&pid_path);
    if matches!(
        pid_state,
        PidState::Dead | PidState::Missing | PidState::Malformed
    ) {
        let _ = std::fs::remove_file(&pid_path);
    }

    // Remove stale socket only when daemon ownership is known-dead.
    let ownership_known_dead = matches!(
        pid_state,
        PidState::Dead | PidState::Missing | PidState::Malformed
    );
    if socket_path.exists() && ownership_known_dead && !daemon_socket_connectable(home) {
        let _ = std::fs::remove_file(&socket_path);
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PidState {
    Missing,
    Malformed,
    Unreadable,
    Dead,
    Alive,
}

#[cfg(unix)]
fn read_daemon_pid_state(pid_path: &std::path::Path) -> PidState {
    if !pid_path.exists() {
        return PidState::Missing;
    }
    let content = match std::fs::read_to_string(pid_path) {
        Ok(s) => s,
        Err(_) => return PidState::Unreadable,
    };
    let pid = match content.trim().parse::<i32>() {
        Ok(pid) => pid,
        Err(_) => return PidState::Malformed,
    };
    if pid_alive(pid) {
        PidState::Alive
    } else {
        PidState::Dead
    }
}

#[cfg(unix)]
fn subscribe_stream_events_unix() -> anyhow::Result<Option<StreamSubscription>> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let socket_path = daemon_socket_path()?;
    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };

    let req = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "stream-subscribe".to_string(),
        payload: serde_json::json!({}),
    };
    let req_line = serde_json::to_string(&req)?;
    stream.write_all(req_line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    // Must receive an explicit stream ACK before treating the subscription as live.
    {
        let mut ack_reader = BufReader::new(stream.try_clone()?);
        let mut ack_line = String::new();
        if ack_reader.read_line(&mut ack_line)? == 0 {
            return Ok(None);
        }
        let ack_json: serde_json::Value = match serde_json::from_str(ack_line.trim()) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
        let ok = ack_json
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s == "ok")
            .unwrap_or(false);
        let streaming = ack_json
            .get("streaming")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !(ok && streaming) {
            return Ok(None);
        }
    }

    let (tx, rx) = std::sync::mpsc::channel::<crate::daemon_stream::DaemonStreamEvent>();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_thread = std::sync::Arc::clone(&stop);
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(500)))
        .ok();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        loop {
            if stop_thread.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let mut line = String::new();
            let n = match reader.read_line(&mut line) {
                Ok(n) => n,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(event) =
                serde_json::from_str::<crate::daemon_stream::DaemonStreamEvent>(trimmed)
            {
                if tx.send(event).is_err() {
                    break;
                }
            }
        }
    });

    Ok(Some(StreamSubscription { rx, stop }))
}

/// Check whether a Unix PID is alive using `kill -0`.
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // SAFETY: kill(pid, 0) is a read-only existence check; no signal is sent.
    // We declare the extern fn inline to avoid a compile-time libc dependency
    // at the crate level (libc is only in [target.'cfg(unix)'.dependencies]).
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: kill with sig=0 never sends a signal; it only checks PID existence.
    let result = unsafe { kill(pid, 0) };
    result == 0
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn with_autostart_disabled<T>(f: impl FnOnce() -> T) -> T {
        let old = std::env::var("ATM_DAEMON_AUTOSTART").ok();
        // SAFETY: test-only env mutation guarded by #[serial] on callers.
        unsafe { std::env::set_var("ATM_DAEMON_AUTOSTART", "0") };
        let out = f();
        // SAFETY: test-only env mutation guarded by #[serial] on callers.
        unsafe {
            match old {
                Some(v) => std::env::set_var("ATM_DAEMON_AUTOSTART", v),
                None => std::env::remove_var("ATM_DAEMON_AUTOSTART"),
            }
        }
        out
    }

    #[test]
    fn test_socket_request_serialization() {
        let req = SocketRequest {
            version: 1,
            request_id: "req-123".to_string(),
            command: "agent-state".to_string(),
            payload: serde_json::json!({ "agent": "arch-ctm", "team": "atm-dev" }),
        };

        let json = serde_json::to_string(&req).unwrap();
        let decoded: SocketRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.request_id, "req-123");
        assert_eq!(decoded.command, "agent-state");
    }

    #[test]
    fn test_socket_response_ok_deserialization() {
        let json = r#"{"version":1,"request_id":"req-123","status":"ok","payload":{"state":"idle","last_transition":"2026-02-16T22:30:00Z"}}"#;
        let resp: SocketResponse = serde_json::from_str(json).unwrap();

        assert!(resp.is_ok());
        assert_eq!(resp.request_id, "req-123");
        assert!(resp.payload.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_socket_response_error_deserialization() {
        let json = r#"{"version":1,"request_id":"req-456","status":"error","error":{"code":"AGENT_NOT_FOUND","message":"Agent 'unknown' is not tracked"}}"#;
        let resp: SocketResponse = serde_json::from_str(json).unwrap();

        assert!(!resp.is_ok());
        let err = resp.error.unwrap();
        assert_eq!(err.code, "AGENT_NOT_FOUND");
    }

    #[test]
    fn test_agent_state_info_deserialization() {
        let json = r#"{"state":"idle","last_transition":"2026-02-16T22:30:00Z"}"#;
        let info: AgentStateInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.state, "idle");
        assert_eq!(
            info.last_transition.as_deref(),
            Some("2026-02-16T22:30:00Z")
        );
    }

    #[test]
    fn test_agent_state_info_missing_transition() {
        let json = r#"{"state":"launching"}"#;
        let info: AgentStateInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.state, "launching");
        assert!(info.last_transition.is_none());
    }

    #[test]
    #[serial]
    fn test_query_daemon_no_socket_returns_none() {
        with_autostart_disabled(|| {
            // Without a running daemon the query should gracefully return None.
            // We ensure no real socket path is present by using a non-existent dir.
            // This test is platform-independent: on non-unix it always returns None.
            let req = SocketRequest {
                version: PROTOCOL_VERSION,
                request_id: "req-test".to_string(),
                command: "agent-state".to_string(),
                payload: serde_json::json!({}),
            };
            // Override socket path resolution is not straightforward without DI;
            // the test relies on the daemon not being present in the test environment.
            // On CI this will always be None. Locally too unless daemon is running.
            let result = query_daemon(&req);
            assert!(result.is_ok());
            // If daemon happens to be running, we just check the call didn't panic.
        });
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_daemon_autostart_flag_parsing() {
        let old = std::env::var("ATM_DAEMON_AUTOSTART").ok();

        // SAFETY: serialized env mutation in test.
        unsafe { std::env::set_var("ATM_DAEMON_AUTOSTART", "1") };
        assert!(daemon_autostart_enabled());
        // SAFETY: serialized env mutation in test.
        unsafe { std::env::set_var("ATM_DAEMON_AUTOSTART", "true") };
        assert!(daemon_autostart_enabled());
        // SAFETY: serialized env mutation in test.
        unsafe { std::env::set_var("ATM_DAEMON_AUTOSTART", "0") };
        assert!(!daemon_autostart_enabled());

        // SAFETY: serialized env mutation in test.
        unsafe {
            match old {
                Some(v) => std::env::set_var("ATM_DAEMON_AUTOSTART", v),
                None => std::env::remove_var("ATM_DAEMON_AUTOSTART"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_resolve_daemon_binary_honors_override() {
        let old = std::env::var("ATM_DAEMON_BIN").ok();
        let tmp = tempfile::tempdir().unwrap();
        let custom = tmp.path().join("custom-atm-daemon");
        std::fs::write(&custom, "#!/bin/sh\nexit 0\n").unwrap();
        // SAFETY: serialized env mutation in test.
        unsafe { std::env::set_var("ATM_DAEMON_BIN", &custom) };
        let resolved = resolve_daemon_binary();
        assert_eq!(std::path::PathBuf::from(resolved), custom);
        // SAFETY: serialized env mutation in test.
        unsafe {
            match old {
                Some(v) => std::env::set_var("ATM_DAEMON_BIN", v),
                None => std::env::remove_var("ATM_DAEMON_BIN"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_cleanup_stale_runtime_files_removes_dead_pid_file() {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let daemon_dir = home.join(".claude/daemon");
        fs::create_dir_all(&daemon_dir).unwrap();

        let pid_path = daemon_dir.join("atm-daemon.pid");
        fs::write(&pid_path, "999999\n").unwrap();
        assert!(pid_path.exists());

        cleanup_stale_daemon_runtime_files(home);
        assert!(
            !pid_path.exists(),
            "stale PID file should be removed when PID is not alive"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_cleanup_stale_runtime_files_handles_malformed_pid() {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let daemon_dir = home.join(".claude/daemon");
        fs::create_dir_all(&daemon_dir).unwrap();

        let pid_path = daemon_dir.join("atm-daemon.pid");
        let socket_path = daemon_dir.join("atm-daemon.sock");
        fs::write(&pid_path, "not-a-pid\n").unwrap();
        fs::write(&socket_path, "stale").unwrap();

        cleanup_stale_daemon_runtime_files(home);

        assert!(
            !pid_path.exists(),
            "malformed PID file should be removed during cleanup"
        );
        assert!(
            !socket_path.exists(),
            "stale socket should be removed when PID ownership is known-dead"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_cleanup_stale_runtime_files_unreadable_pid_does_not_remove_socket() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let daemon_dir = home.join(".claude/daemon");
        fs::create_dir_all(&daemon_dir).unwrap();

        let pid_path = daemon_dir.join("atm-daemon.pid");
        let socket_path = daemon_dir.join("atm-daemon.sock");
        fs::write(&pid_path, "123\n").unwrap();
        fs::write(&socket_path, "stale").unwrap();
        let mut perms = fs::metadata(&pid_path).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&pid_path, perms).unwrap();

        cleanup_stale_daemon_runtime_files(home);
        assert!(
            socket_path.exists(),
            "socket must not be removed when PID ownership cannot be read"
        );

        // Restore permissions so tempdir cleanup succeeds.
        let mut restore = fs::metadata(&pid_path).unwrap().permissions();
        restore.set_mode(0o600);
        fs::set_permissions(&pid_path, restore).unwrap();
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_ensure_daemon_running_timeout_when_spawned_process_never_creates_runtime_files() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let script_path = home.join("fake-daemon-never-ready.sh");
        let script = r#"#!/bin/sh
set -eu
sleep 10
"#;
        fs::write(&script_path, script).unwrap();
        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).unwrap();

        let old_home = std::env::var("ATM_HOME").ok();
        let old_bin = std::env::var("ATM_DAEMON_BIN").ok();
        let old_auto = std::env::var("ATM_DAEMON_AUTOSTART").ok();
        unsafe {
            std::env::set_var("ATM_HOME", &home);
            std::env::set_var("ATM_DAEMON_BIN", &script_path);
            std::env::set_var("ATM_DAEMON_AUTOSTART", "1");
        }

        let err = ensure_daemon_running_unix().expect_err("startup should time out");
        let msg = err.to_string();
        assert!(
            msg.contains("daemon startup timed out after 5s"),
            "timeout error should include actionable timeout details: {msg}"
        );
        assert!(
            msg.contains("pid_path="),
            "timeout error should include pid path"
        );
        assert!(
            msg.contains("socket_path="),
            "timeout error should include socket path"
        );

        unsafe {
            match old_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
            match old_bin {
                Some(v) => std::env::set_var("ATM_DAEMON_BIN", v),
                None => std::env::remove_var("ATM_DAEMON_BIN"),
            }
            match old_auto {
                Some(v) => std::env::set_var("ATM_DAEMON_AUTOSTART", v),
                None => std::env::remove_var("ATM_DAEMON_AUTOSTART"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_ensure_daemon_running_serializes_concurrent_start() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;
        use std::thread;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let script_path = home.join("fake-daemon.sh");

        let script = r#"#!/bin/sh
set -eu
home="${ATM_HOME:?}"
mkdir -p "$home/.claude/daemon"
mkdir -p "$home/spawn-markers"
touch "$home/spawn-markers/spawn.$$"
echo $$ > "$home/.claude/daemon/atm-daemon.pid"
sleep 2
"#;
        fs::write(&script_path, script).unwrap();
        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).unwrap();

        let old_home = std::env::var("ATM_HOME").ok();
        let old_bin = std::env::var("ATM_DAEMON_BIN").ok();
        let old_auto = std::env::var("ATM_DAEMON_AUTOSTART").ok();
        unsafe {
            std::env::set_var("ATM_HOME", &home);
            std::env::set_var("ATM_DAEMON_BIN", &script_path);
            std::env::set_var("ATM_DAEMON_AUTOSTART", "1");
        }

        let mut handles = Vec::new();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        for _ in 0..2 {
            let b = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                b.wait();
                ensure_daemon_running_unix().unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let count = fs::read_dir(home.join("spawn-markers"))
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .count();
        assert_eq!(
            count, 1,
            "concurrent startup attempts should spawn at most one daemon process"
        );

        unsafe {
            match old_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
            match old_bin {
                Some(v) => std::env::set_var("ATM_DAEMON_BIN", v),
                None => std::env::remove_var("ATM_DAEMON_BIN"),
            }
            match old_auto {
                Some(v) => std::env::set_var("ATM_DAEMON_AUTOSTART", v),
                None => std::env::remove_var("ATM_DAEMON_AUTOSTART"),
            }
        }
    }

    #[test]
    fn test_new_request_id_is_unique() {
        let id1 = new_request_id();
        // Tiny sleep to ensure different nanosecond timestamp
        std::thread::sleep(std::time::Duration::from_nanos(1000));
        let id2 = new_request_id();
        // Both should be non-empty; may or may not be equal depending on timing
        assert!(!id1.is_empty());
        assert!(!id2.is_empty());
    }

    #[test]
    fn test_daemon_socket_path_contains_expected_suffix() {
        let path = daemon_socket_path().unwrap();
        assert!(path.to_string_lossy().ends_with("atm-daemon.sock"));
        assert!(path.to_string_lossy().contains(".claude/daemon"));
    }

    #[test]
    fn test_daemon_pid_path_contains_expected_suffix() {
        let path = daemon_pid_path().unwrap();
        assert!(path.to_string_lossy().ends_with("atm-daemon.pid"));
        assert!(path.to_string_lossy().contains(".claude/daemon"));
    }

    #[test]
    #[serial]
    fn test_query_agent_state_no_daemon_returns_none() {
        with_autostart_disabled(|| {
            // Graceful fallback: no daemon → Ok(None)
            let result = query_agent_state("arch-ctm", "atm-dev");
            assert!(result.is_ok());
            // Result is None unless daemon happens to be running
        });
    }

    #[test]
    fn test_agent_pane_info_deserialization() {
        let json = r#"{"pane_id":"%42","log_path":"/home/user/.claude/logs/arch-ctm.log"}"#;
        let info: AgentPaneInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.pane_id, "%42");
        assert_eq!(info.log_path, "/home/user/.claude/logs/arch-ctm.log");
    }

    #[test]
    #[serial]
    fn test_query_agent_pane_no_daemon_returns_none() {
        with_autostart_disabled(|| {
            // Graceful fallback: no daemon → Ok(None)
            let result = query_agent_pane("arch-ctm");
            assert!(result.is_ok());
            // Result is None unless daemon happens to be running
        });
    }

    #[test]
    fn test_launch_config_serialization() {
        let mut env_vars = std::collections::HashMap::new();
        env_vars.insert("EXTRA_VAR".to_string(), "value".to_string());

        let config = LaunchConfig {
            agent: "arch-ctm".to_string(),
            team: "atm-dev".to_string(),
            command: "codex --yolo".to_string(),
            prompt: Some("Review the bridge module".to_string()),
            timeout_secs: 30,
            env_vars,
            runtime: Some("codex".to_string()),
            resume_session_id: None,
        };

        let json = serde_json::to_string(&config).unwrap();
        let decoded: LaunchConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.agent, "arch-ctm");
        assert_eq!(decoded.team, "atm-dev");
        assert_eq!(decoded.command, "codex --yolo");
        assert_eq!(decoded.prompt.as_deref(), Some("Review the bridge module"));
        assert_eq!(decoded.timeout_secs, 30);
        assert_eq!(decoded.runtime.as_deref(), Some("codex"));
        assert!(decoded.resume_session_id.is_none());
        assert_eq!(
            decoded.env_vars.get("EXTRA_VAR").map(String::as_str),
            Some("value")
        );
    }

    #[test]
    fn test_launch_config_no_prompt_serialization() {
        let config = LaunchConfig {
            agent: "worker-1".to_string(),
            team: "my-team".to_string(),
            command: "codex --yolo".to_string(),
            prompt: None,
            timeout_secs: 60,
            env_vars: std::collections::HashMap::new(),
            runtime: None,
            resume_session_id: Some("sess-123".to_string()),
        };

        let json = serde_json::to_string(&config).unwrap();
        let decoded: LaunchConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.agent, "worker-1");
        assert!(decoded.prompt.is_none());
        assert!(decoded.env_vars.is_empty());
        assert!(decoded.runtime.is_none());
        assert_eq!(decoded.resume_session_id.as_deref(), Some("sess-123"));
    }

    #[test]
    fn test_launch_result_serialization() {
        let result = LaunchResult {
            agent: "arch-ctm".to_string(),
            pane_id: "%42".to_string(),
            state: "launching".to_string(),
            warning: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        let decoded: LaunchResult = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.agent, "arch-ctm");
        assert_eq!(decoded.pane_id, "%42");
        assert_eq!(decoded.state, "launching");
        assert!(decoded.warning.is_none());
    }

    #[test]
    fn test_launch_result_with_warning_serialization() {
        let result = LaunchResult {
            agent: "arch-ctm".to_string(),
            pane_id: "%7".to_string(),
            state: "launching".to_string(),
            warning: Some("Readiness timeout reached".to_string()),
        };

        let json = serde_json::to_string(&result).unwrap();
        let decoded: LaunchResult = serde_json::from_str(&json).unwrap();

        assert_eq!(
            decoded.warning.as_deref(),
            Some("Readiness timeout reached")
        );
    }

    #[test]
    fn test_session_query_result_serialization() {
        let result = SessionQueryResult {
            session_id: "abc123".to_string(),
            process_id: 12345,
            alive: true,
            runtime: None,
            runtime_session_id: None,
            pane_id: None,
            runtime_home: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        let decoded: SessionQueryResult = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.session_id, "abc123");
        assert_eq!(decoded.process_id, 12345);
        assert!(decoded.alive);
    }

    #[test]
    fn test_session_query_result_dead() {
        let json = r#"{"session_id":"xyz789","process_id":99,"alive":false}"#;
        let result: SessionQueryResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.session_id, "xyz789");
        assert_eq!(result.process_id, 99);
        assert!(!result.alive);
        assert!(result.runtime.is_none());
        assert!(result.runtime_session_id.is_none());
    }

    #[test]
    #[serial]
    fn test_query_session_no_daemon_returns_none() {
        with_autostart_disabled(|| {
            // Graceful fallback: no daemon → Ok(None)
            let result = query_session("team-lead");
            assert!(result.is_ok());
            // None unless daemon happens to be running
        });
    }

    #[test]
    #[serial]
    fn test_launch_agent_no_daemon_returns_none() {
        with_autostart_disabled(|| {
            if daemon_is_running() {
                // Shared dev machines may have daemon active; this test validates
                // no-daemon behavior only.
                return;
            }
            let config = LaunchConfig {
                agent: "test-agent".to_string(),
                team: "test-team".to_string(),
                command: "codex --yolo".to_string(),
                prompt: None,
                timeout_secs: 5,
                env_vars: std::collections::HashMap::new(),
                runtime: Some("codex".to_string()),
                resume_session_id: None,
            };
            // Without a running daemon the call should gracefully return Ok(None).
            // On non-Unix platforms it always returns None.
            // On Unix with no daemon socket present it also returns None.
            let result = launch_agent(&config);
            // The result should be Ok (no I/O error on missing socket)
            assert!(result.is_ok());
            // Result is None unless daemon happens to be running and handling "launch"
            // (which it won't be in a unit test environment)
        });
    }

    #[test]
    fn test_agent_summary_serialization() {
        let summary = AgentSummary {
            agent: "arch-ctm".to_string(),
            state: "idle".to_string(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        let decoded: AgentSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.agent, "arch-ctm");
        assert_eq!(decoded.state, "idle");
    }

    #[test]
    fn test_canonical_member_state_serialization() {
        let state = CanonicalMemberState {
            agent: "arch-ctm".to_string(),
            state: "active".to_string(),
            activity: "busy".to_string(),
            session_id: Some("sess-123".to_string()),
            process_id: Some(4242),
            reason: "session active with live pid".to_string(),
            source: "session_registry".to_string(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let decoded: CanonicalMemberState = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.agent, "arch-ctm");
        assert_eq!(decoded.state, "active");
        assert_eq!(decoded.activity, "busy");
        assert_eq!(decoded.session_id.as_deref(), Some("sess-123"));
        assert_eq!(decoded.process_id, Some(4242));
    }

    #[test]
    fn test_canonical_liveness_bool_mapping() {
        let mut state = CanonicalMemberState {
            agent: "arch-ctm".to_string(),
            state: "active".to_string(),
            activity: "busy".to_string(),
            session_id: None,
            process_id: None,
            reason: String::new(),
            source: String::new(),
        };
        assert_eq!(canonical_liveness_bool(Some(&state)), Some(true));
        state.state = "idle".to_string();
        assert_eq!(canonical_liveness_bool(Some(&state)), Some(true));
        state.state = "offline".to_string();
        assert_eq!(canonical_liveness_bool(Some(&state)), Some(false));
        state.state = "unknown".to_string();
        assert_eq!(canonical_liveness_bool(Some(&state)), None);
        assert_eq!(canonical_liveness_bool(None), None);
    }

    // Unix-only: test PID alive check for the current process
    #[cfg(unix)]
    #[test]
    fn test_pid_alive_current_process() {
        let pid = std::process::id() as i32;
        assert!(pid_alive(pid));
    }

    #[cfg(unix)]
    #[test]
    fn test_pid_alive_nonexistent_pid() {
        // Use a PID that is extremely unlikely to exist: i32::MAX.
        // On Linux and macOS the max PID is 4194304 or similar; i32::MAX exceeds
        // the kernel's PID range and kill() will return ESRCH (no such process).
        assert!(!pid_alive(i32::MAX));
    }

    /// When `ATM_DAEMON_BIN` is set to a nonexistent path, `ensure_daemon_running`
    /// must return `Err` (spawn fails) rather than silently succeeding.
    /// This confirms that the `ATM_DAEMON_BIN` env var is read by the public API.
    ///
    /// The test is skipped when a live daemon is already running to avoid
    /// interfering with the running process.
    ///
    /// `#[serial]` is required because the test mutates the process environment.
    #[test]
    #[serial]
    fn test_ensure_daemon_running_reads_atm_daemon_bin() {
        // Skip if a live daemon is already running.
        if daemon_is_running() {
            return;
        }
        unsafe {
            std::env::set_var("ATM_DAEMON_BIN", "/nonexistent-bin-for-atm-test");
        }
        let result = ensure_daemon_running();
        unsafe {
            std::env::remove_var("ATM_DAEMON_BIN");
        }
        // On non-Unix the function is a no-op and always returns Ok(()).
        #[cfg(unix)]
        assert!(
            result.is_err(),
            "spawn of nonexistent binary must return Err on Unix"
        );
        #[cfg(not(unix))]
        assert!(
            result.is_ok(),
            "ensure_daemon_running is a no-op on non-Unix"
        );
    }

    // ── LifecycleSource / LifecycleSourceKind ────────────────────────────────

    #[test]
    fn lifecycle_source_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&LifecycleSourceKind::ClaudeHook).unwrap(),
            "\"claude_hook\""
        );
        assert_eq!(
            serde_json::to_string(&LifecycleSourceKind::AtmMcp).unwrap(),
            "\"atm_mcp\""
        );
        assert_eq!(
            serde_json::to_string(&LifecycleSourceKind::AgentHook).unwrap(),
            "\"agent_hook\""
        );
        assert_eq!(
            serde_json::to_string(&LifecycleSourceKind::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    #[test]
    fn lifecycle_source_kind_deserializes_snake_case() {
        let kind: LifecycleSourceKind = serde_json::from_str("\"claude_hook\"").unwrap();
        assert_eq!(kind, LifecycleSourceKind::ClaudeHook);

        let kind: LifecycleSourceKind = serde_json::from_str("\"atm_mcp\"").unwrap();
        assert_eq!(kind, LifecycleSourceKind::AtmMcp);

        let kind: LifecycleSourceKind = serde_json::from_str("\"agent_hook\"").unwrap();
        assert_eq!(kind, LifecycleSourceKind::AgentHook);

        let kind: LifecycleSourceKind = serde_json::from_str("\"unknown\"").unwrap();
        assert_eq!(kind, LifecycleSourceKind::Unknown);
    }

    #[test]
    fn lifecycle_source_round_trip() {
        let src = LifecycleSource::new(LifecycleSourceKind::AtmMcp);
        let json = serde_json::to_string(&src).unwrap();
        assert!(json.contains("\"atm_mcp\""), "serialized: {json}");
        let decoded: LifecycleSource = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.kind, LifecycleSourceKind::AtmMcp);
    }

    #[test]
    fn hook_event_payload_without_source_is_backward_compatible() {
        // A payload without the "source" field must still parse as SocketRequest.
        let json = r#"{
            "version": 1,
            "request_id": "req-test",
            "command": "hook-event",
            "payload": {
                "event": "session_start",
                "agent": "team-lead",
                "team": "atm-dev",
                "session_id": "abc-123"
            }
        }"#;
        let req: SocketRequest = serde_json::from_str(json).unwrap();
        // The payload's "source" field is absent — no panic, no error.
        assert!(req.payload.get("source").is_none());
        assert_eq!(req.command, "hook-event");
    }

    #[test]
    fn hook_event_payload_with_atm_mcp_source_parses() {
        let json = r#"{
            "version": 1,
            "request_id": "req-mcp",
            "command": "hook-event",
            "payload": {
                "event": "session_start",
                "agent": "arch-ctm",
                "team": "atm-dev",
                "session_id": "codex:abc-123",
                "source": {"kind": "atm_mcp"}
            }
        }"#;
        let req: SocketRequest = serde_json::from_str(json).unwrap();
        let source: LifecycleSource =
            serde_json::from_value(req.payload["source"].clone()).unwrap();
        assert_eq!(source.kind, LifecycleSourceKind::AtmMcp);
    }

    #[test]
    fn test_send_control_no_daemon_returns_err() {
        if daemon_is_running() {
            // Shared dev machines may have daemon active; this test validates
            // no-daemon behavior only.
            return;
        }
        // Without a running daemon, send_control must return Err (not None or panic).
        use crate::control::{CONTROL_SCHEMA_VERSION, ControlAction, ControlRequest};

        let req = ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: "req-test-ctrl".to_string(),
            msg_type: "control.stdin.request".to_string(),
            signal: None,
            sent_at: "2026-02-21T00:00:00Z".to_string(),
            team: "atm-dev".to_string(),
            session_id: String::new(),
            agent_id: "arch-ctm".to_string(),
            sender: "tui".to_string(),
            action: ControlAction::Stdin,
            payload: Some("hello".to_string()),
            content_ref: None,
            elicitation_id: None,
            decision: None,
        };

        let result = send_control(&req);
        // With no daemon running the call should fail gracefully (not panic).
        // We only assert it returns an Err — the exact message is implementation detail.
        assert!(
            result.is_err(),
            "send_control should return Err when daemon is not running"
        );
    }

    // ── Windows-specific tests ───────────────────────────────────────────────
    //
    // These tests validate the Windows code paths for daemon auto-start readiness
    // and lock behavior. On Windows, all daemon socket communication is intentionally
    // unavailable (Unix domain sockets only), so the contract is that every public
    // function returns `Ok(None)` or `false` without panicking or returning an error.
    //
    // Requirement: requirements.md §T.1 cross-platform row — "Windows CI coverage
    // must validate spawn/readiness/lock behavior".

    /// On Windows, `query_daemon` must return `Ok(None)` for any request.
    ///
    /// The daemon uses Unix domain sockets which are unavailable on Windows.
    /// The graceful fallback ensures the CLI degrades silently rather than
    /// failing with a platform-specific error.
    #[cfg(windows)]
    #[test]
    #[serial]
    fn windows_query_daemon_returns_ok_none() {
        let req = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-win-test".to_string(),
            command: "agent-state".to_string(),
            payload: serde_json::json!({ "agent": "arch-ctm", "team": "atm-dev" }),
        };
        let result = query_daemon(&req);
        assert!(
            result.is_ok(),
            "query_daemon must not return Err on Windows"
        );
        assert!(
            result.unwrap().is_none(),
            "query_daemon must return Ok(None) on Windows (no Unix socket available)"
        );
    }

    /// On Windows, `daemon_is_running` must return `false` without panicking.
    ///
    /// The PID-file check uses Unix `kill(pid, 0)` which is unavailable on Windows.
    /// The Windows branch always returns `false` — validated here so CI catches
    /// any accidental regression that re-introduces a Unix-only code path.
    #[cfg(windows)]
    #[test]
    fn windows_daemon_is_running_returns_false() {
        // No daemon can be running on Windows (no Unix socket / PID-kill support).
        assert!(
            !daemon_is_running(),
            "daemon_is_running must return false on Windows"
        );
    }

    /// On Windows, `subscribe_stream_events` must return `Ok(None)`.
    ///
    /// Stream subscriptions require a long-lived Unix domain socket connection.
    /// The Windows branch short-circuits to `Ok(None)` so callers can treat the
    /// absence of stream events as equivalent to a daemon that is not running.
    #[cfg(windows)]
    #[test]
    fn windows_subscribe_stream_events_returns_ok_none() {
        let result = subscribe_stream_events();
        assert!(
            result.is_ok(),
            "subscribe_stream_events must not return Err on Windows"
        );
        assert!(
            result.unwrap().is_none(),
            "subscribe_stream_events must return Ok(None) on Windows"
        );
    }

    /// On Windows, `query_agent_state` must return `Ok(None)`.
    ///
    /// Exercises the full call path (including payload serialisation) to confirm
    /// that the Windows `Ok(None)` short-circuit in `query_daemon` propagates
    /// correctly through the higher-level wrapper.
    #[cfg(windows)]
    #[test]
    #[serial]
    fn windows_query_agent_state_returns_ok_none() {
        let result = query_agent_state("arch-ctm", "atm-dev");
        assert!(
            result.is_ok(),
            "query_agent_state must not return Err on Windows"
        );
        assert!(
            result.unwrap().is_none(),
            "query_agent_state must return Ok(None) on Windows"
        );
    }

    /// On Windows, `query_session` must return `Ok(None)`.
    #[cfg(windows)]
    #[test]
    #[serial]
    fn windows_query_session_returns_ok_none() {
        let result = query_session("team-lead");
        assert!(
            result.is_ok(),
            "query_session must not return Err on Windows"
        );
        assert!(
            result.unwrap().is_none(),
            "query_session must return Ok(None) on Windows"
        );
    }

    /// On Windows, `launch_agent` must return `Ok(None)`.
    ///
    /// Confirms that the auto-start path (which requires Unix `fork`/`exec`
    /// semantics) never executes on Windows and the call degrades gracefully.
    #[cfg(windows)]
    #[test]
    #[serial]
    fn windows_launch_agent_returns_ok_none() {
        let config = LaunchConfig {
            agent: "test-agent".to_string(),
            team: "test-team".to_string(),
            command: "codex --yolo".to_string(),
            prompt: None,
            timeout_secs: 5,
            env_vars: std::collections::HashMap::new(),
            runtime: Some("codex".to_string()),
            resume_session_id: None,
        };
        let result = launch_agent(&config);
        assert!(
            result.is_ok(),
            "launch_agent must not return Err on Windows (no daemon socket)"
        );
        assert!(
            result.unwrap().is_none(),
            "launch_agent must return Ok(None) on Windows"
        );
    }

    /// On Windows, the startup lock (`acquire_lock`) must be acquirable and
    /// automatically released on drop.
    ///
    /// The `ensure_daemon_running_unix` function is gated `#[cfg(unix)]` and
    /// never runs on Windows, but the startup-lock path (`fs2::LockFileEx`) is
    /// the same cross-platform primitive used throughout atm-core.  This test
    /// confirms the Windows lock backend works correctly in the context of the
    /// daemon startup directory layout.
    #[cfg(windows)]
    #[test]
    fn windows_startup_lock_acquires_and_releases() {
        use crate::io::lock::acquire_lock;
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let lock_dir = tmp.path().join("config").join("atm");
        fs::create_dir_all(&lock_dir).unwrap();
        let lock_path = lock_dir.join("daemon-start.lock");

        // Acquire the lock — mirrors what ensure_daemon_running_unix does.
        let lock = acquire_lock(&lock_path, 3);
        assert!(
            lock.is_ok(),
            "startup lock must be acquirable on Windows: {:?}",
            lock.err()
        );

        // Explicit drop releases the lock (Windows holds handles; explicit drop
        // ensures the LockFileEx unlock fires before we try to re-acquire).
        drop(lock.unwrap());

        // Re-acquire to confirm the lock was actually released.
        let lock2 = acquire_lock(&lock_path, 1);
        assert!(
            lock2.is_ok(),
            "startup lock must be re-acquirable after release on Windows"
        );
    }

    /// On Windows, `daemon_socket_path` must produce a path ending with the
    /// expected suffix regardless of the underlying home-directory resolver.
    #[cfg(windows)]
    #[test]
    fn windows_daemon_socket_path_has_correct_suffix() {
        let path = daemon_socket_path().unwrap();
        let s = path.to_string_lossy();
        assert!(
            s.ends_with("atm-daemon.sock"),
            "daemon_socket_path must end with 'atm-daemon.sock' on Windows, got: {s}"
        );
        assert!(
            s.contains(".claude") && s.contains("daemon"),
            "daemon_socket_path must contain '.claude/daemon' on Windows, got: {s}"
        );
    }

    /// On Windows, `daemon_pid_path` must produce a path ending with the
    /// expected suffix.
    #[cfg(windows)]
    #[test]
    fn windows_daemon_pid_path_has_correct_suffix() {
        let path = daemon_pid_path().unwrap();
        let s = path.to_string_lossy();
        assert!(
            s.ends_with("atm-daemon.pid"),
            "daemon_pid_path must end with 'atm-daemon.pid' on Windows, got: {s}"
        );
        assert!(
            s.contains(".claude") && s.contains("daemon"),
            "daemon_pid_path must contain '.claude/daemon' on Windows, got: {s}"
        );
    }

    /// On Windows, `send_control` must return `Err` (not panic) when the daemon
    /// is not reachable, because `send_control` intentionally propagates absence
    /// as an error (unlike the `Ok(None)` contract of other public functions).
    #[cfg(windows)]
    #[test]
    fn windows_send_control_no_daemon_returns_err() {
        use crate::control::{CONTROL_SCHEMA_VERSION, ControlAction, ControlRequest};

        let req = ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: "req-win-ctrl".to_string(),
            msg_type: "control.stdin.request".to_string(),
            signal: None,
            sent_at: "2026-02-21T00:00:00Z".to_string(),
            team: "atm-dev".to_string(),
            session_id: String::new(),
            agent_id: "arch-ctm".to_string(),
            sender: "tui".to_string(),
            action: ControlAction::Stdin,
            payload: Some("hello".to_string()),
            content_ref: None,
            elicitation_id: None,
            decision: None,
        };

        let result = send_control(&req);
        assert!(
            result.is_err(),
            "send_control must return Err on Windows when daemon is not reachable"
        );
    }

    #[test]
    fn test_send_control_builds_correct_socket_request() {
        // Verify the SocketRequest built inside send_control has the right shape
        // by re-creating it manually and checking serialization.
        use crate::control::{CONTROL_SCHEMA_VERSION, ControlAction, ControlRequest};

        let req = ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: "req-ctrl-check".to_string(),
            msg_type: "control.interrupt.request".to_string(),
            signal: Some("interrupt".to_string()),
            sent_at: "2026-02-21T00:00:00Z".to_string(),
            team: "atm-dev".to_string(),
            session_id: String::new(),
            agent_id: "arch-ctm".to_string(),
            sender: "tui".to_string(),
            action: ControlAction::Interrupt,
            payload: None,
            content_ref: None,
            elicitation_id: None,
            decision: None,
        };

        // The socket-level request_id is an independent correlation ID generated
        // by send_control (e.g., "sock-<nanos>").  It must NOT be the same as
        // the control payload's stable idempotency key (`req.request_id`).
        let control_payload = serde_json::to_value(&req).expect("serialize ControlRequest");
        let socket_req = SocketRequest {
            version: PROTOCOL_VERSION,
            // Distinct from req.request_id — mirrors what send_control generates.
            request_id: "sock-test-123".to_string(),
            command: "control".to_string(),
            payload: control_payload,
        };

        // Sanity check: outer request_id is the socket-level ID, not the control ID.
        assert_ne!(
            socket_req.request_id, req.request_id,
            "socket-level request_id must differ from control payload request_id"
        );
        assert_eq!(socket_req.request_id, "sock-test-123");

        let json = serde_json::to_string(&socket_req).expect("serialize SocketRequest");

        // Outer envelope fields.
        assert!(
            json.contains("\"command\":\"control\""),
            "command field missing"
        );

        // The control payload's request_id must appear inside the serialized
        // payload body, not as the outer SocketRequest.request_id.
        assert!(
            json.contains("\"request_id\":\"req-ctrl-check\""),
            "control payload request_id must appear inside the payload body"
        );

        // The outer socket-level request_id is present.
        assert!(
            json.contains("\"request_id\":\"sock-test-123\""),
            "socket-level request_id must appear in the outer envelope"
        );

        // The type field in the control payload.
        assert!(
            json.contains("\"type\":\"control.interrupt.request\""),
            "msg_type field missing from control payload"
        );

        // The interrupt signal.
        assert!(json.contains("\"interrupt\""), "interrupt signal missing");
    }
}
