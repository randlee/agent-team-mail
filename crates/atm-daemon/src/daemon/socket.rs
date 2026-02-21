//! Unix socket server for CLI↔daemon IPC.
//!
//! The daemon listens on a Unix domain socket at:
//!
//! ```text
//! ${ATM_HOME}/.claude/daemon/atm-daemon.sock
//! ```
//!
//! Each client connection follows a simple request/response protocol:
//!
//! 1. Client connects
//! 2. Client writes one JSON line (newline-terminated request)
//! 3. Server writes one JSON line (newline-terminated response)
//! 4. Server closes the connection
//!
//! See [`agent_team_mail_core::daemon_client`] for the corresponding client
//! implementation and protocol type definitions.
//!
//! ## Platform availability
//!
//! The socket server is only compiled and active on Unix platforms.
//! On non-Unix platforms the module exposes stub functions that do nothing.

use agent_team_mail_core::control::{
    CONTROL_SCHEMA_VERSION, ContentRef, ControlAck, ControlAction, ControlRequest, ControlResult,
};
use agent_team_mail_core::daemon_client::{LaunchConfig, LaunchResult};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::text::DEFAULT_MAX_MESSAGE_BYTES;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::OnceLock;
use tracing::{debug, error, info, warn};

use crate::daemon::dedup::{DedupeKey, DedupeStore};
use crate::daemon::session_registry::SharedSessionRegistry;
use crate::plugins::worker_adapter::AgentState;

// ── Public API (cross-platform stubs) ────────────────────────────────────────

/// Start the Unix socket server and return a handle that cleans up the socket
/// on drop.
///
/// # Arguments
///
/// * `home_dir` - ATM home directory used to locate the socket path
/// * `state_store` - Shared access to the agent state tracker
/// * `pubsub_store` - Shared pub/sub registry for subscribe/unsubscribe requests
/// * `launch_tx` - Shared sender for forwarding `"launch"` commands to the
///   [`WorkerAdapterPlugin`](crate::plugins::worker_adapter::WorkerAdapterPlugin).
///   Pass [`new_launch_sender()`] (with an empty inner `Option`) when the
///   worker adapter is disabled; the socket server will return a
///   `LAUNCH_UNAVAILABLE` error for any `"launch"` requests.
/// * `session_registry` - Shared session registry for `session-query` requests
/// * `cancel` - Cancellation token; server stops accepting when cancelled
///
/// # Platform Behaviour
///
/// On non-Unix platforms this function returns `Ok(None)` immediately.
pub async fn start_socket_server(
    home_dir: PathBuf,
    state_store: SharedStateStore,
    pubsub_store: SharedPubSubStore,
    launch_tx: LaunchSender,
    session_registry: SharedSessionRegistry,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<Option<SocketServerHandle>> {
    #[cfg(unix)]
    {
        start_unix_socket_server(
            home_dir,
            state_store,
            pubsub_store,
            launch_tx,
            session_registry,
            cancel,
        )
        .await
        .map(Some)
    }

    #[cfg(not(unix))]
    {
        info!("Unix socket server not available on this platform");
        Ok(None)
    }
}

/// A handle to the running socket server.
///
/// Dropping this handle removes the socket file from disk.
pub struct SocketServerHandle {
    /// Path to the socket file (removed on drop)
    socket_path: PathBuf,
    /// Path to the PID file (removed on drop)
    pid_path: PathBuf,
}

impl Drop for SocketServerHandle {
    fn drop(&mut self) {
        cleanup_socket_files(&self.socket_path, &self.pid_path);
    }
}

fn cleanup_socket_files(socket_path: &PathBuf, pid_path: &PathBuf) {
    if socket_path.exists() {
        if let Err(e) = std::fs::remove_file(socket_path) {
            warn!(
                "Failed to remove socket file {}: {e}",
                socket_path.display()
            );
        } else {
            debug!("Removed socket file {}", socket_path.display());
        }
    }
    if pid_path.exists() {
        if let Err(e) = std::fs::remove_file(pid_path) {
            warn!("Failed to remove PID file {}: {e}", pid_path.display());
        } else {
            debug!("Removed PID file {}", pid_path.display());
        }
    }
}

// ── Shared state ──────────────────────────────────────────────────────────────

/// Shared agent state store accessible from socket request handlers.
///
/// Wraps an `Arc<Mutex<AgentStateTracker>>` from the worker adapter plugin.
/// When the worker adapter plugin is not enabled, this is an empty tracker.
pub type SharedStateStore =
    std::sync::Arc<std::sync::Mutex<crate::plugins::worker_adapter::AgentStateTracker>>;

/// Shared pub/sub registry accessible from socket request handlers.
///
/// Wraps an `Arc<Mutex<PubSub>>` from the worker adapter plugin. When the
/// worker adapter plugin is not enabled, this is an empty registry.
pub type SharedPubSubStore =
    std::sync::Arc<std::sync::Mutex<crate::plugins::worker_adapter::PubSub>>;

/// Create a new empty shared state store.
pub fn new_state_store() -> SharedStateStore {
    use crate::plugins::worker_adapter::AgentStateTracker;
    std::sync::Arc::new(std::sync::Mutex::new(AgentStateTracker::new()))
}

/// Create a new empty shared pub/sub store.
pub fn new_pubsub_store() -> SharedPubSubStore {
    use crate::plugins::worker_adapter::PubSub;
    std::sync::Arc::new(std::sync::Mutex::new(PubSub::new()))
}

fn control_dedupe_store() -> &'static std::sync::Mutex<DedupeStore> {
    static STORE: OnceLock<std::sync::Mutex<DedupeStore>> = OnceLock::new();
    STORE.get_or_init(|| std::sync::Mutex::new(DedupeStore::from_env()))
}

// ── Launch channel types ──────────────────────────────────────────────────────

/// A request to launch a new agent, sent from the socket handler to the
/// [`WorkerAdapterPlugin`](crate::plugins::worker_adapter::WorkerAdapterPlugin)
/// via an mpsc channel.
pub struct LaunchRequest {
    /// Launch configuration received from the CLI.
    pub config: LaunchConfig,
    /// One-shot channel for the plugin to send the launch result back.
    pub response_tx: tokio::sync::oneshot::Sender<Result<LaunchResult, String>>,
}

/// Shared sender end of the launch channel.
///
/// The socket server holds this handle.  When it receives a `"launch"` command,
/// it acquires the lock, clones the inner `Sender`, and forwards a
/// [`LaunchRequest`] to the `WorkerAdapterPlugin` run loop.
///
/// The `Option` is `None` when the worker adapter plugin is not enabled (i.e.,
/// no one is listening on the receiver end).
pub type LaunchSender =
    std::sync::Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::Sender<LaunchRequest>>>>;

/// Create a new, empty [`LaunchSender`] (no receiver connected yet).
pub fn new_launch_sender() -> LaunchSender {
    std::sync::Arc::new(tokio::sync::Mutex::new(None))
}

// ── Unix implementation ───────────────────────────────────────────────────────

#[cfg(unix)]
async fn start_unix_socket_server(
    home_dir: PathBuf,
    state_store: SharedStateStore,
    pubsub_store: SharedPubSubStore,
    launch_tx: LaunchSender,
    session_registry: SharedSessionRegistry,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<SocketServerHandle> {
    use tokio::net::UnixListener;

    let daemon_dir = home_dir.join(".claude/daemon");
    let socket_path = daemon_dir.join("atm-daemon.sock");
    let pid_path = daemon_dir.join("atm-daemon.pid");

    // Ensure the daemon directory exists
    std::fs::create_dir_all(&daemon_dir)?;

    // Remove stale socket file if present (daemon may have crashed previously)
    if socket_path.exists() {
        warn!("Removing stale socket file: {}", socket_path.display());
        std::fs::remove_file(&socket_path)?;
    }

    // Write PID file
    let pid = std::process::id();
    std::fs::write(&pid_path, format!("{pid}\n"))?;
    debug!("Wrote PID {pid} to {}", pid_path.display());

    // Bind the Unix listener
    let listener = UnixListener::bind(&socket_path)?;
    info!("Unix socket server listening on {}", socket_path.display());

    // Spawn the accept loop
    let accept_socket_path = socket_path.clone();
    let accept_pid_path = pid_path.clone();
    tokio::spawn(async move {
        run_accept_loop(
            listener,
            state_store,
            pubsub_store,
            launch_tx,
            session_registry,
            cancel,
            &accept_socket_path,
            &accept_pid_path,
        )
        .await;
    });

    Ok(SocketServerHandle {
        socket_path,
        pid_path,
    })
}

#[cfg(unix)]
#[expect(
    clippy::too_many_arguments,
    reason = "accept loop requires shared daemon resources and paths passed from startup"
)]
async fn run_accept_loop(
    listener: tokio::net::UnixListener,
    state_store: SharedStateStore,
    pubsub_store: SharedPubSubStore,
    launch_tx: LaunchSender,
    session_registry: SharedSessionRegistry,
    cancel: tokio_util::sync::CancellationToken,
    socket_path: &std::path::Path,
    _pid_path: &std::path::Path,
) {
    info!("Socket accept loop started");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("Socket server cancelled");
                break;
            }
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        let store = state_store.clone();
                        let ps = pubsub_store.clone();
                        let tx = launch_tx.clone();
                        let sr = session_registry.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, store, ps, tx, sr).await {
                                error!("Socket connection handler error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        error!("Accept error on socket {}: {e}", socket_path.display());
                        // Brief pause before retrying to avoid a tight error loop
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        }
    }

    info!("Socket accept loop stopped");
}

#[cfg(unix)]
async fn handle_connection(
    stream: tokio::net::UnixStream,
    state_store: SharedStateStore,
    pubsub_store: SharedPubSubStore,
    launch_tx: LaunchSender,
    session_registry: SharedSessionRegistry,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    debug!("New socket connection");

    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();

    // Read one request line
    match reader.read_line(&mut request_line).await {
        Ok(0) => {
            debug!("Client disconnected without sending request");
            return Ok(());
        }
        Err(e) => {
            warn!("Failed to read socket request: {e}");
            return Ok(());
        }
        Ok(_) => {}
    }

    let request_str = request_line.trim();

    // Check whether this is a launch command before sync dispatch so we can
    // use async channel communication with the WorkerAdapterPlugin.
    let response = if is_launch_command(request_str) {
        handle_launch_command(request_str, &launch_tx).await
    } else if is_control_command(request_str) {
        handle_control_command(request_str, &state_store, &session_registry).await
    } else if is_hook_event_command(request_str) {
        handle_hook_event_command(request_str, &state_store, &session_registry).await
    } else {
        match parse_and_dispatch(request_str, &state_store, &pubsub_store, &session_registry) {
            Ok(resp) => resp,
            Err(e) => {
                error!("Failed to dispatch socket request: {e}");
                make_error_response(
                    "unknown",
                    "INTERNAL_ERROR",
                    &format!("Internal server error: {e}"),
                )
            }
        }
    };

    // Write response line
    let mut response_json = serde_json::to_string(&response)?;
    response_json.push('\n');

    // Recover the stream from the BufReader to write the response
    let mut stream = reader.into_inner();
    stream.write_all(response_json.as_bytes()).await?;
    stream.flush().await?;

    debug!(
        "Socket response sent for request_id={}",
        response.request_id
    );
    Ok(())
}

/// Quickly determine if a raw JSON line is a `"launch"` command without full
/// parsing — used to decide whether to take the async launch path.
#[cfg(unix)]
fn is_launch_command(request_str: &str) -> bool {
    // Fast path: only parse the "command" field.  A full parse happens inside
    // handle_launch_command.
    request_str.contains(r#""command":"launch""#) || request_str.contains(r#""command": "launch""#)
}

/// Quickly determine if a raw JSON line is a `"control"` command.
#[cfg(unix)]
fn is_control_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"control""#)
        || request_str.contains(r#""command": "control""#)
}

/// Quickly determine if a raw JSON line is a `"hook-event"` command.
#[cfg(unix)]
fn is_hook_event_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"hook-event""#)
        || request_str.contains(r#""command": "hook-event""#)
}

/// Handle the `"hook-event"` command, updating daemon state in real-time
/// from Claude Code lifecycle hooks (session_start, teammate_idle, session_end).
#[cfg(unix)]
async fn handle_hook_event_command(
    request_str: &str,
    state_store: &SharedStateStore,
    session_registry: &SharedSessionRegistry,
) -> SocketResponse {
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};

    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("bad hook-event: {e}"),
            )
        }
    };

    if request.version != PROTOCOL_VERSION {
        return make_error_response(
            &request.request_id,
            "VERSION_MISMATCH",
            "unsupported version",
        );
    }

    let event_type = request
        .payload
        .get("event")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let agent = request
        .payload
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let session_id = request
        .payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let process_id = request
        .payload
        .get("process_id")
        .and_then(|v| v.as_u64())
        .map(|p| p as u32);

    if agent.is_empty() {
        return make_ok_response(
            &request.request_id,
            serde_json::json!({"processed": false, "reason": "missing agent"}),
        );
    }

    match event_type.as_str() {
        "session_start" => {
            if !session_id.is_empty() {
                let pid = process_id.unwrap_or(0);
                session_registry
                    .lock()
                    .unwrap()
                    .upsert(&agent, &session_id, pid);
            }
            {
                let mut tracker = state_store.lock().unwrap();
                if tracker.get_state(&agent).is_none() {
                    tracker.register_agent(&agent);
                }
            }
            info!("hook_event session_start: agent={agent} session_id={session_id}");
        }
        "teammate_idle" => {
            {
                let mut tracker = state_store.lock().unwrap();
                if tracker.get_state(&agent).is_some() {
                    tracker.set_state(&agent, AgentState::Idle);
                } else {
                    tracker.register_agent(&agent);
                    tracker.set_state(&agent, AgentState::Idle);
                }
            }
            info!("hook_event teammate_idle: agent={agent}");
        }
        "session_end" => {
            {
                session_registry.lock().unwrap().mark_dead(&agent);
            }
            {
                let mut tracker = state_store.lock().unwrap();
                if tracker.get_state(&agent).is_some() {
                    tracker.set_state(&agent, AgentState::Killed);
                }
            }
            info!("hook_event session_end: agent={agent}");
        }
        other => {
            debug!("hook_event unknown event type: {other}");
            return make_ok_response(
                &request.request_id,
                serde_json::json!({"processed": false, "reason": format!("unknown event type: {other}")}),
            );
        }
    }

    make_ok_response(
        &request.request_id,
        serde_json::json!({"processed": true, "event": event_type, "agent": agent}),
    )
}

/// Handle the `"launch"` command asynchronously by forwarding it through the
/// [`LaunchSender`] channel to the [`WorkerAdapterPlugin`].
///
/// Times out after 35 seconds so a stalled plugin does not block the
/// connection indefinitely.
#[cfg(unix)]
async fn handle_launch_command(request_str: &str, launch_tx: &LaunchSender) -> SocketResponse {
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};

    // Parse the full request envelope
    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            warn!("Malformed launch request: {e}");
            return make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse launch request: {e}"),
            );
        }
    };

    if request.version != PROTOCOL_VERSION {
        return make_error_response(
            &request.request_id,
            "VERSION_MISMATCH",
            &format!(
                "Unsupported protocol version {}; server supports {}",
                request.version, PROTOCOL_VERSION
            ),
        );
    }

    // Deserialize LaunchConfig from the payload
    let launch_config: LaunchConfig = match serde_json::from_value(request.payload.clone()) {
        Ok(cfg) => cfg,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                "INVALID_PAYLOAD",
                &format!("Failed to parse launch payload: {e}"),
            );
        }
    };

    // Validate required fields
    if launch_config.agent.trim().is_empty() {
        return make_error_response(
            &request.request_id,
            "MISSING_PARAMETER",
            "Missing required payload field: 'agent'",
        );
    }
    if launch_config.team.trim().is_empty() {
        return make_error_response(
            &request.request_id,
            "MISSING_PARAMETER",
            "Missing required payload field: 'team'",
        );
    }

    // Acquire the launch sender
    let maybe_sender = {
        let guard = launch_tx.lock().await;
        guard.clone()
    };

    let sender = match maybe_sender {
        Some(s) => s,
        None => {
            return make_error_response(
                &request.request_id,
                "LAUNCH_UNAVAILABLE",
                "Agent launch is not available (worker adapter plugin not enabled)",
            );
        }
    };

    // Create a oneshot channel for the response
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();

    let launch_req = LaunchRequest {
        config: launch_config,
        response_tx,
    };

    // Send the launch request to the plugin
    if sender.send(launch_req).await.is_err() {
        return make_error_response(
            &request.request_id,
            "LAUNCH_UNAVAILABLE",
            "Launch channel closed (worker adapter plugin may have stopped)",
        );
    }

    // Wait for the plugin to respond (with timeout)
    let timeout = std::time::Duration::from_secs(35);
    match tokio::time::timeout(timeout, response_rx).await {
        Ok(Ok(Ok(result))) => {
            debug!(
                "Launch succeeded for agent {} (pane {})",
                result.agent, result.pane_id
            );
            make_ok_response(
                &request.request_id,
                serde_json::to_value(&result).unwrap_or_default(),
            )
        }
        Ok(Ok(Err(err_msg))) => make_error_response(&request.request_id, "LAUNCH_FAILED", &err_msg),
        Ok(Err(_)) => make_error_response(
            &request.request_id,
            "LAUNCH_FAILED",
            "Launch response channel dropped unexpectedly",
        ),
        Err(_) => make_error_response(
            &request.request_id,
            "LAUNCH_TIMEOUT",
            "Agent did not become ready within the timeout period",
        ),
    }
}

/// Handle the `"control"` command asynchronously.
#[cfg(unix)]
async fn handle_control_command(
    request_str: &str,
    state_store: &SharedStateStore,
    session_registry: &SharedSessionRegistry,
) -> SocketResponse {
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};

    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse control request: {e}"),
            );
        }
    };

    if request.version != PROTOCOL_VERSION {
        return make_error_response(
            &request.request_id,
            "VERSION_MISMATCH",
            &format!(
                "Unsupported protocol version {}; server supports {}",
                request.version, PROTOCOL_VERSION
            ),
        );
    }

    let control: ControlRequest = match serde_json::from_value(request.payload.clone()) {
        Ok(c) => c,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                "INVALID_PAYLOAD",
                &format!("Failed to parse control payload: {e}"),
            );
        }
    };

    let ack = process_control_request(control, state_store, session_registry).await;
    make_ok_response(
        &request.request_id,
        serde_json::to_value(ack).unwrap_or_else(|_| serde_json::json!({})),
    )
}

#[cfg(unix)]
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(unix)]
fn control_ack(
    request_id: &str,
    result: ControlResult,
    duplicate: bool,
    detail: Option<String>,
) -> ControlAck {
    ControlAck {
        request_id: request_id.to_string(),
        result,
        duplicate,
        detail,
        acked_at: now_rfc3339(),
    }
}

#[cfg(unix)]
fn control_action_name(action: &ControlAction) -> &'static str {
    match action {
        ControlAction::Stdin => "control_stdin",
        ControlAction::Interrupt => "control_interrupt",
    }
}

#[cfg(unix)]
fn emit_control_request_event(control: &ControlRequest) {
    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm-daemon",
        action: "control_request",
        team: Some(control.team.clone()),
        session_id: Some(control.session_id.clone()),
        agent_id: Some(control.agent_id.clone()),
        agent_name: Some(control.sender.clone()),
        request_id: Some(control.request_id.clone()),
        target: Some(control_action_name(&control.action).to_string()),
        ..Default::default()
    });
}

#[cfg(unix)]
fn emit_control_ack_event(control: &ControlRequest, ack: &ControlAck) {
    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm-daemon",
        action: "control_ack",
        team: Some(control.team.clone()),
        session_id: Some(control.session_id.clone()),
        agent_id: Some(control.agent_id.clone()),
        agent_name: Some(control.sender.clone()),
        request_id: Some(control.request_id.clone()),
        target: Some(control_action_name(&control.action).to_string()),
        result: Some(format!("{:?}", ack.result).to_ascii_lowercase()),
        message_text: Some(format!("duplicate={}", ack.duplicate)),
        ..Default::default()
    });
}

#[cfg(unix)]
fn control_request_is_live(
    control: &ControlRequest,
    state_store: &SharedStateStore,
    session_registry: &SharedSessionRegistry,
) -> ControlResult {
    let registry = session_registry.lock().unwrap();
    let Some(record) = registry.query(&control.agent_id) else {
        return ControlResult::NotFound;
    };
    if record.session_id != control.session_id {
        return ControlResult::NotFound;
    }
    if record.state != crate::daemon::session_registry::SessionState::Active
        || !record.is_process_alive()
    {
        return ControlResult::NotLive;
    }

    let tracker = state_store.lock().unwrap();
    match tracker.get_state(&control.agent_id) {
        Some(AgentState::Idle) | Some(AgentState::Busy) => ControlResult::Ok,
        Some(AgentState::Launching) | Some(AgentState::Killed) | None => ControlResult::NotLive,
    }
}

#[cfg(unix)]
fn validate_control_request(control: &ControlRequest) -> Option<String> {
    if control.v != CONTROL_SCHEMA_VERSION {
        return Some(format!(
            "unsupported control schema version {}; expected {}",
            control.v, CONTROL_SCHEMA_VERSION
        ));
    }
    if control.request_id.trim().is_empty()
        || control.team.trim().is_empty()
        || control.session_id.trim().is_empty()
        || control.agent_id.trim().is_empty()
        || control.sender.trim().is_empty()
    {
        return Some("missing required control fields".to_string());
    }
    let parsed = match chrono::DateTime::parse_from_rfc3339(&control.sent_at) {
        Ok(t) => t,
        Err(_) => return Some("sent_at must be RFC3339".to_string()),
    };
    let max_skew_secs = std::env::var("ATM_CONTROL_MAX_SKEW_SECS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(300);
    let skew = (chrono::Utc::now() - parsed.with_timezone(&chrono::Utc)).num_seconds();
    if skew.unsigned_abs() > max_skew_secs as u64 {
        return Some(format!("sent_at skew exceeds {max_skew_secs}s"));
    }

    let inline_len = control.payload.as_ref().map(|s| s.len()).unwrap_or(0);
    if inline_len > DEFAULT_MAX_MESSAGE_BYTES {
        return Some(format!(
            "inline payload exceeds {} bytes",
            DEFAULT_MAX_MESSAGE_BYTES
        ));
    }
    None
}

#[cfg(unix)]
fn read_content_ref_text(content_ref: &ContentRef) -> Result<String, String> {
    let home = agent_team_mail_core::home::get_home_dir().map_err(|e| e.to_string())?;
    let allowed = home.join(".config/atm/share");
    std::fs::create_dir_all(&allowed).map_err(|e| {
        format!(
            "failed to prepare allowed content_ref base {}: {e}",
            allowed.display()
        )
    })?;
    let canonical_allowed = std::fs::canonicalize(&allowed).map_err(|e| {
        format!(
            "failed to resolve allowed content_ref base {}: {e}",
            allowed.display()
        )
    })?;
    let canonical_path = std::fs::canonicalize(&content_ref.path)
        .map_err(|e| format!("content_ref path not readable: {e}"))?;
    if !canonical_path.starts_with(&canonical_allowed) {
        return Err("content_ref path escapes allowed base".to_string());
    }

    if let Some(ref expires_at) = content_ref.expires_at {
        let exp = chrono::DateTime::parse_from_rfc3339(expires_at)
            .map_err(|_| "content_ref expires_at must be RFC3339".to_string())?;
        if chrono::Utc::now() > exp.with_timezone(&chrono::Utc) {
            return Err("content_ref has expired".to_string());
        }
    }

    let bytes = std::fs::read(&canonical_path).map_err(|e| e.to_string())?;
    if bytes.len() as u64 != content_ref.size_bytes {
        return Err("content_ref size mismatch".to_string());
    }
    let digest = Sha256::digest(&bytes);
    let actual_sha = format!("{digest:x}");
    if !actual_sha.eq_ignore_ascii_case(&content_ref.sha256) {
        return Err("content_ref sha256 mismatch".to_string());
    }
    String::from_utf8(bytes).map_err(|_| "content_ref is not valid UTF-8 text".to_string())
}

#[cfg(unix)]
async fn enqueue_stdin_message(team: &str, agent_id: &str, content: &str) -> Result<(), String> {
    let home = agent_team_mail_core::home::get_home_dir().map_err(|e| e.to_string())?;
    let dir = home
        .join(".config/atm/agent-sessions")
        .join(team)
        .join(agent_id)
        .join("stdin_queue");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("failed to create stdin_queue dir: {e}"))?;
    let path = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
    tokio::fs::write(path, content.as_bytes())
        .await
        .map_err(|e| format!("failed to write stdin_queue file: {e}"))?;
    Ok(())
}

#[cfg(unix)]
async fn process_control_request(
    control: ControlRequest,
    state_store: &SharedStateStore,
    session_registry: &SharedSessionRegistry,
) -> ControlAck {
    emit_control_request_event(&control);

    if let Some(err) = validate_control_request(&control) {
        let ack = control_ack(
            &control.request_id,
            ControlResult::Rejected,
            false,
            Some(err),
        );
        emit_control_ack_event(&control, &ack);
        return ack;
    }

    let inline_len = control.payload.as_ref().map(|s| s.len()).unwrap_or(0);
    if inline_len > 64 * 1024 {
        emit_event_best_effort(EventFields {
            level: "warn",
            source: "atm-daemon",
            action: "control_payload_soft_limit_exceeded",
            team: Some(control.team.clone()),
            session_id: Some(control.session_id.clone()),
            agent_id: Some(control.agent_id.clone()),
            request_id: Some(control.request_id.clone()),
            count: Some(inline_len as u64),
            error: Some("payload exceeds 64KiB soft limit".to_string()),
            ..Default::default()
        });
    }

    if matches!(control.action, ControlAction::Interrupt) {
        let ack = control_ack(
            &control.request_id,
            ControlResult::Rejected,
            false,
            Some("interrupt receiver path not yet implemented".to_string()),
        );
        emit_control_ack_event(&control, &ack);
        return ack;
    }

    // Only accepted actions consume dedupe slots.
    let key = DedupeKey::new(
        &control.team,
        &control.session_id,
        &control.agent_id,
        &control.request_id,
    );
    let is_duplicate = control_dedupe_store().lock().unwrap().check_and_insert(key);
    if is_duplicate {
        let ack = control_ack(
            &control.request_id,
            ControlResult::Ok,
            true,
            Some("duplicate request_id".to_string()),
        );
        emit_control_ack_event(&control, &ack);
        return ack;
    }

    let live = control_request_is_live(&control, state_store, session_registry);
    if live != ControlResult::Ok {
        let ack = control_ack(
            &control.request_id,
            live,
            false,
            Some("target session is not live".to_string()),
        );
        emit_control_ack_event(&control, &ack);
        return ack;
    }

    let ack = match control.action {
        ControlAction::Interrupt => unreachable!("interrupt handled before dedupe"),
        ControlAction::Stdin => {
            let content = if let Some(payload) = control.payload.clone() {
                payload
            } else if let Some(ref cref) = control.content_ref {
                match read_content_ref_text(cref) {
                    Ok(t) => t,
                    Err(e) => {
                        let ack = control_ack(
                            &control.request_id,
                            ControlResult::Rejected,
                            false,
                            Some(e),
                        );
                        emit_control_ack_event(&control, &ack);
                        return ack;
                    }
                }
            } else {
                let ack = control_ack(
                    &control.request_id,
                    ControlResult::Rejected,
                    false,
                    Some("stdin control requires payload or content_ref".to_string()),
                );
                emit_control_ack_event(&control, &ack);
                return ack;
            };

            if content.trim().is_empty() {
                control_ack(
                    &control.request_id,
                    ControlResult::Rejected,
                    false,
                    Some("stdin payload cannot be empty".to_string()),
                )
            } else {
                match enqueue_stdin_message(&control.team, &control.agent_id, &content).await {
                    Ok(()) => control_ack(
                        &control.request_id,
                        ControlResult::Ok,
                        false,
                        Some("queued for next idle drain".to_string()),
                    ),
                    Err(e) => control_ack(
                        &control.request_id,
                        ControlResult::InternalError,
                        false,
                        Some(format!("enqueue failed: {e}")),
                    ),
                }
            }
        }
    };
    emit_control_ack_event(&control, &ack);
    ack
}

/// Parse a raw JSON request line and dispatch to the appropriate synchronous handler.
///
/// Note: the `"launch"` command is handled asynchronously before this function
/// is called (see `handle_launch_command`).
fn parse_and_dispatch(
    request_str: &str,
    state_store: &SharedStateStore,
    pubsub_store: &SharedPubSubStore,
    session_registry: &SharedSessionRegistry,
) -> Result<SocketResponse> {
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};

    // Parse request envelope
    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            warn!("Malformed socket request: {e} — raw: {request_str}");
            return Ok(make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse request: {e}"),
            ));
        }
    };

    debug!(
        "Socket request: command={} request_id={}",
        request.command, request.request_id
    );

    // Validate protocol version
    if request.version != PROTOCOL_VERSION {
        return Ok(make_error_response(
            &request.request_id,
            "VERSION_MISMATCH",
            &format!(
                "Unsupported protocol version {}; server supports {}",
                request.version, PROTOCOL_VERSION
            ),
        ));
    }

    let response = match request.command.as_str() {
        "agent-state" => handle_agent_state(&request, state_store),
        "list-agents" => handle_list_agents(&request, state_store),
        "agent-pane" => handle_agent_pane(&request, state_store),
        "subscribe" => handle_subscribe(&request, pubsub_store),
        "unsubscribe" => handle_unsubscribe(&request, pubsub_store),
        "session-query" => handle_session_query(&request, session_registry),
        // "launch" is handled asynchronously before parse_and_dispatch is called.
        // If it somehow reaches here, return a clear internal error.
        "launch" => make_error_response(
            &request.request_id,
            "INTERNAL_ERROR",
            "Launch command should have been handled by the async path",
        ),
        // "control" is handled asynchronously before parse_and_dispatch is called.
        "control" => make_error_response(
            &request.request_id,
            "INTERNAL_ERROR",
            "Control command should have been handled by the async path",
        ),
        // "hook-event" is handled asynchronously before parse_and_dispatch is called.
        "hook-event" => make_error_response(
            &request.request_id,
            "INTERNAL_ERROR",
            "hook-event command should have been handled by the async path",
        ),
        other => make_error_response(
            &request.request_id,
            "UNKNOWN_COMMAND",
            &format!("Unknown command: '{other}'"),
        ),
    };

    Ok(response)
}

/// Handle the `session-query` command.
///
/// Payload: `{"name": "<agent-name>"}`
/// Response (found, alive):   `{"session_id": "...", "process_id": 12345, "alive": true}`
/// Response (found, dead):    `{"session_id": "...", "process_id": 12345, "alive": false}`
/// Response (not found):      error with code `AGENT_NOT_FOUND`
fn handle_session_query(
    request: &agent_team_mail_core::daemon_client::SocketRequest,
    session_registry: &SharedSessionRegistry,
) -> SocketResponse {
    let name = match request.payload.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => {
            return make_error_response(
                &request.request_id,
                "MISSING_PARAMETER",
                "Missing required payload field: 'name'",
            );
        }
    };

    let registry = session_registry.lock().unwrap();
    match registry.query(&name) {
        Some(record) => {
            let alive = record.is_process_alive();
            make_ok_response(
                &request.request_id,
                serde_json::json!({
                    "session_id": record.session_id,
                    "process_id": record.process_id,
                    "alive": alive,
                }),
            )
        }
        None => make_error_response(
            &request.request_id,
            "AGENT_NOT_FOUND",
            &format!("No session record for agent '{name}'"),
        ),
    }
}

/// Handle the `agent-state` command.
///
/// Payload: `{"agent": "<name>", "team": "<team>"}`  (team is currently informational)
/// Response: `{"state": "<state>", "last_transition": "<iso8601 or null>"}`
fn handle_agent_state(
    request: &agent_team_mail_core::daemon_client::SocketRequest,
    state_store: &SharedStateStore,
) -> SocketResponse {
    let agent = request
        .payload
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if agent.is_empty() {
        return make_error_response(
            &request.request_id,
            "MISSING_PARAMETER",
            "Missing required payload field: 'agent'",
        );
    }

    let tracker = state_store.lock().unwrap();
    match tracker.get_state(&agent) {
        Some(state) => {
            let last_transition = tracker
                .time_since_transition(&agent)
                .map(format_elapsed_as_iso8601);

            make_ok_response(
                &request.request_id,
                serde_json::json!({
                    "state": state.to_string(),
                    "last_transition": last_transition,
                }),
            )
        }
        None => make_error_response(
            &request.request_id,
            "AGENT_NOT_FOUND",
            &format!("Agent '{agent}' is not tracked"),
        ),
    }
}

/// Handle the `list-agents` command.
///
/// Payload: `{}`
/// Response: array of `{"agent": "<name>", "state": "<state>"}`
fn handle_list_agents(
    request: &agent_team_mail_core::daemon_client::SocketRequest,
    state_store: &SharedStateStore,
) -> SocketResponse {
    let tracker = state_store.lock().unwrap();
    let agents: Vec<serde_json::Value> = tracker
        .all_states()
        .into_iter()
        .map(|(agent, state)| {
            serde_json::json!({
                "agent": agent,
                "state": state.to_string(),
            })
        })
        .collect();

    make_ok_response(&request.request_id, serde_json::json!(agents))
}

/// Handle the `agent-pane` command.
///
/// Returns the tmux pane ID and log file path for the given agent so that
/// the CLI `atm tail` command can locate the log file.
///
/// Payload: `{"agent": "<name>"}`
/// Response: `{"pane_id": "%42", "log_path": "/path/to/agent.log"}`
fn handle_agent_pane(
    request: &agent_team_mail_core::daemon_client::SocketRequest,
    state_store: &SharedStateStore,
) -> SocketResponse {
    let agent = request
        .payload
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if agent.is_empty() {
        return make_error_response(
            &request.request_id,
            "MISSING_PARAMETER",
            "Missing required payload field: 'agent'",
        );
    }

    let tracker = state_store.lock().unwrap();
    match tracker.get_pane_info(&agent) {
        Some(info) => make_ok_response(
            &request.request_id,
            serde_json::json!({
                "pane_id": info.pane_id,
                "log_path": info.log_path.to_string_lossy(),
            }),
        ),
        None => make_error_response(
            &request.request_id,
            "AGENT_NOT_FOUND",
            &format!("Agent '{agent}' is not tracked or has no pane info"),
        ),
    }
}

/// Handle the `subscribe` command.
///
/// Payload: `{"subscriber": "<identity>", "agent": "<name>", "events": ["idle"], "team": "<team>"}`
/// Response: `{"subscribed": true, "subscriber": "...", "agent": "..."}`
fn handle_subscribe(
    request: &agent_team_mail_core::daemon_client::SocketRequest,
    pubsub_store: &SharedPubSubStore,
) -> SocketResponse {
    let subscriber = match request.payload.get("subscriber").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return make_error_response(
                &request.request_id,
                "MISSING_PARAMETER",
                "Missing required payload field: 'subscriber'",
            );
        }
    };

    let agent = match request.payload.get("agent").and_then(|v| v.as_str()) {
        Some(a) if !a.is_empty() => a.to_string(),
        _ => {
            return make_error_response(
                &request.request_id,
                "MISSING_PARAMETER",
                "Missing required payload field: 'agent'",
            );
        }
    };

    // `events` is optional; empty list is a wildcard
    let events: Vec<String> = request
        .payload
        .get("events")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let mut pubsub = pubsub_store.lock().unwrap();
    match pubsub.subscribe(&subscriber, &agent, events) {
        Ok(()) => {
            debug!("Registered subscription: {subscriber} → {agent}");
            make_ok_response(
                &request.request_id,
                serde_json::json!({
                    "subscribed": true,
                    "subscriber": subscriber,
                    "agent": agent,
                }),
            )
        }
        Err(e) => make_error_response(&request.request_id, "CAP_EXCEEDED", &e.to_string()),
    }
}

/// Handle the `unsubscribe` command.
///
/// Payload: `{"subscriber": "<identity>", "agent": "<name>", "team": "<team>"}`
/// Response: `{"unsubscribed": true, "subscriber": "...", "agent": "..."}`
fn handle_unsubscribe(
    request: &agent_team_mail_core::daemon_client::SocketRequest,
    pubsub_store: &SharedPubSubStore,
) -> SocketResponse {
    let subscriber = match request.payload.get("subscriber").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return make_error_response(
                &request.request_id,
                "MISSING_PARAMETER",
                "Missing required payload field: 'subscriber'",
            );
        }
    };

    let agent = match request.payload.get("agent").and_then(|v| v.as_str()) {
        Some(a) if !a.is_empty() => a.to_string(),
        _ => {
            return make_error_response(
                &request.request_id,
                "MISSING_PARAMETER",
                "Missing required payload field: 'agent'",
            );
        }
    };

    pubsub_store
        .lock()
        .unwrap()
        .unsubscribe(&subscriber, &agent);
    debug!("Removed subscription: {subscriber} → {agent}");

    make_ok_response(
        &request.request_id,
        serde_json::json!({
            "unsubscribed": true,
            "subscriber": subscriber,
            "agent": agent,
        }),
    )
}

// ── Response helpers ──────────────────────────────────────────────────────────

use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketError, SocketResponse};

fn make_ok_response(request_id: &str, payload: serde_json::Value) -> SocketResponse {
    SocketResponse {
        version: PROTOCOL_VERSION,
        request_id: request_id.to_string(),
        status: "ok".to_string(),
        payload: Some(payload),
        error: None,
    }
}

fn make_error_response(request_id: &str, code: &str, message: &str) -> SocketResponse {
    SocketResponse {
        version: PROTOCOL_VERSION,
        request_id: request_id.to_string(),
        status: "error".to_string(),
        payload: None,
        error: Some(SocketError {
            code: code.to_string(),
            message: message.to_string(),
        }),
    }
}

/// Convert an elapsed duration since last state transition into an approximate
/// ISO 8601 timestamp by subtracting from now.
fn format_elapsed_as_iso8601(elapsed: std::time::Duration) -> String {
    use chrono::Utc;
    let now = Utc::now();
    let transition_time = now - chrono::Duration::from_std(elapsed).unwrap_or_default();
    transition_time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::session_registry::new_session_registry;
    use crate::plugins::worker_adapter::AgentStateTracker;
    use agent_team_mail_core::control::{CONTROL_SCHEMA_VERSION, ControlAction, ControlRequest};
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};

    fn make_store() -> SharedStateStore {
        std::sync::Arc::new(std::sync::Mutex::new(AgentStateTracker::new()))
    }

    fn make_ps() -> SharedPubSubStore {
        new_pubsub_store()
    }

    fn make_sr() -> SharedSessionRegistry {
        new_session_registry()
    }

    fn make_request(command: &str, payload: serde_json::Value) -> SocketRequest {
        SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-test".to_string(),
            command: command.to_string(),
            payload,
        }
    }

    #[test]
    fn test_agent_state_not_found() {
        let store = make_store();
        let req = make_request(
            "agent-state",
            serde_json::json!({"agent": "ghost", "team": "t"}),
        );
        let resp = handle_agent_state(&req, &store);
        assert_eq!(resp.status, "error");
        let err = resp.error.unwrap();
        assert_eq!(err.code, "AGENT_NOT_FOUND");
    }

    #[test]
    fn test_agent_state_found() {
        use crate::plugins::worker_adapter::AgentState;

        let store = make_store();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Idle);
        }

        let req = make_request(
            "agent-state",
            serde_json::json!({"agent": "arch-ctm", "team": "atm-dev"}),
        );
        let resp = handle_agent_state(&req, &store);
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["state"].as_str().unwrap(), "idle");
    }

    #[test]
    fn test_list_agents_empty() {
        let store = make_store();
        let req = make_request("list-agents", serde_json::json!({}));
        let resp = handle_list_agents(&req, &store);
        assert_eq!(resp.status, "ok");
        let agents = resp.payload.unwrap();
        assert!(agents.as_array().unwrap().is_empty());
    }

    #[test]
    fn test_list_agents_with_entries() {
        use crate::plugins::worker_adapter::AgentState;

        let store = make_store();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Idle);
            tracker.register_agent("worker-1");
        }

        let req = make_request("list-agents", serde_json::json!({}));
        let resp = handle_list_agents(&req, &store);
        assert_eq!(resp.status, "ok");
        let agents = resp.payload.unwrap();
        let arr = agents.as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn test_launch_command_missing_agent() {
        // parse_and_dispatch receives a "launch" command — it should return INTERNAL_ERROR
        // because the async path should have handled it, but the payload may be inspected.
        let store = make_store();
        let ps = make_ps();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r1","command":"launch","payload":{"agent":"","team":"atm-dev","command":"codex","timeout_secs":30,"env_vars":{}}}"#;
        let resp = parse_and_dispatch(req_json, &store, &ps, &sr).unwrap();
        // In parse_and_dispatch the "launch" arm returns INTERNAL_ERROR
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "INTERNAL_ERROR");
    }

    #[test]
    #[cfg(unix)]
    fn test_is_launch_command_detection() {
        assert!(is_launch_command(
            r#"{"version":1,"request_id":"r1","command":"launch","payload":{}}"#
        ));
        assert!(is_launch_command(
            r#"{"version":1,"request_id":"r1","command": "launch","payload":{}}"#
        ));
        assert!(!is_launch_command(
            r#"{"version":1,"request_id":"r1","command":"agent-state","payload":{}}"#
        ));
        assert!(!is_launch_command(
            r#"{"version":1,"request_id":"r1","command":"list-agents","payload":{}}"#
        ));
    }

    #[test]
    #[cfg(unix)]
    fn test_is_control_command_detection() {
        assert!(is_control_command(
            r#"{"version":1,"request_id":"r1","command":"control","payload":{}}"#
        ));
        assert!(is_control_command(
            r#"{"version":1,"request_id":"r1","command": "control","payload":{}}"#
        ));
        assert!(!is_control_command(
            r#"{"version":1,"request_id":"r1","command":"agent-state","payload":{}}"#
        ));
    }

    #[test]
    fn test_parse_and_dispatch_unknown_command() {
        let store = make_store();
        let ps = make_ps();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r1","command":"bogus","payload":{}}"#;
        let resp = parse_and_dispatch(req_json, &store, &ps, &sr).unwrap();
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "UNKNOWN_COMMAND");
    }

    #[test]
    fn test_parse_and_dispatch_malformed_json() {
        let store = make_store();
        let ps = make_ps();
        let sr = make_sr();
        let resp = parse_and_dispatch("not-json{{", &store, &ps, &sr).unwrap();
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "INVALID_REQUEST");
    }

    #[test]
    fn test_parse_and_dispatch_version_mismatch() {
        let store = make_store();
        let ps = make_ps();
        let sr = make_sr();
        let req_json = r#"{"version":99,"request_id":"r1","command":"agent-state","payload":{}}"#;
        let resp = parse_and_dispatch(req_json, &store, &ps, &sr).unwrap();
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "VERSION_MISMATCH");
    }

    #[test]
    fn test_agent_state_missing_agent_field() {
        let store = make_store();
        let req = make_request("agent-state", serde_json::json!({}));
        let resp = handle_agent_state(&req, &store);
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "MISSING_PARAMETER");
    }

    #[test]
    fn test_agent_pane_not_found() {
        let store = make_store();
        let req = make_request("agent-pane", serde_json::json!({"agent": "ghost"}));
        let resp = handle_agent_pane(&req, &store);
        assert_eq!(resp.status, "error");
        let err = resp.error.unwrap();
        assert_eq!(err.code, "AGENT_NOT_FOUND");
    }

    #[test]
    fn test_agent_pane_found() {
        let store = make_store();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_pane_info("arch-ctm", "%42", std::path::Path::new("/tmp/arch-ctm.log"));
        }

        let req = make_request("agent-pane", serde_json::json!({"agent": "arch-ctm"}));
        let resp = handle_agent_pane(&req, &store);
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["pane_id"].as_str().unwrap(), "%42");
        assert_eq!(payload["log_path"].as_str().unwrap(), "/tmp/arch-ctm.log");
    }

    #[test]
    fn test_agent_pane_missing_agent_field() {
        let store = make_store();
        let req = make_request("agent-pane", serde_json::json!({}));
        let resp = handle_agent_pane(&req, &store);
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "MISSING_PARAMETER");
    }

    #[test]
    fn test_make_ok_response_structure() {
        let resp = make_ok_response("req-1", serde_json::json!({"key": "value"}));
        assert_eq!(resp.status, "ok");
        assert_eq!(resp.version, PROTOCOL_VERSION);
        assert!(resp.error.is_none());
        assert!(resp.payload.is_some());
    }

    #[test]
    fn test_make_error_response_structure() {
        let resp = make_error_response("req-2", "MY_ERROR", "Something went wrong");
        assert_eq!(resp.status, "error");
        assert!(resp.payload.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, "MY_ERROR");
        assert_eq!(err.message, "Something went wrong");
    }

    // ── subscribe / unsubscribe handler tests ──────────────────────────────────

    #[test]
    fn test_handle_subscribe_success() {
        let ps = make_ps();
        let req = make_request(
            "subscribe",
            serde_json::json!({"subscriber": "team-lead", "agent": "arch-ctm", "events": ["idle"], "team": "atm-dev"}),
        );
        let resp = handle_subscribe(&req, &ps);
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["subscribed"].as_bool().unwrap());
        assert_eq!(payload["subscriber"].as_str().unwrap(), "team-lead");
        assert_eq!(payload["agent"].as_str().unwrap(), "arch-ctm");

        // Confirm subscription is in the store
        let matches = ps.lock().unwrap().matching_subscribers("arch-ctm", "idle");
        assert_eq!(matches, vec!["team-lead"]);
    }

    #[test]
    fn test_handle_subscribe_missing_subscriber() {
        let ps = make_ps();
        let req = make_request(
            "subscribe",
            serde_json::json!({"agent": "arch-ctm", "events": ["idle"]}),
        );
        let resp = handle_subscribe(&req, &ps);
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "MISSING_PARAMETER");
    }

    #[test]
    fn test_handle_subscribe_missing_agent() {
        let ps = make_ps();
        let req = make_request(
            "subscribe",
            serde_json::json!({"subscriber": "team-lead", "events": ["idle"]}),
        );
        let resp = handle_subscribe(&req, &ps);
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "MISSING_PARAMETER");
    }

    #[test]
    fn test_handle_unsubscribe_success() {
        let ps = make_ps();
        // First subscribe
        {
            ps.lock()
                .unwrap()
                .subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
                .unwrap();
        }

        let req = make_request(
            "unsubscribe",
            serde_json::json!({"subscriber": "team-lead", "agent": "arch-ctm", "team": "atm-dev"}),
        );
        let resp = handle_unsubscribe(&req, &ps);
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["unsubscribed"].as_bool().unwrap());

        // Confirm subscription is gone
        let matches = ps.lock().unwrap().matching_subscribers("arch-ctm", "idle");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_handle_unsubscribe_missing_fields() {
        let ps = make_ps();
        let req = make_request(
            "unsubscribe",
            serde_json::json!({"subscriber": "team-lead"}),
        );
        let resp = handle_unsubscribe(&req, &ps);
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "MISSING_PARAMETER");
    }

    #[test]
    fn test_subscribe_cap_exceeded_returns_error() {
        use crate::plugins::worker_adapter::PubSub;
        use std::time::Duration;

        // Create a pub/sub store with cap=1
        let ps: SharedPubSubStore = std::sync::Arc::new(std::sync::Mutex::new(
            PubSub::with_config(Duration::from_secs(3600), 1),
        ));

        let req1 = make_request(
            "subscribe",
            serde_json::json!({"subscriber": "team-lead", "agent": "agent-a", "events": ["idle"]}),
        );
        let resp1 = handle_subscribe(&req1, &ps);
        assert_eq!(resp1.status, "ok");

        let req2 = make_request(
            "subscribe",
            serde_json::json!({"subscriber": "team-lead", "agent": "agent-b", "events": ["idle"]}),
        );
        let resp2 = handle_subscribe(&req2, &ps);
        assert_eq!(resp2.status, "error");
        assert_eq!(resp2.error.unwrap().code, "CAP_EXCEEDED");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial]
    async fn test_control_stdin_enqueues_payload() {
        use crate::plugins::worker_adapter::AgentState;
        use uuid::Uuid;

        let tmp = tempfile::TempDir::new().unwrap();
        // SAFETY: serialized test; env var restored by process teardown.
        unsafe { std::env::set_var("ATM_HOME", tmp.path()) };

        let state_store = make_store();
        {
            let mut tracker = state_store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Idle);
        }
        let sr = make_sr();
        {
            sr.lock()
                .unwrap()
                .upsert("arch-ctm", "sess-1", std::process::id());
        }

        let req = ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: Uuid::new_v4().to_string(),
            msg_type: "control.stdin.request".to_string(),
            signal: None,
            sent_at: chrono::Utc::now().to_rfc3339(),
            team: "atm-dev".to_string(),
            session_id: "sess-1".to_string(),
            agent_id: "arch-ctm".to_string(),
            sender: "team-lead".to_string(),
            action: ControlAction::Stdin,
            payload: Some("hello from control".to_string()),
            content_ref: None,
        };

        let ack = process_control_request(req, &state_store, &sr).await;
        assert_eq!(ack.result, agent_team_mail_core::control::ControlResult::Ok);
        assert!(!ack.duplicate);

        let qdir = tmp
            .path()
            .join(".config/atm/agent-sessions/atm-dev/arch-ctm/stdin_queue");
        assert!(qdir.exists());
        let mut rd = tokio::fs::read_dir(qdir).await.unwrap();
        let mut files = 0usize;
        while let Ok(Some(_)) = rd.next_entry().await {
            files += 1;
        }
        assert_eq!(files, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial]
    async fn test_control_duplicate_does_not_reenqueue() {
        use crate::plugins::worker_adapter::AgentState;
        use uuid::Uuid;

        let tmp = tempfile::TempDir::new().unwrap();
        // SAFETY: serialized test; env var restored by process teardown.
        unsafe { std::env::set_var("ATM_HOME", tmp.path()) };

        let state_store = make_store();
        {
            let mut tracker = state_store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Busy);
        }
        let sr = make_sr();
        {
            sr.lock()
                .unwrap()
                .upsert("arch-ctm", "sess-dup", std::process::id());
        }
        let request_id = Uuid::new_v4().to_string();
        let mk_req = || ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: request_id.clone(),
            msg_type: "control.stdin.request".to_string(),
            signal: None,
            sent_at: chrono::Utc::now().to_rfc3339(),
            team: "atm-dev".to_string(),
            session_id: "sess-dup".to_string(),
            agent_id: "arch-ctm".to_string(),
            sender: "team-lead".to_string(),
            action: ControlAction::Stdin,
            payload: Some("payload".to_string()),
            content_ref: None,
        };

        let ack1 = process_control_request(mk_req(), &state_store, &sr).await;
        let ack2 = process_control_request(mk_req(), &state_store, &sr).await;
        assert_eq!(
            ack1.result,
            agent_team_mail_core::control::ControlResult::Ok
        );
        assert_eq!(
            ack2.result,
            agent_team_mail_core::control::ControlResult::Ok
        );
        assert!(ack2.duplicate);

        let qdir = tmp
            .path()
            .join(".config/atm/agent-sessions/atm-dev/arch-ctm/stdin_queue");
        let mut rd = tokio::fs::read_dir(qdir).await.unwrap();
        let mut files = 0usize;
        while let Ok(Some(_)) = rd.next_entry().await {
            files += 1;
        }
        assert_eq!(files, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_control_interrupt_returns_rejected() {
        use crate::plugins::worker_adapter::AgentState;
        use uuid::Uuid;

        let state_store = make_store();
        {
            let mut tracker = state_store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Idle);
        }
        let sr = make_sr();
        {
            sr.lock()
                .unwrap()
                .upsert("arch-ctm", "sess-int", std::process::id());
        }
        let req = ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: Uuid::new_v4().to_string(),
            msg_type: "control.interrupt.request".to_string(),
            signal: Some("interrupt".to_string()),
            sent_at: chrono::Utc::now().to_rfc3339(),
            team: "atm-dev".to_string(),
            session_id: "sess-int".to_string(),
            agent_id: "arch-ctm".to_string(),
            sender: "team-lead".to_string(),
            action: ControlAction::Interrupt,
            payload: None,
            content_ref: None,
        };
        let ack = process_control_request(req, &state_store, &sr).await;
        assert_eq!(
            ack.result,
            agent_team_mail_core::control::ControlResult::Rejected
        );
        assert!(
            ack.detail
                .unwrap_or_default()
                .contains("not yet implemented")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_control_interrupt_retry_is_not_marked_duplicate() {
        use crate::plugins::worker_adapter::AgentState;
        use uuid::Uuid;

        let state_store = make_store();
        {
            let mut tracker = state_store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Idle);
        }
        let sr = make_sr();
        {
            sr.lock()
                .unwrap()
                .upsert("arch-ctm", "sess-int-retry", std::process::id());
        }

        let request_id = Uuid::new_v4().to_string();
        let mk_req = || ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: request_id.clone(),
            msg_type: "control.interrupt.request".to_string(),
            signal: Some("interrupt".to_string()),
            sent_at: chrono::Utc::now().to_rfc3339(),
            team: "atm-dev".to_string(),
            session_id: "sess-int-retry".to_string(),
            agent_id: "arch-ctm".to_string(),
            sender: "team-lead".to_string(),
            action: ControlAction::Interrupt,
            payload: None,
            content_ref: None,
        };

        let ack1 = process_control_request(mk_req(), &state_store, &sr).await;
        let ack2 = process_control_request(mk_req(), &state_store, &sr).await;
        assert_eq!(
            ack1.result,
            agent_team_mail_core::control::ControlResult::Rejected
        );
        assert_eq!(
            ack2.result,
            agent_team_mail_core::control::ControlResult::Rejected
        );
        assert!(!ack1.duplicate);
        assert!(!ack2.duplicate);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_control_stale_sent_at_rejected() {
        use crate::plugins::worker_adapter::AgentState;
        use uuid::Uuid;

        let state_store = make_store();
        {
            let mut tracker = state_store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Idle);
        }
        let sr = make_sr();
        {
            sr.lock()
                .unwrap()
                .upsert("arch-ctm", "sess-stale", std::process::id());
        }

        let old = chrono::Utc::now() - chrono::Duration::minutes(10);
        let req = ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: Uuid::new_v4().to_string(),
            msg_type: "control.stdin.request".to_string(),
            signal: None,
            sent_at: old.to_rfc3339(),
            team: "atm-dev".to_string(),
            session_id: "sess-stale".to_string(),
            agent_id: "arch-ctm".to_string(),
            sender: "team-lead".to_string(),
            action: ControlAction::Stdin,
            payload: Some("payload".to_string()),
            content_ref: None,
        };
        let ack = process_control_request(req, &state_store, &sr).await;
        assert_eq!(
            ack.result,
            agent_team_mail_core::control::ControlResult::Rejected
        );
    }

    /// Integration-style test: start server, connect, exchange request/response.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_socket_server_agent_state_roundtrip() {
        use crate::plugins::worker_adapter::AgentState;
        use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio_util::sync::CancellationToken;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let home_dir = temp_dir.path().to_path_buf();
        let cancel = CancellationToken::new();

        // Set up state store with one agent
        let state_store = make_store();
        {
            let mut tracker = state_store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Idle);
        }

        // Start the socket server
        let launch_tx = new_launch_sender();
        let _handle = start_socket_server(
            home_dir.clone(),
            state_store,
            new_pubsub_store(),
            launch_tx,
            make_sr(),
            cancel.clone(),
        )
        .await
        .unwrap()
        .expect("Expected socket server handle on unix");

        // Connect and send a request
        let socket_path = home_dir.join(".claude/daemon/atm-daemon.sock");
        let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();

        let request = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "integration-test-1".to_string(),
            command: "agent-state".to_string(),
            payload: serde_json::json!({"agent": "arch-ctm", "team": "atm-dev"}),
        };
        let req_line = format!("{}\n", serde_json::to_string(&request).unwrap());

        let mut reader = BufReader::new(stream);
        reader
            .get_mut()
            .write_all(req_line.as_bytes())
            .await
            .unwrap();

        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await.unwrap();

        let resp: agent_team_mail_core::daemon_client::SocketResponse =
            serde_json::from_str(resp_line.trim()).unwrap();

        assert!(resp.is_ok(), "Expected ok response, got: {:?}", resp.error);
        let payload = resp.payload.unwrap();
        assert_eq!(payload["state"].as_str().unwrap(), "idle");

        cancel.cancel();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_socket_server_list_agents_roundtrip() {
        use crate::plugins::worker_adapter::AgentState;
        use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio_util::sync::CancellationToken;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let home_dir = temp_dir.path().to_path_buf();
        let cancel = CancellationToken::new();

        let state_store = make_store();
        {
            let mut tracker = state_store.lock().unwrap();
            tracker.register_agent("agent-a");
            tracker.register_agent("agent-b");
            tracker.set_state("agent-b", AgentState::Busy);
        }

        let launch_tx = new_launch_sender();
        let _handle = start_socket_server(
            home_dir.clone(),
            state_store,
            new_pubsub_store(),
            launch_tx,
            make_sr(),
            cancel.clone(),
        )
        .await
        .unwrap()
        .expect("Expected socket server handle on unix");

        let socket_path = home_dir.join(".claude/daemon/atm-daemon.sock");
        let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();

        let request = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "list-test-1".to_string(),
            command: "list-agents".to_string(),
            payload: serde_json::json!({}),
        };
        let req_line = format!("{}\n", serde_json::to_string(&request).unwrap());

        let mut reader = BufReader::new(stream);
        reader
            .get_mut()
            .write_all(req_line.as_bytes())
            .await
            .unwrap();

        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await.unwrap();

        let resp: agent_team_mail_core::daemon_client::SocketResponse =
            serde_json::from_str(resp_line.trim()).unwrap();

        assert!(resp.is_ok());
        let agents = resp.payload.unwrap();
        let arr = agents.as_array().unwrap();
        assert_eq!(arr.len(), 2);

        cancel.cancel();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_socket_server_subscribe_roundtrip() {
        use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio_util::sync::CancellationToken;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let home_dir = temp_dir.path().to_path_buf();
        let cancel = CancellationToken::new();

        let launch_tx = new_launch_sender();
        let _handle = start_socket_server(
            home_dir.clone(),
            make_store(),
            new_pubsub_store(),
            launch_tx,
            make_sr(),
            cancel.clone(),
        )
        .await
        .unwrap()
        .expect("Expected socket server handle on unix");

        let socket_path = home_dir.join(".claude/daemon/atm-daemon.sock");
        let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();

        let request = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "sub-test-1".to_string(),
            command: "subscribe".to_string(),
            payload: serde_json::json!({
                "subscriber": "team-lead",
                "agent": "arch-ctm",
                "events": ["idle"],
                "team": "atm-dev"
            }),
        };
        let req_line = format!("{}\n", serde_json::to_string(&request).unwrap());

        let mut reader = BufReader::new(stream);
        reader
            .get_mut()
            .write_all(req_line.as_bytes())
            .await
            .unwrap();

        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await.unwrap();

        let resp: agent_team_mail_core::daemon_client::SocketResponse =
            serde_json::from_str(resp_line.trim()).unwrap();

        assert!(resp.is_ok(), "Expected ok, got: {:?}", resp.error);
        let payload = resp.payload.unwrap();
        assert!(payload["subscribed"].as_bool().unwrap());

        cancel.cancel();
    }

    /// Integration-style control test over unix socket.
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "integration coverage for control receiver over unix socket"]
    #[serial_test::serial]
    async fn test_socket_server_control_stdin_roundtrip() {
        use crate::plugins::worker_adapter::AgentState;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio_util::sync::CancellationToken;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let home_dir = temp_dir.path().to_path_buf();
        // SAFETY: serialized test; env var scoped by process.
        unsafe { std::env::set_var("ATM_HOME", &home_dir) };
        let cancel = CancellationToken::new();

        let state_store = make_store();
        {
            let mut tracker = state_store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Idle);
        }
        let sr = make_sr();
        {
            sr.lock()
                .unwrap()
                .upsert("arch-ctm", "sess-intg-1", std::process::id());
        }

        let launch_tx = new_launch_sender();
        let _handle = start_socket_server(
            home_dir.clone(),
            state_store,
            new_pubsub_store(),
            launch_tx,
            sr,
            cancel.clone(),
        )
        .await
        .unwrap()
        .expect("Expected socket server handle on unix");

        let socket_path = home_dir.join(".claude/daemon/atm-daemon.sock");
        let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();

        let control_payload = serde_json::json!({
            "v": 1,
            "request_id": "ctrl-intg-1",
            "sent_at": chrono::Utc::now().to_rfc3339(),
            "team": "atm-dev",
            "session_id": "sess-intg-1",
            "agent_id": "arch-ctm",
            "sender": "team-lead",
            "action": "stdin",
            "payload": "integration payload"
        });
        let request = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "sock-ctrl-1".to_string(),
            command: "control".to_string(),
            payload: control_payload,
        };
        let req_line = format!("{}\n", serde_json::to_string(&request).unwrap());
        let mut reader = BufReader::new(stream);
        reader
            .get_mut()
            .write_all(req_line.as_bytes())
            .await
            .unwrap();

        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await.unwrap();
        let resp: agent_team_mail_core::daemon_client::SocketResponse =
            serde_json::from_str(resp_line.trim()).unwrap();
        assert!(resp.is_ok(), "Expected ok response, got: {:?}", resp.error);
        let payload = resp.payload.unwrap();
        assert_eq!(payload["result"].as_str().unwrap(), "ok");
        assert!(!payload["duplicate"].as_bool().unwrap());

        cancel.cancel();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_socket_server_pid_file_written() {
        use tokio_util::sync::CancellationToken;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let home_dir = temp_dir.path().to_path_buf();
        let cancel = CancellationToken::new();
        let state_store = make_store();

        let launch_tx = new_launch_sender();
        let _handle = start_socket_server(
            home_dir.clone(),
            state_store,
            new_pubsub_store(),
            launch_tx,
            make_sr(),
            cancel.clone(),
        )
        .await
        .unwrap()
        .expect("Expected socket server handle on unix");

        let pid_path = home_dir.join(".claude/daemon/atm-daemon.pid");
        assert!(
            pid_path.exists(),
            "PID file should exist after server start"
        );

        let pid_str = std::fs::read_to_string(&pid_path).unwrap();
        let pid: u32 = pid_str.trim().parse().unwrap();
        assert_eq!(pid, std::process::id());

        cancel.cancel();
    }

    // ── hook-event handler tests ───────────────────────────────────────────────

    #[test]
    #[cfg(unix)]
    fn test_is_hook_event_command_detection() {
        assert!(is_hook_event_command(
            r#"{"version":1,"request_id":"r1","command":"hook-event","payload":{}}"#
        ));
        assert!(is_hook_event_command(
            r#"{"version":1,"request_id":"r1","command": "hook-event","payload":{}}"#
        ));
        assert!(!is_hook_event_command(
            r#"{"version":1,"request_id":"r1","command":"agent-state","payload":{}}"#
        ));
        assert!(!is_hook_event_command(
            r#"{"version":1,"request_id":"r1","command":"launch","payload":{}}"#
        ));
        assert!(!is_hook_event_command(
            r#"{"version":1,"request_id":"r1","command":"control","payload":{}}"#
        ));
    }

    #[test]
    fn test_parse_and_dispatch_hook_event_internal_error() {
        let store = make_store();
        let ps = make_ps();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r-hook","command":"hook-event","payload":{"event":"session_start","agent":"test-agent","session_id":"s1"}}"#;
        let resp = parse_and_dispatch(req_json, &store, &ps, &sr).unwrap();
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "INTERNAL_ERROR");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_hook_event_session_start_updates_registry() {
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r1","command":"hook-event","payload":{"event":"session_start","agent":"team-lead","session_id":"sess-abc","process_id":9999}}"#;
        let resp = handle_hook_event_command(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());
        assert_eq!(payload["event"].as_str().unwrap(), "session_start");
        assert_eq!(payload["agent"].as_str().unwrap(), "team-lead");

        // Check session registry updated
        let reg = sr.lock().unwrap();
        let record = reg.query("team-lead").unwrap();
        assert_eq!(record.session_id, "sess-abc");
        assert_eq!(record.process_id, 9999);

        // Check agent registered in state tracker
        let tracker = store.lock().unwrap();
        assert!(tracker.get_state("team-lead").is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_hook_event_session_start_idempotent_if_already_tracked() {
        let store = make_store();
        let sr = make_sr();
        // Pre-register agent as Idle
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("team-lead");
            tracker.set_state("team-lead", AgentState::Idle);
        }
        let req_json = r#"{"version":1,"request_id":"r2","command":"hook-event","payload":{"event":"session_start","agent":"team-lead","session_id":"sess-xyz","process_id":1234}}"#;
        let resp = handle_hook_event_command(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        // State should remain Idle (not reset to Launching) — session_start only registers if not already tracked
        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("team-lead"), Some(AgentState::Idle));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_hook_event_teammate_idle_updates_state() {
        let store = make_store();
        let sr = make_sr();
        // Register agent as Busy first
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Busy);
        }
        let req_json = r#"{"version":1,"request_id":"r3","command":"hook-event","payload":{"event":"teammate_idle","agent":"arch-ctm","session_id":"sess-1","team":"atm-dev"}}"#;
        let resp = handle_hook_event_command(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());
        assert_eq!(payload["event"].as_str().unwrap(), "teammate_idle");

        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Idle));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_hook_event_teammate_idle_auto_registers_unknown_agent() {
        let store = make_store();
        let sr = make_sr();
        // Agent not yet registered
        let req_json = r#"{"version":1,"request_id":"r4","command":"hook-event","payload":{"event":"teammate_idle","agent":"new-agent","session_id":"sess-2","team":"atm-dev"}}"#;
        let resp = handle_hook_event_command(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());

        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("new-agent"), Some(AgentState::Idle));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_hook_event_session_end_marks_dead() {
        let store = make_store();
        let sr = make_sr();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Idle);
        }
        {
            sr.lock().unwrap().upsert("arch-ctm", "sess-end", 1111);
        }
        let req_json = r#"{"version":1,"request_id":"r5","command":"hook-event","payload":{"event":"session_end","agent":"arch-ctm","session_id":"sess-end","team":"atm-dev"}}"#;
        let resp = handle_hook_event_command(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());
        assert_eq!(payload["event"].as_str().unwrap(), "session_end");

        // Session registry should be marked dead
        let reg = sr.lock().unwrap();
        let record = reg.query("arch-ctm").unwrap();
        assert_eq!(
            record.state,
            crate::daemon::session_registry::SessionState::Dead
        );

        // State tracker should be Killed
        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Killed));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_hook_event_unknown_type_returns_ok_not_processed() {
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r6","command":"hook-event","payload":{"event":"some_future_event","agent":"team-lead","session_id":"s1"}}"#;
        let resp = handle_hook_event_command(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(!payload["processed"].as_bool().unwrap());
        assert!(payload["reason"]
            .as_str()
            .unwrap()
            .contains("unknown event type"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_hook_event_missing_agent_returns_not_processed() {
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r7","command":"hook-event","payload":{"event":"session_start","agent":"","session_id":"sess-1"}}"#;
        let resp = handle_hook_event_command(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(!payload["processed"].as_bool().unwrap());
        assert_eq!(payload["reason"].as_str().unwrap(), "missing agent");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_hook_event_version_mismatch() {
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":99,"request_id":"r8","command":"hook-event","payload":{"event":"session_start","agent":"team-lead","session_id":"s1"}}"#;
        let resp = handle_hook_event_command(req_json, &store, &sr).await;
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "VERSION_MISMATCH");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_socket_server_hook_event_roundtrip() {
        use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio_util::sync::CancellationToken;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let home_dir = temp_dir.path().to_path_buf();
        let cancel = CancellationToken::new();

        let state_store = make_store();
        let session_registry = make_sr();

        let launch_tx = new_launch_sender();
        let _handle = start_socket_server(
            home_dir.clone(),
            state_store.clone(),
            new_pubsub_store(),
            launch_tx,
            session_registry.clone(),
            cancel.clone(),
        )
        .await
        .unwrap()
        .expect("Expected socket server handle on unix");

        let socket_path = home_dir.join(".claude/daemon/atm-daemon.sock");

        // Send a hook-event/session_start
        let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let request = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "hook-roundtrip-1".to_string(),
            command: "hook-event".to_string(),
            payload: serde_json::json!({
                "event": "session_start",
                "agent": "team-lead",
                "session_id": "sess-roundtrip",
                "process_id": std::process::id(),
                "team": "atm-dev",
                "source": "init",
            }),
        };
        let req_line = format!("{}\n", serde_json::to_string(&request).unwrap());
        let mut reader = BufReader::new(stream);
        reader
            .get_mut()
            .write_all(req_line.as_bytes())
            .await
            .unwrap();
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await.unwrap();
        let resp: agent_team_mail_core::daemon_client::SocketResponse =
            serde_json::from_str(resp_line.trim()).unwrap();
        assert!(resp.is_ok(), "session_start hook-event failed: {:?}", resp.error);
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());

        // Verify session registry updated
        {
            let reg = session_registry.lock().unwrap();
            let record = reg.query("team-lead").unwrap();
            assert_eq!(record.session_id, "sess-roundtrip");
        }

        // Send teammate_idle
        let stream2 = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let request2 = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "hook-roundtrip-2".to_string(),
            command: "hook-event".to_string(),
            payload: serde_json::json!({
                "event": "teammate_idle",
                "agent": "team-lead",
                "session_id": "sess-roundtrip",
                "team": "atm-dev",
            }),
        };
        let req_line2 = format!("{}\n", serde_json::to_string(&request2).unwrap());
        let mut reader2 = BufReader::new(stream2);
        reader2
            .get_mut()
            .write_all(req_line2.as_bytes())
            .await
            .unwrap();
        let mut resp_line2 = String::new();
        reader2.read_line(&mut resp_line2).await.unwrap();
        let resp2: agent_team_mail_core::daemon_client::SocketResponse =
            serde_json::from_str(resp_line2.trim()).unwrap();
        assert!(resp2.is_ok(), "teammate_idle hook-event failed: {:?}", resp2.error);

        // Verify state updated to Idle
        {
            let tracker = state_store.lock().unwrap();
            assert_eq!(tracker.get_state("team-lead"), Some(AgentState::Idle));
        }

        cancel.cancel();
    }

    /// Integration-style test: session_end hook-event over the unix socket marks
    /// the session Dead and the agent Killed, verified via follow-up queries.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_socket_server_hook_event_session_end_roundtrip() {
        use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio_util::sync::CancellationToken;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let home_dir = temp_dir.path().to_path_buf();
        let cancel = CancellationToken::new();

        let state_store = make_store();
        let session_registry = make_sr();

        // Pre-register the agent as Idle and insert a session record.
        {
            let mut tracker = state_store.lock().unwrap();
            tracker.register_agent("team-lead");
            tracker.set_state("team-lead", AgentState::Idle);
        }
        {
            let mut reg = session_registry.lock().unwrap();
            reg.upsert("team-lead", "sess-end-roundtrip", std::process::id());
        }

        let launch_tx = new_launch_sender();
        let _handle = start_socket_server(
            home_dir.clone(),
            state_store.clone(),
            new_pubsub_store(),
            launch_tx,
            session_registry.clone(),
            cancel.clone(),
        )
        .await
        .unwrap()
        .expect("Expected socket server handle on unix");

        let socket_path = home_dir.join(".claude/daemon/atm-daemon.sock");

        // ── Step 1: Send hook-event/session_end ───────────────────────────────
        let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let request = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "end-roundtrip-1".to_string(),
            command: "hook-event".to_string(),
            payload: serde_json::json!({
                "event": "session_end",
                "agent": "team-lead",
                "session_id": "sess-end-roundtrip",
                "team": "atm-dev",
                "reason": "session_exit",
            }),
        };
        let req_line = format!("{}\n", serde_json::to_string(&request).unwrap());
        let mut reader = BufReader::new(stream);
        reader
            .get_mut()
            .write_all(req_line.as_bytes())
            .await
            .unwrap();
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await.unwrap();
        let resp: agent_team_mail_core::daemon_client::SocketResponse =
            serde_json::from_str(resp_line.trim()).unwrap();
        assert!(
            resp.is_ok(),
            "session_end hook-event failed: {:?}",
            resp.error
        );
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());
        assert_eq!(payload["event"].as_str().unwrap(), "session_end");

        // ── Step 2: Query session-query — expects Dead state ──────────────────
        let stream2 = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let request2 = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "end-roundtrip-2".to_string(),
            command: "session-query".to_string(),
            payload: serde_json::json!({"name": "team-lead"}),
        };
        let req_line2 = format!("{}\n", serde_json::to_string(&request2).unwrap());
        let mut reader2 = BufReader::new(stream2);
        reader2
            .get_mut()
            .write_all(req_line2.as_bytes())
            .await
            .unwrap();
        let mut resp_line2 = String::new();
        reader2.read_line(&mut resp_line2).await.unwrap();
        let resp2: agent_team_mail_core::daemon_client::SocketResponse =
            serde_json::from_str(resp_line2.trim()).unwrap();
        // session-query returns ok with the record, but alive should be false
        // (PID is from current process, but state is Dead).  We verify liveness
        // through the in-memory registry directly rather than the alive flag,
        // because alive checks the OS process table and our PID is real.
        assert!(resp2.is_ok(), "session-query failed: {:?}", resp2.error);
        {
            let reg = session_registry.lock().unwrap();
            let record = reg.query("team-lead").unwrap();
            assert_eq!(
                record.state,
                crate::daemon::session_registry::SessionState::Dead,
                "Session registry must reflect Dead state after session_end"
            );
        }

        // ── Step 3: Query agent-state — expects Killed ────────────────────────
        let stream3 = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        let request3 = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "end-roundtrip-3".to_string(),
            command: "agent-state".to_string(),
            payload: serde_json::json!({"agent": "team-lead", "team": "atm-dev"}),
        };
        let req_line3 = format!("{}\n", serde_json::to_string(&request3).unwrap());
        let mut reader3 = BufReader::new(stream3);
        reader3
            .get_mut()
            .write_all(req_line3.as_bytes())
            .await
            .unwrap();
        let mut resp_line3 = String::new();
        reader3.read_line(&mut resp_line3).await.unwrap();
        let resp3: agent_team_mail_core::daemon_client::SocketResponse =
            serde_json::from_str(resp_line3.trim()).unwrap();
        assert!(resp3.is_ok(), "agent-state failed: {:?}", resp3.error);
        let payload3 = resp3.payload.unwrap();
        assert_eq!(
            payload3["state"].as_str().unwrap(),
            "killed",
            "Agent state must be 'killed' after session_end"
        );

        cancel.cancel();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_socket_file_cleaned_up_on_drop() {
        use tokio_util::sync::CancellationToken;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let home_dir = temp_dir.path().to_path_buf();
        let cancel = CancellationToken::new();
        let state_store = make_store();

        let socket_path = home_dir.join(".claude/daemon/atm-daemon.sock");

        {
            let launch_tx = new_launch_sender();
            let _handle = start_socket_server(
                home_dir.clone(),
                state_store,
                new_pubsub_store(),
                launch_tx,
                make_sr(),
                cancel.clone(),
            )
            .await
            .unwrap()
            .expect("Expected handle");

            assert!(
                socket_path.exists(),
                "Socket should exist while handle is alive"
            );
        }
        // Handle dropped — socket should be gone
        assert!(
            !socket_path.exists(),
            "Socket should be removed after handle drop"
        );

        cancel.cancel();
    }

    // ── session-query handler tests ────────────────────────────────────────────

    #[test]
    fn test_session_query_missing_name_field() {
        let sr = make_sr();
        let req = make_request("session-query", serde_json::json!({}));
        let resp = handle_session_query(&req, &sr);
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "MISSING_PARAMETER");
    }

    #[test]
    fn test_session_query_agent_not_found() {
        let sr = make_sr();
        let req = make_request("session-query", serde_json::json!({"name": "ghost"}));
        let resp = handle_session_query(&req, &sr);
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "AGENT_NOT_FOUND");
    }

    #[test]
    #[cfg(unix)]
    fn test_session_query_agent_alive() {
        let sr = make_sr();
        {
            let mut reg = sr.lock().unwrap();
            reg.upsert("team-lead", "sess-abc123", std::process::id());
        }
        let req = make_request("session-query", serde_json::json!({"name": "team-lead"}));
        let resp = handle_session_query(&req, &sr);
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["session_id"].as_str().unwrap(), "sess-abc123");
        assert_eq!(
            payload["process_id"].as_u64().unwrap(),
            std::process::id() as u64
        );
        assert!(payload["alive"].as_bool().unwrap());
    }

    #[test]
    fn test_session_query_agent_dead_pid() {
        let sr = make_sr();
        {
            let mut reg = sr.lock().unwrap();
            // i32::MAX is an impossibly large PID; always dead
            reg.upsert("stale-agent", "sess-deadbeef", i32::MAX as u32);
        }
        let req = make_request("session-query", serde_json::json!({"name": "stale-agent"}));
        let resp = handle_session_query(&req, &sr);
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["session_id"].as_str().unwrap(), "sess-deadbeef");
        // alive is false because the PID doesn't exist (non-unix: always false)
        assert!(!payload["alive"].as_bool().unwrap());
    }
}
