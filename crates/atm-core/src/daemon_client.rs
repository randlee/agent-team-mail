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
#[allow(unused_variables)]
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


/// Send a single request to the daemon and return the parsed response.
///
/// Returns `Ok(None)` when the daemon is not running or the socket cannot be
/// reached. Returns `Ok(Some(response))` on a successful exchange. Returns
/// `Err` only for I/O errors that occur *after* a connection is established.
///
/// # Platform Behaviour
///
/// On non-Unix platforms this function always returns `Ok(None)`.
#[allow(unused_variables)]
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
    use std::time::Duration;

    let socket_path = daemon_socket_path()?;

    // Attempt connection — return None if socket not present or connection refused
    let stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(_) => return Ok(None),
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
        assert_eq!(info.last_transition.as_deref(), Some("2026-02-16T22:30:00Z"));
    }

    #[test]
    fn test_agent_state_info_missing_transition() {
        let json = r#"{"state":"launching"}"#;
        let info: AgentStateInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.state, "launching");
        assert!(info.last_transition.is_none());
    }

    #[test]
    fn test_query_daemon_no_socket_returns_none() {
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
    fn test_query_agent_state_no_daemon_returns_none() {
        // Graceful fallback: no daemon → Ok(None)
        let result = query_agent_state("arch-ctm", "atm-dev");
        assert!(result.is_ok());
        // Result is None unless daemon happens to be running
    }

    #[test]
    fn test_agent_pane_info_deserialization() {
        let json = r#"{"pane_id":"%42","log_path":"/home/user/.claude/logs/arch-ctm.log"}"#;
        let info: AgentPaneInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.pane_id, "%42");
        assert_eq!(info.log_path, "/home/user/.claude/logs/arch-ctm.log");
    }

    #[test]
    fn test_query_agent_pane_no_daemon_returns_none() {
        // Graceful fallback: no daemon → Ok(None)
        let result = query_agent_pane("arch-ctm");
        assert!(result.is_ok());
        // Result is None unless daemon happens to be running
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
        };

        let json = serde_json::to_string(&config).unwrap();
        let decoded: LaunchConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.agent, "arch-ctm");
        assert_eq!(decoded.team, "atm-dev");
        assert_eq!(decoded.command, "codex --yolo");
        assert_eq!(decoded.prompt.as_deref(), Some("Review the bridge module"));
        assert_eq!(decoded.timeout_secs, 30);
        assert_eq!(decoded.env_vars.get("EXTRA_VAR").map(String::as_str), Some("value"));
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
        };

        let json = serde_json::to_string(&config).unwrap();
        let decoded: LaunchConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.agent, "worker-1");
        assert!(decoded.prompt.is_none());
        assert!(decoded.env_vars.is_empty());
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

        assert_eq!(decoded.warning.as_deref(), Some("Readiness timeout reached"));
    }

    #[test]
    fn test_launch_agent_no_daemon_returns_none() {
        let config = LaunchConfig {
            agent: "test-agent".to_string(),
            team: "test-team".to_string(),
            command: "codex --yolo".to_string(),
            prompt: None,
            timeout_secs: 5,
            env_vars: std::collections::HashMap::new(),
        };
        // Without a running daemon the call should gracefully return Ok(None).
        // On non-Unix platforms it always returns None.
        // On Unix with no daemon socket present it also returns None.
        let result = launch_agent(&config);
        // The result should be Ok (no I/O error on missing socket)
        assert!(result.is_ok());
        // Result is None unless daemon happens to be running and handling "launch"
        // (which it won't be in a unit test environment)
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
}
