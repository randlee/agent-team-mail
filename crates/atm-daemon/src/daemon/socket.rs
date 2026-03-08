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
use agent_team_mail_core::daemon_client::{
    CanonicalMemberState, GhMonitorControlRequest, GhMonitorHealth, GhMonitorLifecycleAction,
    GhMonitorRequest, GhMonitorStatus, GhMonitorTargetKind, GhStatusRequest, LaunchConfig,
    LaunchResult,
};
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::logging_event::LogEventV1;
use agent_team_mail_core::schema::{AgentMember, InboxMessage, TeamConfig};
use agent_team_mail_core::text::DEFAULT_MAX_MESSAGE_BYTES;
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tracing::{debug, error, info, warn};

use crate::daemon::log_writer::LogEventQueue;
use crate::daemon::pid_backend_validation::{
    PidBackendValidation, roster_process_id, validate_pid_backend, validate_pid_runtime,
};

use crate::daemon::dedup::{DedupeKey, DurableDedupeStore};
use crate::daemon::session_registry::{MarkDeadForSessionOutcome, SharedSessionRegistry};
use crate::plugins::worker_adapter::AgentState;

// ── Public API (cross-platform stubs) ────────────────────────────────────────

/// Shared durable dedupe store, threaded through the socket server.
///
/// Wraps a [`DurableDedupeStore`] in an `Arc<Mutex<_>>` so it can be
/// cloned cheaply and shared across connection-handler tasks.
pub type SharedDedupeStore = std::sync::Arc<std::sync::Mutex<DurableDedupeStore>>;

/// Create a new [`SharedDedupeStore`] from the given home directory.
///
/// Reads `ATM_DEDUP_CAPACITY` and `ATM_DEDUP_TTL_SECS` from the environment.
/// The backing file is `{home_dir}/.claude/daemon/dedup.jsonl`.
///
/// # Errors
///
/// Returns an error if the daemon directory cannot be created or the existing
/// backing file cannot be read.
pub fn new_dedup_store(home_dir: &std::path::Path) -> Result<SharedDedupeStore> {
    let store = DurableDedupeStore::from_env(home_dir)?;
    Ok(std::sync::Arc::new(std::sync::Mutex::new(store)))
}

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
/// * `dedup_store` - Shared durable dedupe store for idempotency across restarts.
///   Create with [`new_dedup_store()`].
/// * `stream_state_store` - Shared per-agent stream turn state store.
/// * `stream_event_sender` - Broadcast sender for push-based stream event fanout.
///   Create with [`new_stream_event_sender()`].
/// * `log_event_queue` - Bounded queue for incoming `"log-event"` commands.
///   Create with [`crate::daemon::new_log_event_queue()`].
/// * `cancel` - Cancellation token; server stops accepting when cancelled
///
/// # Platform Behaviour
///
/// On non-Unix platforms this function returns `Ok(None)` immediately.
#[expect(
    clippy::too_many_arguments,
    reason = "public entry point passes through all shared daemon resources"
)]
pub async fn start_socket_server(
    home_dir: PathBuf,
    state_store: SharedStateStore,
    pubsub_store: SharedPubSubStore,
    launch_tx: LaunchSender,
    session_registry: SharedSessionRegistry,
    dedup_store: SharedDedupeStore,
    stream_state_store: SharedStreamStateStore,
    stream_event_sender: SharedStreamEventSender,
    log_event_queue: LogEventQueue,
    _daemon_lock: &agent_team_mail_core::io::lock::FileLock,
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
            dedup_store,
            stream_state_store,
            stream_event_sender,
            log_event_queue,
            _daemon_lock,
            cancel,
        )
        .await
        .map(Some)
    }

    #[cfg(not(unix))]
    {
        let _ = log_event_queue;
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

// ── Stream state store ───────────────────────────────────────────────────────

/// Per-agent stream turn state, updated by `"stream-event"` socket commands.
///
/// Maps agent name to [`AgentStreamState`].
pub type SharedStreamStateStore = std::sync::Arc<
    std::sync::Mutex<
        std::collections::HashMap<String, agent_team_mail_core::daemon_stream::AgentStreamState>,
    >,
>;

/// Create a new, empty [`SharedStreamStateStore`].
pub fn new_stream_state_store() -> SharedStreamStateStore {
    std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()))
}

// ── Stream event broadcast channel ───────────────────────────────────────────

/// Sender half of the daemon's stream-event broadcast channel.
///
/// `atm-agent-mcp` transports send [`DaemonStreamEvent`](agent_team_mail_core::daemon_stream::DaemonStreamEvent)
/// to the daemon via the `"stream-event"` socket command. The daemon publishes
/// each received event on this channel so that any in-process subscriber
/// (e.g. `atm-tui` via a `"stream-subscribe"` connection) receives events
/// with low latency.
///
/// Capacity of 256 events: lagged subscribers receive an error and must
/// re-sync via `agent-stream-state`.
pub type SharedStreamEventSender = std::sync::Arc<
    tokio::sync::broadcast::Sender<agent_team_mail_core::daemon_stream::DaemonStreamEvent>,
>;

/// Create a new broadcast channel for stream events.
///
/// Returns an [`SharedStreamEventSender`] backed by a channel with capacity
/// 256.  Dropping the last [`tokio::sync::broadcast::Receiver`] does not
/// close the sender; the sender is kept alive by the `Arc`.
pub fn new_stream_event_sender() -> SharedStreamEventSender {
    let (tx, _rx) = tokio::sync::broadcast::channel(256);
    std::sync::Arc::new(tx)
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
#[expect(
    clippy::too_many_arguments,
    reason = "socket server startup requires shared daemon resources passed from main"
)]
async fn start_unix_socket_server(
    home_dir: PathBuf,
    state_store: SharedStateStore,
    pubsub_store: SharedPubSubStore,
    launch_tx: LaunchSender,
    session_registry: SharedSessionRegistry,
    dedup_store: SharedDedupeStore,
    stream_state_store: SharedStreamStateStore,
    stream_event_sender: SharedStreamEventSender,
    log_event_queue: LogEventQueue,
    _daemon_lock: &agent_team_mail_core::io::lock::FileLock,
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
            home_dir,
            state_store,
            pubsub_store,
            launch_tx,
            session_registry,
            dedup_store,
            stream_state_store,
            stream_event_sender,
            log_event_queue,
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
    home_dir: std::path::PathBuf,
    state_store: SharedStateStore,
    pubsub_store: SharedPubSubStore,
    launch_tx: LaunchSender,
    session_registry: SharedSessionRegistry,
    dedup_store: SharedDedupeStore,
    stream_state_store: SharedStreamStateStore,
    stream_event_sender: SharedStreamEventSender,
    log_event_queue: LogEventQueue,
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
                        let home = home_dir.clone();
                        let store = state_store.clone();
                        let ps = pubsub_store.clone();
                        let tx = launch_tx.clone();
                        let sr = session_registry.clone();
                        let dd = dedup_store.clone();
                        let ss = stream_state_store.clone();
                        let ses = stream_event_sender.clone();
                        let leq = log_event_queue.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, home, store, ps, tx, sr, dd, ss, ses, leq).await {
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
#[expect(
    clippy::too_many_arguments,
    reason = "connection handler needs all shared daemon resources for command dispatch"
)]
async fn handle_connection(
    stream: tokio::net::UnixStream,
    home: std::path::PathBuf,
    state_store: SharedStateStore,
    pubsub_store: SharedPubSubStore,
    launch_tx: LaunchSender,
    session_registry: SharedSessionRegistry,
    dedup_store: SharedDedupeStore,
    stream_state_store: SharedStreamStateStore,
    stream_event_sender: SharedStreamEventSender,
    log_event_queue: LogEventQueue,
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

    // Long-lived "stream-subscribe" connections are handled before the normal
    // one-shot request/response path.
    if is_stream_subscribe_command(request_str) {
        let mut stream = reader.into_inner();
        handle_stream_subscribe(&mut stream, request_str, &stream_event_sender).await;
        return Ok(());
    }

    // Check whether this is a launch command before sync dispatch so we can
    // use async channel communication with the WorkerAdapterPlugin.
    let response = if is_launch_command(request_str) {
        handle_launch_command(request_str, &launch_tx).await
    } else if is_gh_monitor_command(request_str) {
        handle_gh_monitor_command(request_str, &home).await
    } else if is_gh_monitor_control_command(request_str) {
        handle_gh_monitor_control_command(request_str, &home).await
    } else if is_gh_monitor_health_command(request_str) {
        handle_gh_monitor_health_command(request_str, &home).await
    } else if is_gh_status_command(request_str) {
        handle_gh_status_command(request_str, &home).await
    } else if is_control_command(request_str) {
        handle_control_command(
            request_str,
            &home,
            &state_store,
            &session_registry,
            &dedup_store,
        )
        .await
    } else if is_hook_event_command(request_str) {
        handle_hook_event_command_with_dedup(
            request_str,
            &state_store,
            &session_registry,
            &dedup_store,
        )
        .await
    } else if is_stream_event_command(request_str) {
        handle_stream_event_command(request_str, &stream_state_store, &stream_event_sender).await
    } else if is_log_event_command(request_str) {
        handle_log_event_command(request_str, &log_event_queue).await
    } else {
        match parse_and_dispatch(
            request_str,
            &state_store,
            &pubsub_store,
            &session_registry,
            &stream_state_store,
        ) {
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

/// Quickly determine if a raw JSON line is a `"gh-monitor"` command.
#[cfg(unix)]
fn is_gh_monitor_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"gh-monitor""#)
        || request_str.contains(r#""command": "gh-monitor""#)
}

/// Quickly determine if a raw JSON line is a `"gh-status"` command.
#[cfg(unix)]
fn is_gh_status_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"gh-status""#)
        || request_str.contains(r#""command": "gh-status""#)
}

/// Quickly determine if a raw JSON line is a `"gh-monitor-control"` command.
#[cfg(unix)]
fn is_gh_monitor_control_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"gh-monitor-control""#)
        || request_str.contains(r#""command": "gh-monitor-control""#)
}

/// Quickly determine if a raw JSON line is a `"gh-monitor-health"` command.
#[cfg(unix)]
fn is_gh_monitor_health_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"gh-monitor-health""#)
        || request_str.contains(r#""command": "gh-monitor-health""#)
}

/// Quickly determine if a raw JSON line is a `"hook-event"` command.
#[cfg(unix)]
fn is_hook_event_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"hook-event""#)
        || request_str.contains(r#""command": "hook-event""#)
}

/// Quickly determine if a raw JSON line is a `"stream-event"` command.
#[cfg(unix)]
fn is_stream_event_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"stream-event""#)
        || request_str.contains(r#""command": "stream-event""#)
}

/// Quickly determine if a raw JSON line is a `"stream-subscribe"` command.
#[cfg(unix)]
fn is_stream_subscribe_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"stream-subscribe""#)
        || request_str.contains(r#""command": "stream-subscribe""#)
}

/// Quickly determine if a raw JSON line is a `"log-event"` command.
#[cfg(unix)]
fn is_log_event_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"log-event""#)
        || request_str.contains(r#""command": "log-event""#)
}

/// Handle a `"stream-subscribe"` command: long-lived connection that streams
/// [`DaemonStreamEvent`]s to the caller via the broadcast channel.
///
/// Sends an initial ACK line `{"version":1,"status":"ok","streaming":true}`,
/// then writes one JSON line per received event until the client disconnects
/// or the broadcast sender is closed.
///
/// An optional `agent` field in the request payload filters events to a single
/// agent; when absent all events are forwarded.
///
/// Lagged subscribers (more than 256 unconsumed events) receive a warning log
/// and continue — they will miss events but the connection stays open.
#[cfg(unix)]
async fn handle_stream_subscribe(
    stream: &mut tokio::net::UnixStream,
    request_str: &str,
    stream_event_sender: &SharedStreamEventSender,
) {
    use tokio::io::AsyncWriteExt;

    // Parse optional agent filter from payload.
    let agent_filter: Option<String> = serde_json::from_str::<serde_json::Value>(request_str)
        .ok()
        .and_then(|v| {
            v.get("payload")
                .and_then(|p| p.get("agent"))
                .and_then(|a| a.as_str().map(str::to_string))
        });

    // Send initial ACK so the subscriber knows the channel is live.
    let ack = serde_json::json!({"version": 1, "status": "ok", "streaming": true});
    let ack_line = format!("{ack}\n");
    if stream.write_all(ack_line.as_bytes()).await.is_err() {
        return; // Client already gone.
    }
    if stream.flush().await.is_err() {
        return;
    }

    // Subscribe to the broadcast channel.
    let mut rx = stream_event_sender.subscribe();

    loop {
        match rx.recv().await {
            Ok(event) => {
                // Apply optional agent filter.
                let matches = match &agent_filter {
                    Some(filter) => event.agent() == filter,
                    None => true,
                };
                if !matches {
                    continue;
                }
                match serde_json::to_string(&event) {
                    Ok(line) => {
                        let line = format!("{line}\n");
                        if stream.write_all(line.as_bytes()).await.is_err() {
                            break; // Client disconnected.
                        }
                        if stream.flush().await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("stream-subscribe: failed to serialize event: {e}");
                    }
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                debug!("stream-subscribe: broadcast channel closed");
                break;
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                warn!(
                    "stream-subscribe: lagged by {n} events; subscriber must re-sync via agent-stream-state"
                );
                // Continue — subscriber misses events but stays connected.
            }
        }
    }
}

/// Handle a `"stream-event"` command: parse the [`DaemonStreamEvent`], validate
/// sender authorization, update the per-agent stream state store, publish to
/// the broadcast channel, and return an `{ok: true}` response.
///
/// # Authorization (arch-ctm review finding)
///
/// Extracts the `team` field from the request payload and validates that the
/// agent identified in the event is a member of that team (using the same
/// team config lookup as [`authorize_hook_event`]).  This prevents local
/// processes from spoofing agent stream state for arbitrary teams.
#[cfg(unix)]
async fn handle_stream_event_command(
    request_str: &str,
    stream_state_store: &SharedStreamStateStore,
    stream_event_sender: &SharedStreamEventSender,
) -> SocketResponse {
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};
    use agent_team_mail_core::daemon_stream::{AgentStreamState, DaemonStreamEvent};

    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse stream-event request: {e}"),
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

    // Clone payload before consuming it for deserialization, so we can
    // read the "team" field for authorization afterwards.
    let payload_raw = request.payload.clone();

    let event: DaemonStreamEvent = match serde_json::from_value(request.payload) {
        Ok(e) => e,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                "INVALID_PAYLOAD",
                &format!("Failed to parse DaemonStreamEvent: {e}"),
            );
        }
    };

    // Authorization: verify that the agent in the event is a team member.
    // Extract team from the payload's "team" field (same pattern as hook-event).
    let agent = AgentStreamState::agent_from_event(&event).to_string();
    if let Some(team) = payload_raw.get("team").and_then(|v| v.as_str()) {
        if let Err(reason) = authorize_hook_event(
            team,
            &agent,
            agent_team_mail_core::daemon_client::LifecycleSourceKind::Unknown,
        ) {
            warn!(
                agent = %agent,
                team = %team,
                reason = %reason,
                "stream-event rejected: sender authorization failed"
            );
            return make_error_response(
                &request.request_id,
                "UNAUTHORIZED",
                &format!("stream-event sender not authorized: {reason}"),
            );
        }
    }
    // Note: if no "team" field is present, we allow the event through — this
    // matches the behavior of internal emitters (stream_emit.rs) which may
    // not include a team field.  The daemon socket is already local-only (Unix
    // domain socket), so the auth gate above is defense-in-depth.

    // Update the per-agent stream state.
    {
        let mut store = stream_state_store.lock().unwrap();
        let state = store.entry(agent).or_default();
        state.apply(&event);
    }

    // Append observability summaries to structured logs for non-turn events.
    match &event {
        DaemonStreamEvent::StreamError {
            agent_id,
            session_id,
            error_summary,
        } => {
            agent_team_mail_core::event_log::emit_event_best_effort(
                agent_team_mail_core::event_log::EventFields {
                    level: "warn",
                    source: "atm-daemon",
                    action: "stream_error_summary",
                    session_id: Some(session_id.clone()),
                    agent_id: Some(agent_id.clone()),
                    error: Some(error_summary.clone()),
                    ..Default::default()
                },
            );
        }
        DaemonStreamEvent::DroppedCounters {
            agent_id,
            dropped,
            unknown,
        } => {
            agent_team_mail_core::event_log::emit_event_best_effort(
                agent_team_mail_core::event_log::EventFields {
                    level: "info",
                    source: "atm-daemon",
                    action: "stream_dropped_counters",
                    agent_id: Some(agent_id.clone()),
                    result: Some(format!("dropped={dropped},unknown={unknown}")),
                    count: Some(dropped.saturating_add(*unknown)),
                    ..Default::default()
                },
            );
        }
        _ => {}
    }

    // Publish to the broadcast channel for push-based subscribers.
    // Ignore send errors — no active subscribers is the normal steady state.
    let _ = stream_event_sender.send(event.clone());

    debug!("stream-event processed: {event:?}");

    make_ok_response(&request.request_id, serde_json::json!({"ok": true}))
}

/// Handle a `"log-event"` command.
///
/// Parses the [`LogEventV1`] from the socket request payload, validates it,
/// redacts sensitive fields, and enqueues it in the bounded log event queue.
///
/// # Response payload
///
/// - On success: `{"accepted": true}`
/// - On queue full: `{"accepted": false, "error": "QUEUE_FULL"}`
/// - On validation failure: error response with code `INVALID_PAYLOAD`
/// - On version mismatch: error response with code `VERSION_MISMATCH`
/// - On parse failure: error response with code `INVALID_PAYLOAD`
#[cfg(unix)]
async fn handle_log_event_command(
    request_str: &str,
    log_event_queue: &LogEventQueue,
) -> SocketResponse {
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};

    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                "unknown",
                "INVALID_PAYLOAD",
                &format!("Failed to parse log-event request: {e}"),
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

    let mut event: LogEventV1 = match serde_json::from_value(request.payload) {
        Ok(e) => e,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                "INVALID_PAYLOAD",
                &format!("Failed to parse LogEventV1: {e}"),
            );
        }
    };

    if event.v != 1 {
        return make_error_response(
            &request.request_id,
            "VERSION_MISMATCH",
            &format!(
                "Unsupported log event schema version {}; expected 1",
                event.v
            ),
        );
    }

    if let Err(e) = event.validate() {
        return make_error_response(
            &request.request_id,
            "INVALID_PAYLOAD",
            &format!("Log event validation failed: {e}"),
        );
    }

    // Redact sensitive fields before enqueueing.
    event.redact();

    let accepted = {
        let mut q = log_event_queue.lock().await;
        q.push(event)
    };

    if accepted {
        make_ok_response(&request.request_id, serde_json::json!({"accepted": true}))
    } else {
        make_ok_response(
            &request.request_id,
            serde_json::json!({"accepted": false, "error": "QUEUE_FULL"}),
        )
    }
}

#[cfg(unix)]
struct HookEventAuth {
    is_team_lead: bool,
    /// Resolved lifecycle source, extracted from the `source` payload field.
    ///
    /// Defaults to [`LifecycleSourceKind::Unknown`] when the field is absent.
    source: agent_team_mail_core::daemon_client::LifecycleSourceKind,
}

/// Resolve and authorize hook-event sender identity against team config.
///
/// Returns [`HookEventAuth`] containing:
/// - `is_team_lead`: whether `agent` is the configured team lead
/// - `source`: the resolved [`LifecycleSourceKind`] from the payload (or
///   [`Unknown`](agent_team_mail_core::daemon_client::LifecycleSourceKind::Unknown)
///   when the field is absent)
#[cfg(unix)]
fn authorize_hook_event(
    team: &str,
    agent: &str,
    source: agent_team_mail_core::daemon_client::LifecycleSourceKind,
) -> std::result::Result<HookEventAuth, String> {
    let home_dir = agent_team_mail_core::home::get_home_dir()
        .map_err(|e| format!("failed to resolve home directory: {e}"))?;

    let config_path = home_dir
        .join(".claude/teams")
        .join(team)
        .join("config.json");
    let content = std::fs::read_to_string(&config_path)
        .map_err(|_| format!("team config not found: {}", config_path.display()))?;
    let config: TeamConfig =
        serde_json::from_str(&content).map_err(|e| format!("invalid team config: {e}"))?;

    // Support either canonical agent_id or bare name.
    let expected_agent_id = format!("{agent}@{team}");
    let Some(member) = config
        .members
        .iter()
        .find(|m| m.name == agent || m.agent_id == expected_agent_id)
    else {
        return Err("agent not in team".to_string());
    };

    let is_team_lead = member.agent_id == config.lead_agent_id;
    Ok(HookEventAuth {
        is_team_lead,
        source,
    })
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct TransitionEventSpec {
    level: &'static str,
    action: &'static str,
    old: String,
    new: String,
    reason: String,
}

#[cfg(unix)]
fn is_online_state(state: Option<AgentState>) -> bool {
    matches!(state, Some(AgentState::Active | AgentState::Idle))
}

#[cfg(unix)]
fn online_state_label(state: Option<AgentState>) -> &'static str {
    if is_online_state(state) {
        "Online"
    } else {
        "Offline"
    }
}

#[cfg(unix)]
fn activity_label(state: AgentState) -> Option<&'static str> {
    match state {
        AgentState::Active => Some("Busy"),
        AgentState::Idle => Some("Idle"),
        AgentState::Offline | AgentState::Unknown => None,
    }
}

#[cfg(unix)]
fn collect_member_transition_events(
    old_state: Option<AgentState>,
    new_state: AgentState,
    reason: &str,
) -> Vec<TransitionEventSpec> {
    if old_state == Some(new_state) {
        return Vec::new();
    }

    let mut specs = Vec::new();
    if is_online_state(old_state) != is_online_state(Some(new_state)) {
        specs.push(TransitionEventSpec {
            level: "info",
            action: "member_state_change",
            old: online_state_label(old_state).to_string(),
            new: online_state_label(Some(new_state)).to_string(),
            reason: reason.to_string(),
        });
    }

    let old_activity = old_state.and_then(activity_label);
    let new_activity = activity_label(new_state);
    if let (Some(old), Some(new)) = (old_activity, new_activity)
        && old != new
    {
        specs.push(TransitionEventSpec {
            level: "debug",
            action: "member_activity_change",
            old: old.to_string(),
            new: new.to_string(),
            reason: reason.to_string(),
        });
    }

    specs
}

#[cfg(unix)]
fn emit_member_transition_events(
    team: &str,
    agent: &str,
    old_state: Option<AgentState>,
    new_state: AgentState,
    reason: &str,
    session_id: Option<&str>,
    process_id: Option<u32>,
) {
    for spec in collect_member_transition_events(old_state, new_state, reason) {
        let mut extra_fields = serde_json::Map::new();
        extra_fields.insert("old".to_string(), serde_json::Value::String(spec.old));
        extra_fields.insert("new".to_string(), serde_json::Value::String(spec.new));
        extra_fields.insert(
            "source".to_string(),
            serde_json::Value::String("daemon".to_string()),
        );
        extra_fields.insert("reason".to_string(), serde_json::Value::String(spec.reason));
        if let Some(pid) = process_id {
            extra_fields.insert("pid".to_string(), serde_json::Value::Number(pid.into()));
        }

        emit_event_best_effort(EventFields {
            level: spec.level,
            source: "atm-daemon",
            action: spec.action,
            team: Some(team.to_string()),
            agent_name: Some(agent.to_string()),
            session_id: session_id
                .map(str::trim)
                .filter(|sid| !sid.is_empty())
                .map(ToString::to_string),
            result: Some("success".to_string()),
            extra_fields,
            ..Default::default()
        });
    }
}

#[cfg(unix)]
fn emit_session_identity_change_events(
    team: &str,
    agent: &str,
    old_record: Option<&crate::daemon::session_registry::SessionRecord>,
    new_session_id: &str,
    new_process_id: Option<u32>,
    reason: &str,
) {
    let old_session_id = old_record.map(|record| record.session_id.as_str());
    let (session_changed, process_changed) =
        session_identity_change_flags(old_record, new_session_id, new_process_id);

    if session_changed {
        let mut extra_fields = serde_json::Map::new();
        extra_fields.insert(
            "old".to_string(),
            old_session_id
                .map(|sid| serde_json::Value::String(sid.to_string()))
                .unwrap_or(serde_json::Value::Null),
        );
        extra_fields.insert(
            "new".to_string(),
            serde_json::Value::String(new_session_id.to_string()),
        );
        extra_fields.insert(
            "source".to_string(),
            serde_json::Value::String("daemon".to_string()),
        );
        extra_fields.insert(
            "reason".to_string(),
            serde_json::Value::String(reason.to_string()),
        );
        if let Some(pid) = new_process_id {
            extra_fields.insert("pid".to_string(), serde_json::Value::Number(pid.into()));
        }

        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm-daemon",
            action: "session_id_change",
            team: Some(team.to_string()),
            agent_name: Some(agent.to_string()),
            session_id: Some(new_session_id.to_string()),
            result: Some("success".to_string()),
            extra_fields,
            ..Default::default()
        });
    }

    if process_changed && let Some(new_pid) = new_process_id {
        let old_pid = old_record.map(|record| record.process_id);
        let mut extra_fields = serde_json::Map::new();
        extra_fields.insert(
            "old".to_string(),
            old_pid
                .map(|pid| serde_json::Value::Number(pid.into()))
                .unwrap_or(serde_json::Value::Null),
        );
        extra_fields.insert("new".to_string(), serde_json::Value::Number(new_pid.into()));
        extra_fields.insert(
            "source".to_string(),
            serde_json::Value::String("daemon".to_string()),
        );
        extra_fields.insert(
            "reason".to_string(),
            serde_json::Value::String(reason.to_string()),
        );

        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm-daemon",
            action: "process_id_change",
            team: Some(team.to_string()),
            agent_name: Some(agent.to_string()),
            session_id: Some(new_session_id.to_string()),
            result: Some("success".to_string()),
            extra_fields,
            ..Default::default()
        });
    }
}

#[cfg(unix)]
fn session_identity_change_flags(
    old_record: Option<&crate::daemon::session_registry::SessionRecord>,
    new_session_id: &str,
    new_process_id: Option<u32>,
) -> (bool, bool) {
    let session_changed =
        old_record.map(|record| record.session_id.as_str()) != Some(new_session_id);
    let process_changed = new_process_id
        .is_some_and(|new_pid| old_record.map(|record| record.process_id) != Some(new_pid));
    (session_changed, process_changed)
}

#[cfg(unix)]
fn hook_action_name(event_type: &str) -> Option<&'static str> {
    match event_type {
        "session_start" => Some("hook.session_start"),
        "permission_request" => Some("hook.permission_request"),
        "stop" => Some("hook.stop"),
        "notification_idle_prompt" => Some("hook.notification_idle_prompt"),
        "pre_compact" => Some("hook.pre_compact"),
        "compact_complete" => Some("hook.compact_complete"),
        "session_end" => Some("hook.session_end"),
        _ => None,
    }
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct HookLogContext<'a> {
    team: Option<&'a str>,
    agent: Option<&'a str>,
    session_id: Option<&'a str>,
    process_id: Option<u32>,
}

#[cfg(unix)]
fn emit_hook_event(
    level: &'static str,
    action: &'static str,
    ctx: HookLogContext<'_>,
    outcome: &str,
    error: Option<String>,
    event_type: Option<&str>,
) {
    let mut extra_fields = serde_json::Map::new();
    extra_fields.insert(
        "source".to_string(),
        serde_json::Value::String("hook".to_string()),
    );
    if let Some(pid) = ctx.process_id {
        extra_fields.insert("pid".to_string(), serde_json::Value::Number(pid.into()));
    }
    if let Some(event_type) = event_type {
        extra_fields.insert(
            "event".to_string(),
            serde_json::Value::String(event_type.to_string()),
        );
    }

    emit_event_best_effort(EventFields {
        level,
        source: "atm-daemon",
        action,
        team: ctx
            .team
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
        agent_name: ctx
            .agent
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
        session_id: ctx
            .session_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
        result: Some(outcome.to_string()),
        error,
        extra_fields,
        ..Default::default()
    });
}

#[cfg(unix)]
fn emit_hook_success(
    event_type: &str,
    team: &str,
    agent: &str,
    session_id: Option<&str>,
    process_id: Option<u32>,
) {
    if let Some(action) = hook_action_name(event_type) {
        let ctx = HookLogContext {
            team: Some(team),
            agent: Some(agent),
            session_id,
            process_id,
        };
        emit_hook_event("info", action, ctx, "success", None, Some(event_type));
    }
}

#[cfg(unix)]
fn emit_hook_failure(
    event_type: Option<&str>,
    team: Option<&str>,
    agent: Option<&str>,
    session_id: Option<&str>,
    process_id: Option<u32>,
    reason: &str,
) {
    let ctx = HookLogContext {
        team,
        agent,
        session_id,
        process_id,
    };
    emit_hook_event(
        "warn",
        "hook.failure",
        ctx,
        "failure",
        Some(reason.to_string()),
        event_type,
    );
}

/// Handle the `"hook-event"` command, updating daemon state in real-time
/// from Claude Code lifecycle hooks (session_start, teammate_idle, session_end).
#[cfg(unix)]
async fn handle_hook_event_command_with_dedup(
    request_str: &str,
    state_store: &SharedStateStore,
    session_registry: &SharedSessionRegistry,
    dedup_store: &SharedDedupeStore,
) -> SocketResponse {
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};

    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("bad hook-event: {e}"),
            );
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
    let team = request
        .payload
        .get("team")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let process_id = request
        .payload
        .get("process_id")
        .and_then(|v| v.as_u64())
        .map(|p| p as u32);

    // Extract optional `source` field; default to Unknown for backward compat.
    let source_kind = request
        .payload
        .get("source")
        .and_then(|v| {
            serde_json::from_value::<agent_team_mail_core::daemon_client::LifecycleSource>(
                v.clone(),
            )
            .ok()
        })
        .map(|s| s.kind)
        .unwrap_or(agent_team_mail_core::daemon_client::LifecycleSourceKind::Unknown);

    if agent.is_empty() {
        emit_hook_failure(
            Some(event_type.as_str()),
            Some(team.as_str()),
            None,
            Some(session_id.as_str()),
            process_id,
            "missing agent",
        );
        return make_ok_response(
            &request.request_id,
            serde_json::json!({"processed": false, "reason": "missing agent"}),
        );
    }
    if team.is_empty() {
        emit_hook_failure(
            Some(event_type.as_str()),
            None,
            Some(agent.as_str()),
            Some(session_id.as_str()),
            process_id,
            "missing team",
        );
        return make_ok_response(
            &request.request_id,
            serde_json::json!({"processed": false, "reason": "missing team"}),
        );
    }

    let auth = match authorize_hook_event(&team, &agent, source_kind) {
        Ok(auth) => auth,
        Err(reason) => {
            emit_hook_failure(
                Some(event_type.as_str()),
                Some(team.as_str()),
                Some(agent.as_str()),
                Some(session_id.as_str()),
                process_id,
                &reason,
            );
            return make_ok_response(
                &request.request_id,
                serde_json::json!({"processed": false, "reason": reason}),
            );
        }
    };

    // Determine whether the source requires team-lead restriction on
    // session_end. `session_start` must be accepted for all known team members
    // so spawned teammates can register runtime sessions.
    // `claude_hook` and `unknown` remain strictest for termination semantics;
    // `atm_mcp` and `agent_hook` relax the restriction because those adapters
    // manage their own agent sessions.
    use agent_team_mail_core::daemon_client::LifecycleSourceKind;
    let require_lead_for_session_end = matches!(
        auth.source,
        LifecycleSourceKind::ClaudeHook | LifecycleSourceKind::Unknown
    );

    // Deduplicate lifecycle hook deliveries before any mutable state changes.
    // Some adapters may retry on transport failures using the same request_id.
    let request_id = request.request_id.trim();
    if !request_id.is_empty() {
        let session_key = if session_id.trim().is_empty() {
            "_"
        } else {
            session_id.trim()
        };
        let key = DedupeKey::new(&team, session_key, &agent, request_id);
        if dedup_store.lock().unwrap().check_and_insert(key) {
            info!(
                event = %event_type,
                team = %team,
                agent = %agent,
                request_id = %request_id,
                "hook_event duplicate delivery ignored"
            );
            return make_ok_response(
                &request.request_id,
                serde_json::json!({
                    "processed": true,
                    "duplicate": true,
                    "event": event_type,
                    "agent": agent
                }),
            );
        }
    }

    let agent_pid = process_id.unwrap_or(0);

    match event_type.as_str() {
        "session_start" => {
            if session_id.is_empty() {
                emit_hook_failure(
                    Some(event_type.as_str()),
                    Some(team.as_str()),
                    Some(agent.as_str()),
                    None,
                    process_id,
                    "missing session_id",
                );
                return make_ok_response(
                    &request.request_id,
                    serde_json::json!({"processed": false, "reason": "missing session_id"}),
                );
            }
            let home = match agent_team_mail_core::home::get_home_dir() {
                Ok(h) => h,
                Err(e) => {
                    let reason = format!("home resolution failed: {e}");
                    emit_hook_failure(
                        Some(event_type.as_str()),
                        Some(team.as_str()),
                        Some(agent.as_str()),
                        Some(session_id.as_str()),
                        process_id,
                        &reason,
                    );
                    return make_ok_response(
                        &request.request_id,
                        serde_json::json!({"processed": false, "reason": reason}),
                    );
                }
            };
            let Some(member) = load_team_member(&home, &team, &agent) else {
                emit_hook_failure(
                    Some(event_type.as_str()),
                    Some(team.as_str()),
                    Some(agent.as_str()),
                    Some(session_id.as_str()),
                    process_id,
                    "agent not in team",
                );
                return make_ok_response(
                    &request.request_id,
                    serde_json::json!({"processed": false, "reason": "agent not in team"}),
                );
            };
            let previous_session_record = session_registry
                .lock()
                .unwrap()
                .query_for_team(&team, &agent)
                .cloned();
            let has_existing_session = previous_session_record.is_some();
            let has_activity_hint = member.is_active == Some(true)
                || member.last_active.is_some()
                || member
                    .session_id
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|s| !s.is_empty());
            if !has_existing_session && has_activity_hint {
                let msg = format!(
                    "ACTIVE_WITHOUT_SESSION: agent='{}' had activity hint before session_start registration (isActive={:?}, lastActive={:?}, sessionHint={:?})",
                    agent, member.is_active, member.last_active, member.session_id
                );
                warn!("{msg}");
                emit_event_best_effort(EventFields {
                    level: "warn",
                    source: "atm-daemon",
                    action: "ACTIVE_WITHOUT_SESSION",
                    team: Some(team.clone()),
                    agent_name: Some(agent.clone()),
                    result: Some("registration".to_string()),
                    error: Some(msg),
                    ..Default::default()
                });
            }
            if agent_pid > 1 {
                let validation = validate_pid_backend(&member, agent_pid);
                if validation.is_alive_mismatch() {
                    emit_pid_process_mismatch(&team, &agent, &validation, "registration");
                    {
                        let mut tracker = state_store.lock().unwrap();
                        if tracker.get_state(&agent).is_none() {
                            tracker.register_agent(&agent);
                        }
                        tracker.set_state_with_context(
                            &agent,
                            AgentState::Offline,
                            &format!(
                                "pid/backend mismatch: backend='{}' expected='{}' actual='{}' pid={}",
                                validation.backend,
                                validation.expected_display(),
                                validation.actual_display(),
                                validation.pid
                            ),
                            "pid_backend_validation",
                        );
                    }
                    let reason = format!(
                        "pid/backend mismatch: backend='{}' expected='{}' actual='{}' pid={}",
                        validation.backend,
                        validation.expected_display(),
                        validation.actual_display(),
                        validation.pid
                    );
                    emit_hook_failure(
                        Some(event_type.as_str()),
                        Some(team.as_str()),
                        Some(agent.as_str()),
                        Some(session_id.as_str()),
                        process_id,
                        &reason,
                    );
                    return make_ok_response(
                        &request.request_id,
                        serde_json::json!({
                            "processed": false,
                            "reason": reason
                        }),
                    );
                }
            }
            session_registry
                .lock()
                .unwrap()
                .upsert_for_team(&team, &agent, &session_id, agent_pid);
            let (old_state, new_state) = {
                let mut tracker = state_store.lock().unwrap();
                let current = tracker.get_state(&agent);
                if current.is_none() {
                    tracker.register_agent(&agent);
                    tracker.set_state_with_context(
                        &agent,
                        AgentState::Active,
                        "session_start lifecycle",
                        "hook_event",
                    );
                } else if matches!(current, Some(AgentState::Offline | AgentState::Unknown)) {
                    tracker.set_state_with_context(
                        &agent,
                        AgentState::Active,
                        "session_start lifecycle revive",
                        "hook_event",
                    );
                }
                let updated = tracker.get_state(&agent).unwrap_or(AgentState::Unknown);
                (current, updated)
            };
            emit_session_identity_change_events(
                &team,
                &agent,
                previous_session_record.as_ref(),
                &session_id,
                process_id,
                "hook_event.session_start",
            );
            emit_member_transition_events(
                &team,
                &agent,
                old_state,
                new_state,
                "hook_event.session_start",
                Some(session_id.as_str()),
                process_id,
            );
            emit_hook_success(
                event_type.as_str(),
                &team,
                &agent,
                Some(session_id.as_str()),
                process_id,
            );
            info!(agent = %agent, agent_pid = agent_pid, session_id = %session_id, "hook_event.session_start");
        }
        "pre_compact" | "compact_complete" => {
            emit_hook_success(
                event_type.as_str(),
                &team,
                &agent,
                Some(session_id.as_str()),
                process_id,
            );
            info!(
                agent = %agent,
                team = %team,
                session_id = %session_id,
                "hook_event {}",
                event_type
            );
        }
        "permission_request" => {
            let (old_state, new_state) = {
                let mut tracker = state_store.lock().unwrap();
                let current = tracker.get_state(&agent);
                if current.is_some() {
                    tracker.set_state_with_context(
                        &agent,
                        AgentState::Active,
                        "permission_request lifecycle (blocked-permission)",
                        "hook_event",
                    );
                } else {
                    tracker.register_agent(&agent);
                    tracker.set_state_with_context(
                        &agent,
                        AgentState::Active,
                        "permission_request lifecycle (blocked-permission, auto-register)",
                        "hook_event",
                    );
                }
                let updated = tracker.get_state(&agent).unwrap_or(AgentState::Unknown);
                (current, updated)
            };
            emit_member_transition_events(
                &team,
                &agent,
                old_state,
                new_state,
                "hook_event.permission_request",
                Some(session_id.as_str()),
                process_id,
            );
            info!(agent = %agent, agent_pid = agent_pid, "hook_event permission_request");
        }
        "stop" => {
            let (old_state, new_state) = {
                let mut tracker = state_store.lock().unwrap();
                let current = tracker.get_state(&agent);
                if current.is_some() {
                    tracker.set_state_with_context(
                        &agent,
                        AgentState::Idle,
                        "stop lifecycle",
                        "hook_event",
                    );
                } else {
                    tracker.register_agent(&agent);
                    tracker.set_state_with_context(
                        &agent,
                        AgentState::Idle,
                        "stop lifecycle (auto-register)",
                        "hook_event",
                    );
                }
                let updated = tracker.get_state(&agent).unwrap_or(AgentState::Unknown);
                (current, updated)
            };
            emit_member_transition_events(
                &team,
                &agent,
                old_state,
                new_state,
                "hook_event.stop",
                Some(session_id.as_str()),
                process_id,
            );
            info!(agent = %agent, agent_pid = agent_pid, "hook_event stop");
        }
        "notification_idle_prompt" => {
            let (old_state, new_state) = {
                let mut tracker = state_store.lock().unwrap();
                let current = tracker.get_state(&agent);
                if current.is_some() {
                    tracker.set_state_with_context(
                        &agent,
                        AgentState::Idle,
                        "notification_idle_prompt lifecycle",
                        "hook_event",
                    );
                } else {
                    tracker.register_agent(&agent);
                    tracker.set_state_with_context(
                        &agent,
                        AgentState::Idle,
                        "notification_idle_prompt lifecycle (auto-register)",
                        "hook_event",
                    );
                }
                let updated = tracker.get_state(&agent).unwrap_or(AgentState::Unknown);
                (current, updated)
            };
            emit_member_transition_events(
                &team,
                &agent,
                old_state,
                new_state,
                "hook_event.notification_idle_prompt",
                Some(session_id.as_str()),
                process_id,
            );
            info!(agent = %agent, agent_pid = agent_pid, "hook_event notification_idle_prompt");
        }
        "teammate_idle" => {
            let (old_state, new_state) = {
                let mut tracker = state_store.lock().unwrap();
                let current = tracker.get_state(&agent);
                if current.is_some() {
                    tracker.set_state_with_context(
                        &agent,
                        AgentState::Idle,
                        "teammate_idle lifecycle",
                        "hook_event",
                    );
                } else {
                    tracker.register_agent(&agent);
                    tracker.set_state_with_context(
                        &agent,
                        AgentState::Idle,
                        "teammate_idle lifecycle (auto-register)",
                        "hook_event",
                    );
                }
                let updated = tracker.get_state(&agent).unwrap_or(AgentState::Unknown);
                (current, updated)
            };
            emit_member_transition_events(
                &team,
                &agent,
                old_state,
                new_state,
                "hook_event.teammate_idle",
                Some(session_id.as_str()),
                process_id,
            );
            info!(agent = %agent, agent_pid = agent_pid, "hook_event teammate_idle");
        }
        "session_end" => {
            let mark_dead_outcome = if session_id.trim().is_empty() {
                MarkDeadForSessionOutcome::UnknownSession
            } else {
                let current_record = {
                    let registry = session_registry.lock().unwrap();
                    registry.query_for_team(&team, &agent).cloned()
                };
                match current_record {
                    None => MarkDeadForSessionOutcome::UnknownSession,
                    Some(record) if record.session_id != session_id => {
                        MarkDeadForSessionOutcome::SessionMismatch {
                            current_session_id: record.session_id,
                        }
                    }
                    Some(record)
                        if record.state == crate::daemon::session_registry::SessionState::Dead =>
                    {
                        MarkDeadForSessionOutcome::AlreadyDead
                    }
                    Some(_) => {
                        if require_lead_for_session_end && !auth.is_team_lead {
                            emit_hook_failure(
                                Some(event_type.as_str()),
                                Some(team.as_str()),
                                Some(agent.as_str()),
                                Some(session_id.as_str()),
                                process_id,
                                "only team-lead may send session_end",
                            );
                            return make_ok_response(
                                &request.request_id,
                                serde_json::json!({"processed": false, "reason": "only team-lead may send session_end"}),
                            );
                        }
                        let mut registry = session_registry.lock().unwrap();
                        registry.mark_dead_for_team_session(&team, &agent, &session_id)
                    }
                }
            };

            match mark_dead_outcome {
                MarkDeadForSessionOutcome::MarkedDead => {
                    let (old_state, new_state) = {
                        let mut tracker = state_store.lock().unwrap();
                        let current = tracker.get_state(&agent);
                        if tracker.get_state(&agent).is_some() {
                            tracker.set_state_with_context(
                                &agent,
                                AgentState::Offline,
                                "session_end lifecycle",
                                "hook_event",
                            );
                        }
                        let updated = tracker.get_state(&agent).unwrap_or(AgentState::Unknown);
                        (current, updated)
                    };
                    emit_member_transition_events(
                        &team,
                        &agent,
                        old_state,
                        new_state,
                        "hook_event.session_end",
                        Some(session_id.as_str()),
                        process_id,
                    );
                    emit_hook_success(
                        event_type.as_str(),
                        &team,
                        &agent,
                        Some(session_id.as_str()),
                        process_id,
                    );
                    info!(agent = %agent, agent_pid = agent_pid, "hook_event session_end");
                }
                MarkDeadForSessionOutcome::AlreadyDead => {
                    debug!(
                        team = %team,
                        agent = %agent,
                        session_id = %session_id,
                        "hook_event session_end duplicate ignored (already dead)"
                    );
                }
                MarkDeadForSessionOutcome::UnknownSession => {
                    debug!(
                        team = %team,
                        agent = %agent,
                        session_id = %session_id,
                        "hook_event session_end ignored (unknown session)"
                    );
                }
                MarkDeadForSessionOutcome::SessionMismatch { current_session_id } => {
                    let msg = format!(
                        "session_end ignored due to session mismatch (expected/current='{}', received='{}')",
                        current_session_id, session_id
                    );
                    warn!(
                        team = %team,
                        agent = %agent,
                        active_session_id = %current_session_id,
                        current_session_id = %current_session_id,
                        received_session_id = %session_id,
                        "hook_event session_end session_id mismatch; ignoring"
                    );
                    emit_event_best_effort(EventFields {
                        level: "warn",
                        source: "atm-daemon",
                        action: "SESSION_END_SESSION_MISMATCH",
                        team: Some(team.clone()),
                        agent_name: Some(agent.clone()),
                        session_id: Some(current_session_id),
                        target: Some(format!("received:{session_id}")),
                        result: Some("ignored".to_string()),
                        error: Some(msg),
                        ..Default::default()
                    });
                }
            }
        }
        other => {
            debug!("hook_event unknown event type: {other}");
            emit_hook_failure(
                Some(other),
                Some(team.as_str()),
                Some(agent.as_str()),
                Some(session_id.as_str()),
                process_id,
                &format!("unknown event type: {other}"),
            );
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

#[cfg(all(test, unix))]
async fn handle_hook_event_command(
    request_str: &str,
    state_store: &SharedStateStore,
    session_registry: &SharedSessionRegistry,
) -> SocketResponse {
    let dedup_path = std::env::temp_dir().join(format!(
        "atm-hook-event-test-dedup-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let store = DurableDedupeStore::new(dedup_path, std::time::Duration::from_secs(600), 1000)
        .expect("failed to create test dedupe store");
    let dedup_store = std::sync::Arc::new(std::sync::Mutex::new(store));
    handle_hook_event_command_with_dedup(request_str, state_store, session_registry, &dedup_store)
        .await
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct GhMonitorStateFile {
    records: Vec<GhMonitorStatus>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct GhMonitorHealthFile {
    records: Vec<GhMonitorHealth>,
}

#[cfg(unix)]
fn default_gh_monitor_health(team: &str) -> GhMonitorHealth {
    GhMonitorHealth {
        team: team.to_string(),
        configured: false,
        enabled: false,
        config_source: None,
        config_path: None,
        lifecycle_state: "running".to_string(),
        availability_state: "healthy".to_string(),
        in_flight: 0,
        updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        message: None,
    }
}

#[cfg(unix)]
fn gh_monitor_health_path(home: &std::path::Path) -> PathBuf {
    home.join(".claude/daemon/gh-monitor-health.json")
}

#[cfg(unix)]
fn load_gh_monitor_health_map(
    home: &std::path::Path,
) -> Result<std::collections::HashMap<String, GhMonitorHealth>> {
    let path = gh_monitor_health_path(home);
    if !path.exists() {
        return Ok(std::collections::HashMap::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    let file = serde_json::from_str::<GhMonitorHealthFile>(&raw)?;
    let mut map = std::collections::HashMap::new();
    for record in file.records {
        map.insert(record.team.clone(), record);
    }
    Ok(map)
}

#[cfg(unix)]
fn upsert_gh_monitor_health(home: &std::path::Path, health: GhMonitorHealth) -> Result<()> {
    let mut map = load_gh_monitor_health_map(home)?;
    map.insert(health.team.clone(), health);
    let mut records: Vec<GhMonitorHealth> = map.into_values().collect();
    records.sort_by(|a, b| a.team.cmp(&b.team));
    let file = GhMonitorHealthFile { records };
    let path = gh_monitor_health_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&file)?)?;
    Ok(())
}

#[cfg(unix)]
fn read_gh_monitor_health(home: &std::path::Path, team: &str) -> Result<GhMonitorHealth> {
    let map = load_gh_monitor_health_map(home)?;
    Ok(map
        .get(team)
        .cloned()
        .unwrap_or_else(|| default_gh_monitor_health(team)))
}

#[cfg(unix)]
fn count_in_flight_monitors(home: &std::path::Path, team: &str) -> u64 {
    load_gh_monitor_state_map(home)
        .ok()
        .map(|map| {
            map.values()
                .filter(|status| status.team == team && status.state == "tracking")
                .count() as u64
        })
        .unwrap_or(0)
}

#[cfg(unix)]
fn emit_gh_monitor_health_transition(
    home: &std::path::Path,
    team: &str,
    old_state: &str,
    new_state: &str,
    reason: &str,
) {
    if old_state == new_state {
        return;
    }

    let level = if new_state == "healthy" {
        "info"
    } else {
        "warn"
    };
    emit_event_best_effort(EventFields {
        level,
        source: "atm-daemon",
        action: "gh_monitor_health_transition",
        team: Some(team.to_string()),
        result: Some(format!("{old_state}->{new_state}")),
        error: Some(reason.to_string()),
        ..Default::default()
    });

    let (from_agent, targets) = resolve_ci_alert_routing(home, team);
    let text = format!(
        "[gh_monitor] availability transition {} -> {}\nreason: {}",
        old_state, new_state, reason
    );
    for (agent, target_team) in targets {
        let inbox_path = home
            .join(".claude/teams")
            .join(&target_team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        let message = InboxMessage {
            from: from_agent.clone(),
            text: text.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(format!("gh_monitor: {new_state}")),
            message_id: Some(uuid::Uuid::new_v4().to_string()),
            unknown_fields: std::collections::HashMap::new(),
        };
        if let Err(e) = agent_team_mail_core::io::inbox::inbox_append(
            &inbox_path,
            &message,
            &target_team,
            &agent,
        ) {
            warn!(
                team = %target_team,
                agent = %agent,
                "failed to emit gh_monitor transition alert: {e}"
            );
        }
    }
}

#[cfg(unix)]
fn set_gh_monitor_health_state(
    home: &std::path::Path,
    team: &str,
    lifecycle_state: Option<&str>,
    availability_state: Option<&str>,
    in_flight: Option<u64>,
    message: Option<String>,
    config_state: Option<&GhMonitorConfigState>,
) -> Result<GhMonitorHealth> {
    let mut current = read_gh_monitor_health(home, team)?;
    let old_availability = current.availability_state.clone();

    if let Some(lifecycle_state) = lifecycle_state {
        current.lifecycle_state = lifecycle_state.to_string();
    }
    if let Some(availability_state) = availability_state {
        current.availability_state = availability_state.to_string();
    }
    if let Some(in_flight) = in_flight {
        current.in_flight = in_flight;
    }
    if let Some(config_state) = config_state {
        current.configured = config_state.configured;
        current.enabled = config_state.enabled;
        current.config_source = config_state.config_source.clone();
        current.config_path = config_state.config_path.clone();
    }
    current.updated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    current.message = message;

    if old_availability != current.availability_state {
        let reason = current
            .message
            .clone()
            .unwrap_or_else(|| "availability changed".to_string());
        emit_gh_monitor_health_transition(
            home,
            team,
            &old_availability,
            &current.availability_state,
            &reason,
        );
    }

    upsert_gh_monitor_health(home, current.clone())?;
    Ok(current)
}

#[cfg(unix)]
#[derive(Debug, Clone)]
struct GhMonitorConfigState {
    configured: bool,
    enabled: bool,
    config_source: Option<String>,
    config_path: Option<String>,
    error: Option<String>,
}

#[cfg(unix)]
fn evaluate_gh_monitor_config(
    home: &std::path::Path,
    team: &str,
    config_cwd: Option<&str>,
) -> GhMonitorConfigState {
    let current_dir = config_cwd
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.to_path_buf());
    let location = agent_team_mail_core::config::resolve_plugin_config_location(
        "gh_monitor",
        &current_dir,
        home,
    );

    let config = agent_team_mail_core::config::resolve_config(
        &agent_team_mail_core::config::ConfigOverrides {
            team: Some(team.to_string()),
            ..Default::default()
        },
        &current_dir,
        home,
    );
    let mut state = GhMonitorConfigState {
        configured: false,
        enabled: false,
        config_source: location.as_ref().map(|loc| loc.source.clone()),
        config_path: location
            .as_ref()
            .map(|loc| loc.path.to_string_lossy().to_string()),
        error: None,
    };

    let config = match config {
        Ok(config) => config,
        Err(e) => {
            state.error = Some(e.to_string());
            return state;
        }
    };

    let Some(table) = config.plugin_config("gh_monitor") else {
        state.error = Some("missing [plugins.gh_monitor] configuration".to_string());
        return state;
    };
    state.configured = true;

    let parsed = match crate::plugins::ci_monitor::CiMonitorConfig::from_toml(table) {
        Ok(parsed) => parsed,
        Err(e) => {
            state.error = Some(e.to_string());
            return state;
        }
    };
    state.enabled = parsed.enabled;

    if !parsed.enabled {
        state.error = Some("gh_monitor plugin disabled in configuration".to_string());
        return state;
    }

    if parsed
        .repo
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        state.error = Some("gh_monitor configuration missing required field: repo".to_string());
        return state;
    }

    state
}

#[cfg(unix)]
fn apply_config_state_to_status(status: &mut GhMonitorStatus, config_state: &GhMonitorConfigState) {
    status.configured = config_state.configured;
    status.enabled = config_state.enabled;
    status.config_source = config_state.config_source.clone();
    status.config_path = config_state.config_path.clone();
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhRunView {
    database_id: u64,
    name: String,
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
    head_branch: String,
    head_sha: String,
    url: String,
    #[serde(default)]
    jobs: Vec<GhRunJob>,
    #[serde(default)]
    attempt: Option<u64>,
    #[serde(default)]
    pull_requests: Vec<GhPullRequest>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhRunJob {
    database_id: u64,
    name: String,
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
    #[serde(default)]
    steps: Vec<GhRunStep>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhRunStep {
    name: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPullRequest {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPrLookupView {
    #[serde(default)]
    head_ref_name: Option<String>,
    #[serde(default)]
    head_ref_oid: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPrView {
    #[serde(default)]
    merge_state_status: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhRunListEntry {
    #[serde(default)]
    database_id: Option<u64>,
    #[serde(default)]
    head_sha: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GhRunTerminalState {
    Success,
    Failure,
    TimedOut,
    Cancelled,
    ActionRequired,
    Other,
}

#[cfg(unix)]
async fn handle_gh_monitor_command(request_str: &str, home: &std::path::Path) -> SocketResponse {
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};

    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse gh-monitor request: {e}"),
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

    let gh_request: GhMonitorRequest = match serde_json::from_value(request.payload.clone()) {
        Ok(payload) => payload,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                "INVALID_PAYLOAD",
                &format!("Failed to parse gh-monitor payload: {e}"),
            );
        }
    };

    if gh_request.team.trim().is_empty() {
        return make_error_response(
            &request.request_id,
            "MISSING_PARAMETER",
            "Missing required payload field: 'team'",
        );
    }

    if gh_request.target.trim().is_empty() {
        return make_error_response(
            &request.request_id,
            "MISSING_PARAMETER",
            "Missing required payload field: 'target'",
        );
    }

    // Lifecycle gate: monitor operations require running lifecycle state.
    let current_health = match read_gh_monitor_health(home, &gh_request.team) {
        Ok(health) => health,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                "INTERNAL_ERROR",
                &format!("Failed to read gh monitor health: {e}"),
            );
        }
    };
    if current_health.lifecycle_state != "running" {
        return make_error_response(
            &request.request_id,
            "MONITOR_STOPPED",
            "gh monitor lifecycle is not running (run `atm gh monitor start` first)",
        );
    }

    let config_state =
        evaluate_gh_monitor_config(home, &gh_request.team, gh_request.config_cwd.as_deref());

    // Config gate: invalid/disabled config moves availability into
    // disabled_config_error and blocks polling work.
    if let Some(reason) = config_state.error.clone() {
        // Intentional: command-dispatch config validation updates persisted
        // availability state but does not emit a separate "manual" inbox
        // notification path; transition alerts are emitted by the shared
        // state-transition hook (when availability actually changes).
        let _ = set_gh_monitor_health_state(
            home,
            &gh_request.team,
            None,
            Some("disabled_config_error"),
            Some(0),
            Some(reason.clone()),
            Some(&config_state),
        );
        return make_error_response(
            &request.request_id,
            "CONFIG_ERROR",
            &format!("gh_monitor unavailable: {reason}"),
        );
    }

    if matches!(gh_request.target_kind, GhMonitorTargetKind::Workflow)
        && gh_request
            .reference
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
    {
        return make_error_response(
            &request.request_id,
            "MISSING_PARAMETER",
            "Missing required payload field: 'reference' for workflow monitor",
        );
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut status = GhMonitorStatus {
        team: gh_request.team.clone(),
        configured: config_state.configured,
        enabled: config_state.enabled,
        config_source: config_state.config_source.clone(),
        config_path: config_state.config_path.clone(),
        target_kind: gh_request.target_kind,
        target: gh_request.target.clone(),
        state: "monitoring".to_string(),
        run_id: None,
        reference: gh_request.reference.clone(),
        updated_at: now,
        message: None,
    };

    let mut transient_failure: Option<String> = None;
    match gh_request.target_kind {
        GhMonitorTargetKind::Run => {
            status.run_id = gh_request.target.parse::<u64>().ok();
        }
        GhMonitorTargetKind::Workflow => {
            if let Some(reference) = gh_request.reference.as_deref() {
                match try_find_workflow_run_id(&gh_request.target, reference).await {
                    Ok(Some(run_id)) => status.run_id = Some(run_id),
                    Ok(None) => {}
                    Err(e) => {
                        transient_failure = Some(format!("{e}"));
                        status.message = Some(format!(
                            "workflow run lookup unavailable; tracking without run id: {e}"
                        ));
                    }
                }
            }
        }
        GhMonitorTargetKind::Pr => {
            let pr_number = match gh_request.target.parse::<u64>() {
                Ok(value) if value > 0 => value,
                _ => {
                    return make_error_response(
                        &request.request_id,
                        "INVALID_PAYLOAD",
                        "PR target must be a positive integer",
                    );
                }
            };
            let mut preflight_blocked = false;
            match fetch_pr_merge_state(pr_number).await {
                Ok(Some(pr_view)) => {
                    if let Some(merge_state_status) = pr_view.merge_state_status.as_deref()
                        && is_pr_merge_state_dirty(merge_state_status)
                    {
                        status.state = "merge_conflict".to_string();
                        status.message = Some(format!(
                            "PR #{pr_number} has mergeStateStatus={merge_state_status}; resolve conflicts before CI monitoring."
                        ));
                        emit_merge_conflict_alert(
                            home,
                            &status,
                            pr_view.url.as_deref(),
                            merge_state_status,
                            None,
                        );
                        preflight_blocked = true;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        team = %gh_request.team,
                        pr = pr_number,
                        "gh-monitor preflight mergeStateStatus lookup failed: {e}"
                    );
                }
            }

            if !preflight_blocked {
                let timeout_secs = gh_request.start_timeout_secs.unwrap_or(120);
                if timeout_secs == 0 {
                    status.state = "ci_not_started".to_string();
                    status.message =
                        Some("No workflow run observed before start-timeout (0s).".to_string());
                } else {
                    match wait_for_pr_run_start(pr_number, timeout_secs).await {
                        Ok(Some(run_id)) => {
                            status.run_id = Some(run_id);
                        }
                        Ok(None) => {
                            status.state = "ci_not_started".to_string();
                            status.message = Some(format!(
                                "No workflow run observed for PR #{pr_number} within {timeout_secs}s."
                            ));
                        }
                        Err(e) => {
                            transient_failure = Some(format!("{e}"));
                            status.state = "ci_not_started".to_string();
                            status.message = Some(format!(
                                "Unable to query workflow runs for PR #{pr_number}: {e}"
                            ));
                        }
                    }
                }
            }
        }
    }

    if let Err(e) = upsert_gh_monitor_status(home, status.clone()) {
        let _ = set_gh_monitor_health_state(
            home,
            &gh_request.team,
            None,
            Some("degraded"),
            None,
            Some(format!("failed to persist monitor status: {e}")),
            Some(&config_state),
        );
        return make_error_response(
            &request.request_id,
            "INTERNAL_ERROR",
            &format!("Failed to persist gh monitor state: {e}"),
        );
    }

    if status.state == "ci_not_started" {
        emit_ci_not_started_alert(home, &status);
    } else if let Some(run_id) = status.run_id {
        let home = home.to_path_buf();
        let status_seed = status.clone();
        let gh_request = gh_request.clone();
        tokio::spawn(async move {
            if let Err(e) = monitor_gh_run(home.as_path(), &status_seed, &gh_request, run_id).await
            {
                warn!(
                    team = %status_seed.team,
                    target = %status_seed.target,
                    run_id = run_id,
                    "gh monitor background task failed: {e}"
                );
            }
        });
    }

    if let Some(reason) = transient_failure {
        let _ = set_gh_monitor_health_state(
            home,
            &gh_request.team,
            None,
            Some("degraded"),
            Some(count_in_flight_monitors(home, &gh_request.team)),
            Some(format!("transient provider/gh failure: {reason}")),
            Some(&config_state),
        );
    } else {
        let _ = set_gh_monitor_health_state(
            home,
            &gh_request.team,
            Some("running"),
            Some("healthy"),
            Some(count_in_flight_monitors(home, &gh_request.team)),
            Some("monitor request succeeded".to_string()),
            Some(&config_state),
        );
    }

    make_ok_response(
        &request.request_id,
        serde_json::to_value(status).unwrap_or_default(),
    )
}

#[cfg(unix)]
async fn handle_gh_monitor_control_command(
    request_str: &str,
    home: &std::path::Path,
) -> SocketResponse {
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};

    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse gh-monitor-control request: {e}"),
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

    let control: GhMonitorControlRequest = match serde_json::from_value(request.payload.clone()) {
        Ok(payload) => payload,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                "INVALID_PAYLOAD",
                &format!("Failed to parse gh-monitor-control payload: {e}"),
            );
        }
    };
    if control.team.trim().is_empty() {
        return make_error_response(
            &request.request_id,
            "MISSING_PARAMETER",
            "Missing required payload field: 'team'",
        );
    }
    let config_state =
        evaluate_gh_monitor_config(home, &control.team, control.config_cwd.as_deref());

    let health = match control.action {
        GhMonitorLifecycleAction::Start => match set_gh_monitor_health_state(
            home,
            &control.team,
            Some("running"),
            None,
            Some(count_in_flight_monitors(home, &control.team)),
            Some("gh monitor lifecycle started".to_string()),
            Some(&config_state),
        ) {
            Ok(health) => health,
            Err(e) => {
                return make_error_response(
                    &request.request_id,
                    "INTERNAL_ERROR",
                    &format!("failed to update monitor lifecycle state: {e}"),
                );
            }
        },
        GhMonitorLifecycleAction::Stop => {
            let drain_timeout_secs = control.drain_timeout_secs.unwrap_or(30);
            let _ = set_gh_monitor_health_state(
                home,
                &control.team,
                Some("draining"),
                None,
                Some(count_in_flight_monitors(home, &control.team)),
                Some(format!(
                    "draining in-flight monitors (timeout={}s)",
                    drain_timeout_secs
                )),
                Some(&config_state),
            );

            let deadline = std::time::Instant::now()
                + std::time::Duration::from_secs(drain_timeout_secs.max(1));
            let mut in_flight = count_in_flight_monitors(home, &control.team);
            while in_flight > 0 && std::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                in_flight = count_in_flight_monitors(home, &control.team);
            }

            let message = if in_flight == 0 {
                "gh monitor lifecycle stopped after in-flight drain".to_string()
            } else {
                format!(
                    "drain timeout reached; stopped with {} in-flight monitor(s)",
                    in_flight
                )
            };
            match set_gh_monitor_health_state(
                home,
                &control.team,
                Some("stopped"),
                None,
                Some(in_flight),
                Some(message),
                Some(&config_state),
            ) {
                Ok(health) => health,
                Err(e) => {
                    return make_error_response(
                        &request.request_id,
                        "INTERNAL_ERROR",
                        &format!("failed to stop monitor lifecycle: {e}"),
                    );
                }
            }
        }
        GhMonitorLifecycleAction::Restart => {
            let drain_timeout_secs = control.drain_timeout_secs.unwrap_or(30);
            let _ = set_gh_monitor_health_state(
                home,
                &control.team,
                Some("draining"),
                None,
                Some(count_in_flight_monitors(home, &control.team)),
                Some(format!(
                    "draining in-flight monitors before restart (timeout={}s)",
                    drain_timeout_secs
                )),
                Some(&config_state),
            );

            let deadline = std::time::Instant::now()
                + std::time::Duration::from_secs(drain_timeout_secs.max(1));
            let mut in_flight = count_in_flight_monitors(home, &control.team);
            while in_flight > 0 && std::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                in_flight = count_in_flight_monitors(home, &control.team);
            }

            // Re-evaluate config after the drain window so restart/reload picks
            // up current on-disk config without requiring daemon restart.
            let reloaded_config =
                evaluate_gh_monitor_config(home, &control.team, control.config_cwd.as_deref());
            if let Some(reason) = reloaded_config.error.as_deref() {
                let message = format!("gh monitor restart blocked: {reason}");
                let _ = set_gh_monitor_health_state(
                    home,
                    &control.team,
                    Some("stopped"),
                    Some("disabled_config_error"),
                    Some(in_flight),
                    Some(message),
                    Some(&reloaded_config),
                );
                return make_error_response(
                    &request.request_id,
                    "CONFIG_ERROR",
                    &format!("gh_monitor unavailable after reload: {reason}"),
                );
            }

            match set_gh_monitor_health_state(
                home,
                &control.team,
                Some("running"),
                Some("healthy"),
                Some(in_flight),
                Some(if in_flight == 0 {
                    "gh monitor lifecycle restarted after in-flight drain".to_string()
                } else {
                    format!(
                        "gh monitor lifecycle restarted after drain timeout; {} in-flight monitor(s) remain",
                        in_flight
                    )
                }),
                Some(&reloaded_config),
            ) {
                Ok(health) => health,
                Err(e) => {
                    return make_error_response(
                        &request.request_id,
                        "INTERNAL_ERROR",
                        &format!("failed to restart monitor lifecycle: {e}"),
                    );
                }
            }
        }
    };

    make_ok_response(
        &request.request_id,
        serde_json::to_value(health).unwrap_or_default(),
    )
}

#[cfg(unix)]
async fn handle_gh_monitor_health_command(
    request_str: &str,
    home: &std::path::Path,
) -> SocketResponse {
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};

    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse gh-monitor-health request: {e}"),
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

    let team = request
        .payload
        .get("team")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let config_cwd = request
        .payload
        .get("config_cwd")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if team.is_empty() {
        return make_error_response(
            &request.request_id,
            "MISSING_PARAMETER",
            "Missing required payload field: 'team'",
        );
    }

    let config_state = evaluate_gh_monitor_config(home, &team, config_cwd.as_deref());

    let health = match read_gh_monitor_health(home, &team) {
        Ok(mut health) => {
            health.in_flight = count_in_flight_monitors(home, &team);
            health.configured = config_state.configured;
            health.enabled = config_state.enabled;
            health.config_source = config_state.config_source.clone();
            health.config_path = config_state.config_path.clone();
            if let Some(reason) = config_state.error.as_deref() {
                health.availability_state = "disabled_config_error".to_string();
                health.message = Some(reason.to_string());
            }
            health
        }
        Err(e) => {
            return make_error_response(
                &request.request_id,
                "INTERNAL_ERROR",
                &format!("Failed to read gh monitor health: {e}"),
            );
        }
    };
    make_ok_response(
        &request.request_id,
        serde_json::to_value(health).unwrap_or_default(),
    )
}

#[cfg(not(unix))]
async fn handle_gh_monitor_command(request_str: &str, _home: &std::path::Path) -> SocketResponse {
    let request_id = serde_json::from_str::<serde_json::Value>(request_str)
        .ok()
        .and_then(|value| {
            value
                .get("request_id")
                .and_then(|request_id| request_id.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    make_error_response(
        &request_id,
        "UNSUPPORTED_PLATFORM",
        "gh-monitor commands require Unix daemon transport",
    )
}

#[cfg(not(unix))]
async fn handle_gh_monitor_control_command(
    request_str: &str,
    _home: &std::path::Path,
) -> SocketResponse {
    let request_id = serde_json::from_str::<serde_json::Value>(request_str)
        .ok()
        .and_then(|value| {
            value
                .get("request_id")
                .and_then(|request_id| request_id.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    make_error_response(
        &request_id,
        "UNSUPPORTED_PLATFORM",
        "gh-monitor-control commands require Unix daemon transport",
    )
}

#[cfg(not(unix))]
async fn handle_gh_monitor_health_command(
    request_str: &str,
    _home: &std::path::Path,
) -> SocketResponse {
    let request_id = serde_json::from_str::<serde_json::Value>(request_str)
        .ok()
        .and_then(|value| {
            value
                .get("request_id")
                .and_then(|request_id| request_id.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    make_error_response(
        &request_id,
        "UNSUPPORTED_PLATFORM",
        "gh-monitor-health commands require Unix daemon transport",
    )
}

#[cfg(unix)]
async fn handle_gh_status_command(request_str: &str, home: &std::path::Path) -> SocketResponse {
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};

    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse gh-status request: {e}"),
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

    let gh_request: GhStatusRequest = match serde_json::from_value(request.payload.clone()) {
        Ok(payload) => payload,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                "INVALID_PAYLOAD",
                &format!("Failed to parse gh-status payload: {e}"),
            );
        }
    };

    let config_state =
        evaluate_gh_monitor_config(home, &gh_request.team, gh_request.config_cwd.as_deref());
    if let Some(reason) = config_state.error.as_deref() {
        return make_error_response(
            &request.request_id,
            "CONFIG_ERROR",
            &format!("gh_monitor unavailable: {reason}"),
        );
    }

    let state = match load_gh_monitor_state_map(home) {
        Ok(map) => map,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                "INTERNAL_ERROR",
                &format!("Failed to read gh monitor state: {e}"),
            );
        }
    };

    let key = gh_monitor_key(
        &gh_request.team,
        gh_request.target_kind,
        &gh_request.target,
        gh_request.reference.as_deref(),
    );
    if let Some(mut status) = state.get(&key).cloned() {
        apply_config_state_to_status(&mut status, &config_state);
        return make_ok_response(
            &request.request_id,
            serde_json::to_value(status).unwrap_or_default(),
        );
    }

    if matches!(gh_request.target_kind, GhMonitorTargetKind::Workflow) {
        let mut candidates: Vec<&GhMonitorStatus> = state
            .values()
            .filter(|record| {
                record.team == gh_request.team
                    && record.target_kind == GhMonitorTargetKind::Workflow
                    && record.target == gh_request.target
                    && gh_request
                        .reference
                        .as_deref()
                        .is_none_or(|reference| record.reference.as_deref() == Some(reference))
            })
            .collect();
        candidates.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        if let Some(mut status) = candidates.last().cloned().cloned() {
            apply_config_state_to_status(&mut status, &config_state);
            return make_ok_response(
                &request.request_id,
                serde_json::to_value(status).unwrap_or_default(),
            );
        }
    }

    make_error_response(
        &request.request_id,
        "MONITOR_NOT_FOUND",
        "No gh monitor state found for requested target",
    )
}

#[cfg(not(unix))]
async fn handle_gh_status_command(request_str: &str, _home: &std::path::Path) -> SocketResponse {
    let request_id = serde_json::from_str::<serde_json::Value>(request_str)
        .ok()
        .and_then(|value| {
            value
                .get("request_id")
                .and_then(|request_id| request_id.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    make_error_response(
        &request_id,
        "UNSUPPORTED_PLATFORM",
        "gh-status commands require Unix daemon transport",
    )
}

#[cfg(unix)]
async fn wait_for_pr_run_start(pr_number: u64, timeout_secs: u64) -> Result<Option<u64>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        if let Some(run_id) = try_find_pr_run_id(pr_number).await? {
            return Ok(Some(run_id));
        }

        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        let remaining = deadline.saturating_duration_since(now);
        let sleep_for = remaining.min(std::time::Duration::from_secs(5));
        tokio::time::sleep(sleep_for).await;
    }
}

#[cfg(unix)]
async fn try_find_pr_run_id(pr_number: u64) -> Result<Option<u64>> {
    let output = run_gh_command(&[
        "pr",
        "view",
        &pr_number.to_string(),
        "--json",
        "headRefName,headRefOid,createdAt",
    ])
    .await?;
    let pr_view = serde_json::from_str::<GhPrLookupView>(&output)?;
    let branch = pr_view
        .head_ref_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let pr_head_sha = pr_view
        .head_ref_oid
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let pr_created_at = pr_view
        .created_at
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let Some(branch) = branch else {
        return Ok(None);
    };

    let output = run_gh_command(&[
        "run",
        "list",
        "--branch",
        &branch,
        "--limit",
        "20",
        "--json",
        "databaseId,headSha,createdAt",
    ])
    .await?;
    let runs = serde_json::from_str::<Vec<GhRunListEntry>>(&output)?;
    for run in runs {
        let Some(run_id) = run.database_id else {
            continue;
        };

        if let Some(expected_head_sha) = pr_head_sha.as_deref()
            && run
                .head_sha
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                != Some(expected_head_sha)
        {
            continue;
        }

        if !run_passes_pr_recency_gate(run.created_at.as_deref(), pr_created_at.as_deref()) {
            continue;
        }

        return Ok(Some(run_id));
    }

    Ok(None)
}

#[cfg(unix)]
fn run_passes_pr_recency_gate(run_created_at: Option<&str>, pr_created_at: Option<&str>) -> bool {
    let Some(pr_created_at) = pr_created_at else {
        return true;
    };
    let Some(run_created_at) = run_created_at else {
        return true;
    };

    let parse_ts = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok();
    let Some(pr_ts) = parse_ts(pr_created_at) else {
        return true;
    };
    let Some(run_ts) = parse_ts(run_created_at) else {
        return true;
    };

    run_ts >= pr_ts
}

#[cfg(unix)]
async fn fetch_pr_merge_state(pr_number: u64) -> Result<Option<GhPrView>> {
    let output = run_gh_command(&[
        "pr",
        "view",
        &pr_number.to_string(),
        "--json",
        "mergeStateStatus,url",
    ])
    .await?;
    let pr = serde_json::from_str::<GhPrView>(&output)?;
    if pr
        .merge_state_status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some(pr))
}

#[cfg(unix)]
fn is_pr_merge_state_dirty(merge_state_status: &str) -> bool {
    merge_state_status.trim().eq_ignore_ascii_case("dirty")
}

#[cfg(unix)]
async fn try_find_workflow_run_id(workflow: &str, reference: &str) -> Result<Option<u64>> {
    let output = run_gh_command(&[
        "run",
        "list",
        "--workflow",
        workflow,
        "--limit",
        "20",
        "--json",
        "databaseId,headBranch,headSha",
    ])
    .await?;
    let runs = serde_json::from_str::<Vec<serde_json::Value>>(&output)?;

    for run in runs {
        let branch = run.get("headBranch").and_then(|v| v.as_str());
        let sha = run.get("headSha").and_then(|v| v.as_str());
        let matches_ref =
            branch == Some(reference) || sha.is_some_and(|s| s.starts_with(reference));
        if matches_ref && let Some(run_id) = run.get("databaseId").and_then(|v| v.as_u64()) {
            return Ok(Some(run_id));
        }
    }

    Ok(None)
}

#[cfg(unix)]
async fn run_gh_command(args: &[&str]) -> Result<String> {
    let output = tokio::process::Command::new("gh")
        .args(args)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gh {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(unix)]
async fn monitor_gh_run(
    home: &std::path::Path,
    status_seed: &GhMonitorStatus,
    gh_request: &GhMonitorRequest,
    run_id: u64,
) -> Result<()> {
    let (from_agent, targets) = resolve_ci_alert_routing(home, &status_seed.team);
    let mut seen_completed: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut pending_completed: Vec<GhRunJob> = Vec::new();
    let mut last_progress_emit: Option<std::time::Instant> = None;
    let mut first_poll = true;

    loop {
        let run = fetch_run_view(run_id).await?;
        let completed_jobs: Vec<GhRunJob> = run
            .jobs
            .iter()
            .filter(|job| is_job_completed(job))
            .cloned()
            .collect();
        for job in completed_jobs {
            if seen_completed.insert(job.database_id) {
                pending_completed.push(job);
            }
        }

        let terminal = classify_terminal_state(&run);
        if terminal.is_none() {
            let now = std::time::Instant::now();
            if should_emit_progress(last_progress_emit, now) && !pending_completed.is_empty() {
                let message = format_progress_message(&run, &pending_completed);
                let summary = format!(
                    "ci progress: run {} ({}/{})",
                    run.database_id,
                    count_completed_jobs(&run),
                    run.jobs.len()
                );
                emit_ci_monitor_message(
                    home,
                    &from_agent,
                    &targets,
                    &summary,
                    &message,
                    Some(format!(
                        "ci-progress-{}-{}",
                        run.database_id,
                        uuid::Uuid::new_v4()
                    )),
                );
                pending_completed.clear();
                last_progress_emit = Some(now);
            }

            let mut state = status_seed.clone();
            state.run_id = Some(run.database_id);
            state.state = "monitoring".to_string();
            state.updated_at = chrono::Utc::now().to_rfc3339();
            state.message = Some(format!(
                "Run {} still in progress ({}/{})",
                run.database_id,
                count_completed_jobs(&run),
                run.jobs.len()
            ));
            upsert_gh_monitor_status(home, state)?;

            // Send first update quickly to make monitor state visible, then settle
            // into a lighter poll cadence.
            let sleep_secs = if first_poll { 5 } else { 15 };
            first_poll = false;
            tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
            continue;
        }

        let terminal = terminal.unwrap_or(GhRunTerminalState::Other);
        let summary_table = format_summary_table(&run);
        let mut message = format!(
            "CI monitor terminal update\nRun: {}\nWorkflow: {}\nState: {}\nURL: {}\n\n{}\n",
            run.database_id,
            run.name,
            terminal_state_label(terminal),
            run.url,
            summary_table
        );

        if terminal != GhRunTerminalState::Success {
            let correlation_id = format!("ci-failure-{}-{}", run.database_id, uuid::Uuid::new_v4());
            let failure_payload =
                build_failure_payload(&run, status_seed, gh_request, &correlation_id).await;
            message.push_str("\nFailure details:\n");
            message.push_str(&failure_payload);
        }

        let summary = format!(
            "ci terminal: run {} {}",
            run.database_id,
            terminal_state_label(terminal)
        );
        emit_ci_monitor_message(
            home,
            &from_agent,
            &targets,
            &summary,
            &message,
            Some(format!(
                "ci-terminal-{}-{}",
                run.database_id,
                uuid::Uuid::new_v4()
            )),
        );

        let mut state = status_seed.clone();
        state.run_id = Some(run.database_id);
        state.state = terminal_state_label(terminal)
            .to_lowercase()
            .replace(' ', "_");
        state.updated_at = chrono::Utc::now().to_rfc3339();
        state.message = Some(format!(
            "Terminal: {} ({}/{})",
            terminal_state_label(terminal),
            count_completed_jobs(&run),
            run.jobs.len()
        ));
        upsert_gh_monitor_status(home, state)?;

        if matches!(gh_request.target_kind, GhMonitorTargetKind::Pr)
            && let Ok(pr_number) = status_seed.target.trim().parse::<u64>()
        {
            match fetch_pr_merge_state(pr_number).await {
                Ok(Some(pr_view)) => {
                    if let Some(merge_state_status) = pr_view.merge_state_status.as_deref()
                        && is_pr_merge_state_dirty(merge_state_status)
                    {
                        emit_merge_conflict_alert(
                            home,
                            status_seed,
                            pr_view.url.as_deref(),
                            merge_state_status,
                            run.conclusion.as_deref(),
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        team = %status_seed.team,
                        pr = %status_seed.target,
                        "gh-monitor post-terminal mergeStateStatus lookup failed: {e}"
                    );
                }
            }
        }
        return Ok(());
    }
}

#[cfg(unix)]
async fn fetch_run_view(run_id: u64) -> Result<GhRunView> {
    let output = run_gh_command(&[
        "run",
        "view",
        &run_id.to_string(),
        "--json",
        "databaseId,name,status,conclusion,headBranch,headSha,url,jobs,attempt,pullRequests",
    ])
    .await?;
    let run = serde_json::from_str::<GhRunView>(&output)?;
    Ok(run)
}

#[cfg(unix)]
fn should_emit_progress(
    last_progress_emit: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    match last_progress_emit {
        None => true,
        Some(prev) => now.duration_since(prev) >= std::time::Duration::from_secs(60),
    }
}

#[cfg(unix)]
fn is_job_completed(job: &GhRunJob) -> bool {
    job.status.eq_ignore_ascii_case("completed")
}

#[cfg(unix)]
fn count_completed_jobs(run: &GhRunView) -> usize {
    run.jobs.iter().filter(|job| is_job_completed(job)).count()
}

#[cfg(unix)]
fn format_progress_message(run: &GhRunView, pending_completed: &[GhRunJob]) -> String {
    let new_jobs = pending_completed
        .iter()
        .map(|job| format!("{}({})", job.name, job_status_label(job)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "CI monitor progress\nRun: {}\nWorkflow: {}\nCompleted: {}/{}\nNewly completed: {}\nRun URL: {}",
        run.database_id,
        run.name,
        count_completed_jobs(run),
        run.jobs.len(),
        if new_jobs.is_empty() {
            "(none)"
        } else {
            &new_jobs
        },
        run.url
    )
}

#[cfg(unix)]
fn job_status_label(job: &GhRunJob) -> &'static str {
    match job
        .conclusion
        .as_deref()
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "success" => "success",
        "failure" => "failure",
        "timedout" | "timed_out" => "timed_out",
        "cancelled" => "cancelled",
        "actionrequired" | "action_required" => "action_required",
        _ => {
            if is_job_completed(job) {
                "completed"
            } else {
                "in_progress"
            }
        }
    }
}

#[cfg(unix)]
fn classify_terminal_state(run: &GhRunView) -> Option<GhRunTerminalState> {
    if !run.status.eq_ignore_ascii_case("completed") && run.conclusion.is_none() {
        return None;
    }
    let state = match run
        .conclusion
        .as_deref()
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "success" => GhRunTerminalState::Success,
        "failure" => GhRunTerminalState::Failure,
        "timedout" | "timed_out" => GhRunTerminalState::TimedOut,
        "cancelled" => GhRunTerminalState::Cancelled,
        "actionrequired" | "action_required" => GhRunTerminalState::ActionRequired,
        _ => GhRunTerminalState::Other,
    };
    Some(state)
}

#[cfg(unix)]
fn terminal_state_label(state: GhRunTerminalState) -> &'static str {
    match state {
        GhRunTerminalState::Success => "SUCCESS",
        GhRunTerminalState::Failure => "FAILURE",
        GhRunTerminalState::TimedOut => "TIMED_OUT",
        GhRunTerminalState::Cancelled => "CANCELLED",
        GhRunTerminalState::ActionRequired => "ACTION_REQUIRED",
        GhRunTerminalState::Other => "UNKNOWN",
    }
}

#[cfg(unix)]
fn format_summary_table(run: &GhRunView) -> String {
    let mut lines = Vec::new();
    lines.push("| Job/Test | Status | Runtime |".to_string());
    lines.push("|---|---|---|".to_string());
    for job in &run.jobs {
        lines.push(format!(
            "| {} | {} | {} |",
            job.name,
            job_status_label(job),
            format_job_runtime(job)
        ));
    }
    lines.join("\n")
}

#[cfg(unix)]
fn format_job_runtime(job: &GhRunJob) -> String {
    let Some(started) = job.started_at.as_deref() else {
        return "-".to_string();
    };
    let Some(completed) = job.completed_at.as_deref() else {
        return "-".to_string();
    };
    let Ok(started_dt) = chrono::DateTime::parse_from_rfc3339(started) else {
        return "-".to_string();
    };
    let Ok(completed_dt) = chrono::DateTime::parse_from_rfc3339(completed) else {
        return "-".to_string();
    };
    let duration = completed_dt.signed_duration_since(started_dt);
    let secs = duration.num_seconds().max(0);
    format!("{}m {}s", secs / 60, secs % 60)
}

#[cfg(unix)]
fn emit_ci_monitor_message(
    home: &std::path::Path,
    from_agent: &str,
    targets: &[(String, String)],
    summary: &str,
    text: &str,
    message_id: Option<String>,
) {
    for (agent, team) in targets {
        let inbox_path = home
            .join(".claude/teams")
            .join(team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        let message = InboxMessage {
            from: from_agent.to_string(),
            text: text.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(summary.to_string()),
            message_id: message_id.clone(),
            unknown_fields: std::collections::HashMap::new(),
        };
        if let Err(e) =
            agent_team_mail_core::io::inbox::inbox_append(&inbox_path, &message, team, agent)
        {
            warn!(
                team = %team,
                agent = %agent,
                "failed to emit ci monitor message: {e}"
            );
        }
    }
}

#[cfg(unix)]
async fn build_failure_payload(
    run: &GhRunView,
    status_seed: &GhMonitorStatus,
    gh_request: &GhMonitorRequest,
    correlation_id: &str,
) -> String {
    let failed_jobs: Vec<&GhRunJob> = run
        .jobs
        .iter()
        .filter(|job| matches!(job_status_label(job), "failure" | "timed_out"))
        .collect();
    let failed_job_names = failed_jobs
        .iter()
        .map(|job| job.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let failed_job_urls = failed_jobs
        .iter()
        .map(|job| {
            job.url.clone().unwrap_or_else(|| {
                format!("{}/job/{}", run.url.trim_end_matches('/'), job.database_id)
            })
        })
        .collect::<Vec<_>>();
    let first_failing_step = failed_jobs
        .iter()
        .flat_map(|job| job.steps.iter())
        .find(|step| {
            let conclusion = step
                .conclusion
                .as_deref()
                .unwrap_or_default()
                .to_lowercase();
            let status = step.status.as_deref().unwrap_or_default().to_lowercase();
            conclusion == "failure"
                || conclusion == "timed_out"
                || conclusion == "timedout"
                || status == "failed"
        })
        .map(|step| step.name.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let failed_log_excerpt = if let Some(first_job) = failed_jobs.first() {
        fetch_failed_log_excerpt(first_job.database_id)
            .await
            .unwrap_or_else(|_| "(log excerpt unavailable)".to_string())
    } else {
        "(no failed jobs captured)".to_string()
    };

    let classification = classify_failure(run);
    let pr_url = derive_pr_url(run, status_seed, gh_request);
    let repo_base = derive_repo_base_from_run_url(&run.url).unwrap_or_default();
    let next_action = if failed_jobs.is_empty() {
        format!("atm gh status run {}", run.database_id)
    } else {
        format!("gh run view {} --log-failed", run.database_id)
    };
    format!(
        "run_url: {run_url}\nfailed_job_urls: {failed_job_urls}\npr_url: {pr_url}\nworkflow: {workflow}\njob_names: {job_names}\nrun_id: {run_id}\nrun_attempt: {attempt}\nbranch: {branch}\ncommit_short: {sha_short}\ncommit_full: {sha_full}\nclassification: {classification}\nfirst_failing_step: {first_failing_step}\nlog_excerpt: {log_excerpt}\ncorrelation_id: {correlation_id}\nnext_action_hint: {next_action}\nrepo_base: {repo_base}",
        run_url = run.url,
        failed_job_urls = if failed_job_urls.is_empty() {
            "(none)".to_string()
        } else {
            failed_job_urls.join(", ")
        },
        pr_url = pr_url.unwrap_or_else(|| "(unknown)".to_string()),
        workflow = run.name,
        job_names = if failed_job_names.is_empty() {
            "(none)".to_string()
        } else {
            failed_job_names
        },
        run_id = run.database_id,
        attempt = run.attempt.unwrap_or(1),
        branch = run.head_branch,
        sha_short = short_sha(&run.head_sha),
        sha_full = run.head_sha,
        classification = classification,
        first_failing_step = first_failing_step,
        log_excerpt = failed_log_excerpt
            .replace('\n', " ")
            .chars()
            .take(240)
            .collect::<String>(),
        correlation_id = correlation_id,
        next_action = next_action,
        repo_base = repo_base,
    )
}

#[cfg(unix)]
async fn fetch_failed_log_excerpt(job_id: u64) -> Result<String> {
    let output = run_gh_command(&["run", "view", "--job", &job_id.to_string(), "--log"]).await?;
    let excerpt = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join(" | ");
    Ok(excerpt)
}

#[cfg(unix)]
fn classify_failure(run: &GhRunView) -> &'static str {
    match run
        .conclusion
        .as_deref()
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "timedout" | "timed_out" => "timeout",
        "cancelled" => "cancelled",
        "actionrequired" | "action_required" => "action_required",
        "failure" => {
            if is_infra_failure(run) {
                "infra"
            } else {
                "test_fail"
            }
        }
        _ => "unknown",
    }
}

#[cfg(unix)]
fn is_infra_failure(run: &GhRunView) -> bool {
    const INFRA_HINTS: &[&str] = &[
        "runner",
        "infrastructure",
        "resource exhausted",
        "no space",
        "disk",
        "network",
        "connection",
        "service unavailable",
        "timed out waiting",
        "oom",
        "out of memory",
    ];

    let contains_infra_hint = |value: &str| {
        let lowered = value.to_lowercase();
        INFRA_HINTS.iter().any(|hint| lowered.contains(hint))
    };

    run.jobs.iter().any(|job| {
        let failed = matches!(job_status_label(job), "failure" | "timed_out");
        failed
            && (contains_infra_hint(&job.name)
                || job.steps.iter().any(|step| contains_infra_hint(&step.name)))
    })
}

#[cfg(unix)]
fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect::<String>()
}

#[cfg(unix)]
fn derive_repo_base_from_run_url(run_url: &str) -> Option<String> {
    let parts = run_url.split('/').collect::<Vec<_>>();
    if parts.len() < 5 {
        return None;
    }
    Some(format!(
        "{}//{}/{}/{}",
        parts[0], parts[2], parts[3], parts[4]
    ))
}

#[cfg(unix)]
fn derive_pr_url(
    run: &GhRunView,
    status_seed: &GhMonitorStatus,
    gh_request: &GhMonitorRequest,
) -> Option<String> {
    if let Some(url) = run.pull_requests.iter().find_map(|pr| pr.url.clone()) {
        return Some(url);
    }
    if matches!(gh_request.target_kind, GhMonitorTargetKind::Pr)
        && let Some(repo_base) = derive_repo_base_from_run_url(&run.url)
    {
        return Some(format!("{}/pull/{}", repo_base, status_seed.target.trim()));
    }
    None
}

#[cfg(unix)]
fn gh_monitor_state_path(home: &std::path::Path) -> PathBuf {
    home.join(".claude/daemon/gh-monitor-state.json")
}

#[cfg(unix)]
fn gh_monitor_key(
    team: &str,
    target_kind: GhMonitorTargetKind,
    target: &str,
    reference: Option<&str>,
) -> String {
    let kind = match target_kind {
        GhMonitorTargetKind::Pr => "pr",
        GhMonitorTargetKind::Workflow => "workflow",
        GhMonitorTargetKind::Run => "run",
    };
    let reference = reference.unwrap_or_default();
    format!(
        "{}|{}|{}|{}",
        team.trim(),
        kind,
        target.trim(),
        reference.trim()
    )
}

#[cfg(unix)]
fn load_gh_monitor_state_map(
    home: &std::path::Path,
) -> Result<std::collections::HashMap<String, GhMonitorStatus>> {
    let path = gh_monitor_state_path(home);
    if !path.exists() {
        return Ok(std::collections::HashMap::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    let state = serde_json::from_str::<GhMonitorStateFile>(&raw)?;
    let mut map = std::collections::HashMap::new();
    for record in state.records {
        let key = gh_monitor_key(
            &record.team,
            record.target_kind,
            &record.target,
            record.reference.as_deref(),
        );
        map.insert(key, record);
    }
    Ok(map)
}

#[cfg(unix)]
fn upsert_gh_monitor_status(home: &std::path::Path, status: GhMonitorStatus) -> Result<()> {
    let mut map = load_gh_monitor_state_map(home)?;
    let key = gh_monitor_key(
        &status.team,
        status.target_kind,
        &status.target,
        status.reference.as_deref(),
    );
    map.insert(key, status);
    let mut records: Vec<GhMonitorStatus> = map.into_values().collect();
    records.sort_by(|a, b| {
        let ak = gh_monitor_key(&a.team, a.target_kind, &a.target, a.reference.as_deref());
        let bk = gh_monitor_key(&b.team, b.target_kind, &b.target, b.reference.as_deref());
        ak.cmp(&bk)
    });
    let state = GhMonitorStateFile { records };
    let path = gh_monitor_state_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&state)?)?;
    Ok(())
}

#[cfg(unix)]
fn emit_ci_not_started_alert(home: &std::path::Path, status: &GhMonitorStatus) {
    let (from_agent, targets) = resolve_ci_alert_routing(home, &status.team);
    let text = format!(
        "[ci_not_started] {} target '{}' did not produce a run in the start window.\n{}",
        match status.target_kind {
            GhMonitorTargetKind::Pr => "PR monitor",
            GhMonitorTargetKind::Workflow => "workflow monitor",
            GhMonitorTargetKind::Run => "run monitor",
        },
        status.target,
        status.message.clone().unwrap_or_default()
    );
    let summary = format!("ci_not_started: {}", status.target);
    for (agent, team) in targets {
        let inbox_path = home
            .join(".claude/teams")
            .join(&team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        let message = InboxMessage {
            from: from_agent.clone(),
            text: text.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(summary.clone()),
            message_id: Some(uuid::Uuid::new_v4().to_string()),
            unknown_fields: std::collections::HashMap::new(),
        };
        if let Err(e) =
            agent_team_mail_core::io::inbox::inbox_append(&inbox_path, &message, &team, &agent)
        {
            warn!(
                team = %team,
                agent = %agent,
                "failed to emit ci_not_started alert: {e}"
            );
        }
    }
}

#[cfg(unix)]
fn emit_merge_conflict_alert(
    home: &std::path::Path,
    status: &GhMonitorStatus,
    pr_url: Option<&str>,
    merge_state_status: &str,
    run_conclusion: Option<&str>,
) {
    let (from_agent, targets) = resolve_ci_alert_routing(home, &status.team);
    let target_kind = match status.target_kind {
        GhMonitorTargetKind::Pr => "pr",
        GhMonitorTargetKind::Workflow => "workflow",
        GhMonitorTargetKind::Run => "run",
    };
    let mut text = format!(
        "[merge_conflict] Merge conflict detected for monitored target.\nclassification: merge_conflict\nstatus: merge_conflict\ntarget_kind: {target_kind}\ntarget: {}\npr_url: {}\nmerge_state_status: {}",
        status.target,
        pr_url.unwrap_or("(unknown)"),
        merge_state_status
    );
    if let Some(run_conclusion) = run_conclusion {
        text.push_str(&format!("\nrun_conclusion: {run_conclusion}"));
    }
    if let Some(message) = status.message.as_deref()
        && !message.trim().is_empty()
    {
        text.push_str(&format!("\nreason: {message}"));
    }

    let summary = format!("merge_conflict: {}", status.target);
    let mut extra_fields = serde_json::Map::new();
    extra_fields.insert(
        "classification".to_string(),
        serde_json::Value::String("merge_conflict".to_string()),
    );
    extra_fields.insert(
        "status".to_string(),
        serde_json::Value::String("merge_conflict".to_string()),
    );
    extra_fields.insert(
        "target_kind".to_string(),
        serde_json::Value::String(target_kind.to_string()),
    );
    extra_fields.insert(
        "pr_url".to_string(),
        serde_json::Value::String(pr_url.unwrap_or("(unknown)").to_string()),
    );
    extra_fields.insert(
        "merge_state_status".to_string(),
        serde_json::Value::String(merge_state_status.to_string()),
    );
    if let Some(run_conclusion) = run_conclusion {
        extra_fields.insert(
            "run_conclusion".to_string(),
            serde_json::Value::String(run_conclusion.to_string()),
        );
    }
    emit_event_best_effort(EventFields {
        level: "warn",
        source: "atm-daemon",
        action: "gh_monitor_merge_conflict",
        team: Some(status.team.clone()),
        target: Some(status.target.clone()),
        result: Some("merge_conflict".to_string()),
        error: Some(format!(
            "merge_state_status={}",
            merge_state_status.trim().to_uppercase()
        )),
        extra_fields,
        ..Default::default()
    });

    for (agent, team) in targets {
        let inbox_path = home
            .join(".claude/teams")
            .join(&team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        let message = InboxMessage {
            from: from_agent.clone(),
            text: text.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: Some(summary.clone()),
            message_id: Some(uuid::Uuid::new_v4().to_string()),
            unknown_fields: std::collections::HashMap::new(),
        };
        if let Err(e) =
            agent_team_mail_core::io::inbox::inbox_append(&inbox_path, &message, &team, &agent)
        {
            warn!(
                team = %team,
                agent = %agent,
                "failed to emit merge_conflict alert: {e}"
            );
        }
    }
}

#[cfg(unix)]
fn resolve_ci_alert_routing(home: &std::path::Path, team: &str) -> (String, Vec<(String, String)>) {
    let current_dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(_) => {
            return (
                "gh-monitor".to_string(),
                vec![("team-lead".to_string(), team.to_string())],
            );
        }
    };
    let config = match agent_team_mail_core::config::resolve_config(
        &agent_team_mail_core::config::ConfigOverrides {
            team: Some(team.to_string()),
            ..Default::default()
        },
        &current_dir,
        home,
    ) {
        Ok(cfg) => cfg,
        Err(_) => {
            return (
                "gh-monitor".to_string(),
                vec![("team-lead".to_string(), team.to_string())],
            );
        }
    };

    let plugin_table = config.plugin_config("gh_monitor");
    let Some(plugin_table) = plugin_table else {
        return (
            "gh-monitor".to_string(),
            vec![("team-lead".to_string(), team.to_string())],
        );
    };

    let parsed = match crate::plugins::ci_monitor::CiMonitorConfig::from_toml(plugin_table) {
        Ok(cfg) => cfg,
        Err(_) => {
            return (
                "gh-monitor".to_string(),
                vec![("team-lead".to_string(), team.to_string())],
            );
        }
    };

    let from_agent = if parsed.agent.trim().is_empty() {
        "gh-monitor".to_string()
    } else {
        parsed.agent
    };
    let targets = if parsed.notify_target.is_empty() {
        vec![("team-lead".to_string(), team.to_string())]
    } else {
        parsed
            .notify_target
            .into_iter()
            .map(|t| {
                let target_team = t.team.unwrap_or_else(|| team.to_string());
                (t.agent, target_team)
            })
            .collect()
    };
    (from_agent, targets)
}

/// Handle the `"control"` command asynchronously.
#[cfg(unix)]
async fn handle_control_command(
    request_str: &str,
    home: &std::path::Path,
    state_store: &SharedStateStore,
    session_registry: &SharedSessionRegistry,
    dedup_store: &SharedDedupeStore,
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

    let ack =
        process_control_request(control, home, state_store, session_registry, dedup_store).await;
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
        ControlAction::ElicitationResponse => "control_elicitation_response",
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
    let mut registry = session_registry.lock().unwrap();
    let Some(record) = registry.query_with_liveness(&control.agent_id) else {
        return ControlResult::NotFound;
    };
    if record.session_id != control.session_id {
        return ControlResult::NotFound;
    }
    if record.state != crate::daemon::session_registry::SessionState::Active {
        return ControlResult::NotLive;
    }

    let tracker = state_store.lock().unwrap();
    match tracker.get_state(&control.agent_id) {
        Some(AgentState::Idle) | Some(AgentState::Active) => ControlResult::Ok,
        Some(AgentState::Unknown) | Some(AgentState::Offline) | None => ControlResult::NotLive,
    }
}

#[cfg(unix)]
pub(crate) fn validate_control_request(control: &ControlRequest) -> Option<String> {
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
    if matches!(control.action, ControlAction::ElicitationResponse) {
        let Some(elicitation_id) = control.elicitation_id.as_deref() else {
            return Some("elicitation_response requires elicitation_id".to_string());
        };
        if elicitation_id.trim().is_empty() {
            return Some("elicitation_id cannot be empty".to_string());
        }
        let Some(decision) = control.decision.as_deref() else {
            return Some("elicitation_response requires decision".to_string());
        };
        if !matches!(decision, "approve" | "reject") {
            return Some("decision must be 'approve' or 'reject'".to_string());
        }
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
async fn enqueue_stdin_message(
    home: &std::path::Path,
    team: &str,
    agent_id: &str,
    content: &str,
) -> Result<(), String> {
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
async fn enqueue_elicitation_response(
    home: &std::path::Path,
    team: &str,
    agent_id: &str,
    elicitation_id: &str,
    decision: &str,
    text: Option<&str>,
) -> Result<(), String> {
    let dir = home
        .join(".config/atm/agent-sessions")
        .join(team)
        .join(agent_id)
        .join("elicitation_queue");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("failed to create elicitation_queue dir: {e}"))?;
    let path = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
    let payload = serde_json::json!({
        "elicitation_id": elicitation_id,
        "decision": decision,
        "text": text,
    });
    tokio::fs::write(
        path,
        serde_json::to_vec(&payload).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| format!("failed to write elicitation_queue file: {e}"))?;
    Ok(())
}

#[cfg(unix)]
async fn process_control_request(
    control: ControlRequest,
    home: &std::path::Path,
    state_store: &SharedStateStore,
    session_registry: &SharedSessionRegistry,
    dedup_store: &SharedDedupeStore,
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
    let is_duplicate = dedup_store.lock().unwrap().check_and_insert(key);
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
                match enqueue_stdin_message(home, &control.team, &control.agent_id, &content).await
                {
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
        ControlAction::ElicitationResponse => {
            let elicitation_id = control
                .elicitation_id
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_string();
            let decision = control
                .decision
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_string();
            let text = control
                .payload
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            match enqueue_elicitation_response(
                home,
                &control.team,
                &control.agent_id,
                &elicitation_id,
                &decision,
                text,
            )
            .await
            {
                Ok(()) => control_ack(
                    &control.request_id,
                    ControlResult::Ok,
                    false,
                    Some("queued elicitation response".to_string()),
                ),
                Err(e) => control_ack(
                    &control.request_id,
                    ControlResult::InternalError,
                    false,
                    Some(format!("enqueue failed: {e}")),
                ),
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
    stream_state_store: &SharedStreamStateStore,
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
        "agent-state" => handle_agent_state(&request, state_store, session_registry),
        "list-agents" => handle_list_agents(&request, state_store, session_registry),
        "agent-pane" => handle_agent_pane(&request, state_store),
        "subscribe" => handle_subscribe(&request, pubsub_store),
        "unsubscribe" => handle_unsubscribe(&request, pubsub_store),
        "register-hint" => handle_register_hint(&request, state_store, session_registry),
        "session-query" => handle_session_query(&request, session_registry),
        "session-query-team" => handle_session_query_team(&request, session_registry),
        "agent-stream-state" => handle_agent_stream_state(&request, stream_state_store),
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
        // "stream-event" is handled asynchronously before parse_and_dispatch is called.
        "stream-event" => make_error_response(
            &request.request_id,
            "INTERNAL_ERROR",
            "stream-event command should have been handled by the async path",
        ),
        // "gh-monitor" is handled asynchronously before parse_and_dispatch is called.
        "gh-monitor" => make_error_response(
            &request.request_id,
            "INTERNAL_ERROR",
            "gh-monitor command should have been handled by the async path",
        ),
        // "gh-status" is handled asynchronously before parse_and_dispatch is called.
        "gh-status" => make_error_response(
            &request.request_id,
            "INTERNAL_ERROR",
            "gh-status command should have been handled by the async path",
        ),
        // "gh-monitor-control" is handled asynchronously before parse_and_dispatch is called.
        "gh-monitor-control" => make_error_response(
            &request.request_id,
            "INTERNAL_ERROR",
            "gh-monitor-control command should have been handled by the async path",
        ),
        // "gh-monitor-health" is handled asynchronously before parse_and_dispatch is called.
        "gh-monitor-health" => make_error_response(
            &request.request_id,
            "INTERNAL_ERROR",
            "gh-monitor-health command should have been handled by the async path",
        ),
        other => make_error_response(
            &request.request_id,
            "UNKNOWN_COMMAND",
            &format!("Unknown command: '{other}'"),
        ),
    };

    Ok(response)
}

/// Handle the `agent-stream-state` command.
///
/// Payload: `{"agent": "<agent-name>"}`
/// Response: the agent's [`AgentStreamState`] or `AGENT_NOT_FOUND` error.
fn handle_agent_stream_state(
    request: &agent_team_mail_core::daemon_client::SocketRequest,
    stream_state_store: &SharedStreamStateStore,
) -> SocketResponse {
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

    let store = stream_state_store.lock().unwrap();
    match store.get(&agent) {
        Some(state) => {
            let payload = serde_json::to_value(state).unwrap_or_default();
            make_ok_response(&request.request_id, payload)
        }
        None => make_error_response(
            &request.request_id,
            "AGENT_NOT_FOUND",
            &format!("No stream state for agent '{agent}'"),
        ),
    }
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

    let mut registry = session_registry.lock().unwrap();
    match registry.query_with_liveness(&name) {
        Some(record) => {
            let alive = record.state == crate::daemon::session_registry::SessionState::Active;
            make_ok_response(
                &request.request_id,
                serde_json::json!({
                    "session_id": record.session_id,
                    "process_id": record.process_id,
                    "alive": alive,
                    "last_alive_at": record.last_alive_at,
                    "runtime": record.runtime,
                    "runtime_session_id": record.runtime_session_id,
                    "pane_id": record.pane_id,
                    "runtime_home": record.runtime_home,
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

/// Handle the `session-query-team` command.
///
/// Payload: `{"team": "<team-name>", "name": "<agent-name>"}`
/// Response (found, alive):   `{"session_id": "...", "process_id": 12345, "alive": true}`
/// Response (found, dead):    `{"session_id": "...", "process_id": 12345, "alive": false}`
/// Response (not found/mismatch): error with code `AGENT_NOT_FOUND`
fn handle_session_query_team(
    request: &agent_team_mail_core::daemon_client::SocketRequest,
    session_registry: &SharedSessionRegistry,
) -> SocketResponse {
    let team = match request.payload.get("team").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            return make_error_response(
                &request.request_id,
                "MISSING_PARAMETER",
                "Missing required payload field: 'team'",
            );
        }
    };
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

    let mut registry = session_registry.lock().unwrap();
    let Some(record) = registry.query_for_team_with_liveness(&team, &name) else {
        return make_error_response(
            &request.request_id,
            "AGENT_NOT_FOUND",
            &format!("No session record for agent '{name}'"),
        );
    };

    // Team-scoped verification: the queried record must match the team's current leadSessionId
    // for team-lead lookups. This avoids cross-team collisions when names overlap.
    if name == "team-lead" {
        let home = match agent_team_mail_core::home::get_home_dir() {
            Ok(h) => h,
            Err(_) => {
                return make_error_response(
                    &request.request_id,
                    "INTERNAL_ERROR",
                    "Failed to resolve home directory",
                );
            }
        };
        let config_path = home.join(".claude/teams").join(&team).join("config.json");
        let content = match std::fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(_) => {
                return make_error_response(
                    &request.request_id,
                    "TEAM_NOT_FOUND",
                    &format!("Team config not found for '{team}'"),
                );
            }
        };
        let cfg: agent_team_mail_core::schema::TeamConfig = match serde_json::from_str(&content) {
            Ok(c) => c,
            Err(_) => {
                return make_error_response(
                    &request.request_id,
                    "INVALID_TEAM_CONFIG",
                    &format!("Failed to parse team config for '{team}'"),
                );
            }
        };
        if cfg.lead_session_id != record.session_id {
            return make_error_response(
                &request.request_id,
                "AGENT_NOT_FOUND",
                &format!("No team-scoped session record for agent '{name}' in team '{team}'"),
            );
        }
    }

    let alive = record.state == crate::daemon::session_registry::SessionState::Active;
    make_ok_response(
        &request.request_id,
        serde_json::json!({
            "session_id": record.session_id,
            "process_id": record.process_id,
            "alive": alive,
            "last_alive_at": record.last_alive_at,
            "runtime": record.runtime,
            "runtime_session_id": record.runtime_session_id,
            "pane_id": record.pane_id,
            "runtime_home": record.runtime_home,
        }),
    )
}

/// Handle the `register-hint` command.
///
/// Payload:
/// `{"team":"<team>","agent":"<name>","session_id":"<sid>","process_id":1234,"runtime":"codex?"...}`
///
/// This updates daemon session registry and state tracker through canonical
/// daemon-owned paths for runtimes that cannot emit lifecycle hooks directly.
fn handle_register_hint(
    request: &agent_team_mail_core::daemon_client::SocketRequest,
    state_store: &SharedStateStore,
    session_registry: &SharedSessionRegistry,
) -> SocketResponse {
    let team = match request.payload.get("team").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => {
            return make_error_response(
                &request.request_id,
                "MISSING_PARAMETER",
                "Missing required payload field: 'team'",
            );
        }
    };
    let agent = match request.payload.get("agent").and_then(|v| v.as_str()) {
        Some(a) if !a.trim().is_empty() => a.trim().to_string(),
        _ => {
            return make_error_response(
                &request.request_id,
                "MISSING_PARAMETER",
                "Missing required payload field: 'agent'",
            );
        }
    };
    let requesting_identity = request
        .payload
        .get("identity")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    if let Some(identity) = requesting_identity.as_deref()
        && identity != agent
    {
        warn!(
            attempted_writer = %identity,
            owner = %agent,
            field = "sessionId",
            "register-hint ownership guard rejected cross-identity session write"
        );
        return make_error_response(
            &request.request_id,
            "PERMISSION_DENIED",
            &format!("Identity '{identity}' is not allowed to update sessionId for '{agent}'"),
        );
    }
    let session_id = match request.payload.get("session_id").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            return make_error_response(
                &request.request_id,
                "MISSING_PARAMETER",
                "Missing required payload field: 'session_id'",
            );
        }
    };
    let process_id = match request.payload.get("process_id").and_then(|v| v.as_u64()) {
        Some(pid) if pid > 1 => pid as u32,
        _ => {
            return make_error_response(
                &request.request_id,
                "MISSING_PARAMETER",
                "Missing required payload field: 'process_id' (>1)",
            );
        }
    };

    let runtime = request
        .payload
        .get("runtime")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let runtime_session_id = request
        .payload
        .get("runtime_session_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let pane_id = request
        .payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let runtime_home = request
        .payload
        .get("runtime_home")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);

    let home = match agent_team_mail_core::home::get_home_dir() {
        Ok(h) => h,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                "INTERNAL_ERROR",
                &format!("Failed to resolve ATM home: {e}"),
            );
        }
    };
    let Some(member) = load_team_member(&home, &team, &agent) else {
        return make_error_response(
            &request.request_id,
            "AGENT_NOT_FOUND",
            &format!("Agent '{agent}' is not in team '{team}'"),
        );
    };

    let validation = validate_pid_backend(&member, process_id);
    if validation.is_alive_mismatch() {
        emit_pid_process_mismatch(&team, &agent, &validation, "register_hint");
        return make_error_response(
            &request.request_id,
            "PID_PROCESS_MISMATCH",
            &format!(
                "backend='{}' expected='{}' actual='{}' pid={}",
                validation.backend,
                validation.expected_display(),
                validation.actual_display(),
                validation.pid
            ),
        );
    }

    let runtime_session = runtime_session_id.clone().or_else(|| {
        runtime
            .as_ref()
            .map(|_| session_id.clone())
            .filter(|v| !v.is_empty())
    });
    session_registry.lock().unwrap().upsert_runtime_for_team(
        &team,
        &agent,
        &session_id,
        process_id,
        runtime.clone(),
        runtime_session.clone(),
        pane_id,
        runtime_home,
    );

    {
        let mut tracker = state_store.lock().unwrap();
        if tracker.get_state(&agent).is_none() {
            tracker.register_agent(&agent);
        }
        tracker.set_state_with_context(
            &agent,
            AgentState::Active,
            "register-hint update",
            "register_hint",
        );
    }

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm-daemon",
        action: "register_hint",
        team: Some(team.clone()),
        agent_name: Some(agent.clone()),
        session_id: Some(session_id.clone()),
        target: Some(format!("pid:{process_id}")),
        result: Some("registered".to_string()),
        runtime,
        runtime_session_id: runtime_session,
        ..Default::default()
    });

    make_ok_response(
        &request.request_id,
        serde_json::json!({
            "processed": true,
            "team": team,
            "agent": agent,
            "session_id": session_id,
            "process_id": process_id
        }),
    )
}

/// Handle the `agent-state` command.
///
/// Payload: `{"agent": "<name>", "team": "<team>"}`  (team is currently informational)
/// Response: `{"state": "<state>", "last_transition": "<iso8601 or null>"}`
fn handle_agent_state(
    request: &agent_team_mail_core::daemon_client::SocketRequest,
    state_store: &SharedStateStore,
    session_registry: &SharedSessionRegistry,
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

    let team = request
        .payload
        .get("team")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if team.is_empty() {
        return make_error_response(
            &request.request_id,
            "MISSING_PARAMETER",
            "Missing required payload field: 'team'",
        );
    }

    let home = match agent_team_mail_core::home::get_home_dir() {
        Ok(h) => h,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                "INTERNAL_ERROR",
                &format!("Failed to resolve ATM home: {e}"),
            );
        }
    };
    let tracker = state_store.lock().unwrap();
    let tracker_state = tracker.get_state(&agent);
    let last_transition = tracker
        .time_since_transition(&agent)
        .map(format_elapsed_as_iso8601);
    let tracker_meta = tracker.transition_meta(&agent).cloned();
    drop(tracker);

    let Some(member) = load_team_member(&home, &team, &agent) else {
        return match tracker_state {
            Some(state) => make_ok_response(
                &request.request_id,
                serde_json::json!({
                    "state": state.to_string(),
                    "last_transition": last_transition,
                    "reason": tracker_meta.as_ref().map(|m| m.reason.clone()).unwrap_or_else(|| "tracker-only fallback".to_string()),
                    "source": tracker_meta.as_ref().map(|m| m.source.clone()).unwrap_or_else(|| "state_tracker".to_string()),
                }),
            ),
            None => make_error_response(
                &request.request_id,
                "AGENT_NOT_FOUND",
                &format!("Agent '{agent}' is not in team '{team}'"),
            ),
        };
    };

    let session = session_registry
        .lock()
        .unwrap()
        .query_for_team_with_liveness(&team, &agent);
    let canonical = derive_canonical_member_state(
        &team,
        &member,
        tracker_state,
        session.as_ref(),
        tracker_meta.as_ref(),
    );

    make_ok_response(
        &request.request_id,
        serde_json::json!({
            "state": canonical.state.clone(),
            "baseline_state": canonical.state,
            "last_transition": last_transition,
            "reason": canonical.reason,
            "source": canonical.source,
        }),
    )
}

/// Handle the `list-agents` command.
///
/// Payload: `{}`
/// Response: array of `{"agent": "<name>", "state": "<state>"}`
fn handle_list_agents(
    request: &agent_team_mail_core::daemon_client::SocketRequest,
    state_store: &SharedStateStore,
    session_registry: &SharedSessionRegistry,
) -> SocketResponse {
    let team = request.payload.get("team").and_then(|v| v.as_str());
    if let Some(team_name) = team {
        let home = match agent_team_mail_core::home::get_home_dir() {
            Ok(h) => h,
            Err(e) => {
                return make_error_response(
                    &request.request_id,
                    "INTERNAL_ERROR",
                    &format!("Failed to resolve ATM home: {e}"),
                );
            }
        };
        let members = load_team_members(&home, team_name).unwrap_or_default();
        let tracker = state_store.lock().unwrap();
        let mut session_guard = session_registry.lock().unwrap();
        let mut merged_states: std::collections::BTreeMap<String, CanonicalMemberState> =
            std::collections::BTreeMap::new();

        for m in members {
            bootstrap_session_from_member_hint(team_name, &m, &mut session_guard);
            let tracker_state = tracker.get_state(&m.name);
            let tracker_meta = tracker.transition_meta(&m.name);
            let session = session_guard.query_for_team_with_liveness(team_name, &m.name);
            let state = derive_canonical_member_state(
                team_name,
                &m,
                tracker_state,
                session.as_ref(),
                tracker_meta,
            );
            merged_states.insert(m.name.clone(), state);
        }

        for session in session_guard.sessions_for_team_with_liveness(team_name) {
            if merged_states.contains_key(&session.agent_name) {
                continue;
            }
            let tracker_state = tracker.get_state(&session.agent_name);
            let tracker_meta = tracker.transition_meta(&session.agent_name);
            let state =
                derive_unregistered_member_state(team_name, &session, tracker_state, tracker_meta);
            merged_states.insert(session.agent_name.clone(), state);
        }

        let agents: Vec<serde_json::Value> = merged_states
            .into_values()
            .map(|state| {
                serde_json::to_value(state)
                    .unwrap_or_else(|_| serde_json::json!({"agent": "unknown", "state": "unknown"}))
            })
            .collect();
        return make_ok_response(&request.request_id, serde_json::json!(agents));
    }

    let tracker = state_store.lock().unwrap();
    let agents: Vec<serde_json::Value> = tracker
        .all_states()
        .into_keys()
        .map(|agent| {
            let state = tracker
                .get_state(&agent)
                .map(|s| match s {
                    AgentState::Idle => "idle",
                    AgentState::Offline => "offline",
                    AgentState::Active => "active",
                    AgentState::Unknown => "unknown",
                })
                .unwrap_or("unknown");
            serde_json::json!({ "agent": agent, "state": state })
        })
        .collect();
    make_ok_response(&request.request_id, serde_json::json!(agents))
}

fn load_team_members(
    home: &std::path::Path,
    team: &str,
) -> Option<Vec<agent_team_mail_core::schema::AgentMember>> {
    let config_path = home.join(".claude/teams").join(team).join("config.json");
    let content = std::fs::read_to_string(config_path).ok()?;
    let config: agent_team_mail_core::schema::TeamConfig = serde_json::from_str(&content).ok()?;
    Some(config.members)
}

fn load_team_member(
    home: &std::path::Path,
    team: &str,
    agent: &str,
) -> Option<agent_team_mail_core::schema::AgentMember> {
    let members = load_team_members(home, team)?;
    members
        .into_iter()
        .find(|m| m.name == agent || m.agent_id == format!("{agent}@{team}"))
}

fn emit_pid_process_mismatch(
    team: &str,
    agent: &str,
    validation: &PidBackendValidation,
    stage: &str,
) {
    let msg = format!(
        "pid/backend mismatch at {}: agent='{}' backend='{}' expected='{}' actual='{}' pid={}",
        stage,
        agent,
        validation.backend,
        validation.expected_display(),
        validation.actual_display(),
        validation.pid
    );
    warn!("{msg}");
    emit_event_best_effort(EventFields {
        level: "warn",
        source: "atm-daemon",
        action: "PID_PROCESS_MISMATCH",
        team: Some(team.to_string()),
        agent_name: Some(agent.to_string()),
        target: Some(format!("pid:{}", validation.pid)),
        result: Some(stage.to_string()),
        error: Some(msg),
        ..Default::default()
    });
}

fn runtime_for_member(member: &AgentMember) -> Option<String> {
    member.effective_backend_type().and_then(|bt| match bt {
        agent_team_mail_core::schema::BackendType::ClaudeCode => Some("claude".to_string()),
        agent_team_mail_core::schema::BackendType::Codex => Some("codex".to_string()),
        agent_team_mail_core::schema::BackendType::Gemini => Some("gemini".to_string()),
        agent_team_mail_core::schema::BackendType::External
        | agent_team_mail_core::schema::BackendType::Human(_) => None,
    })
}

fn bootstrap_session_from_member_hint(
    team: &str,
    member: &AgentMember,
    session_registry: &mut crate::daemon::session_registry::SessionRegistry,
) {
    if session_registry
        .query_for_team(team, &member.name)
        .is_some()
    {
        return;
    }

    let Some(pid) = roster_process_id(member).filter(|pid| *pid > 1) else {
        return;
    };
    if !agent_team_mail_core::pid::is_pid_alive(pid) {
        return;
    }

    let validation = validate_pid_backend(member, pid);
    if validation.is_alive_mismatch() {
        emit_pid_process_mismatch(team, &member.name, &validation, "bootstrap");
        return;
    }

    let now_ms = chrono::Utc::now().timestamp_millis() as u64;
    let session_id = member
        .session_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("local:{}:{now_ms}:{pid}", member.name));
    let runtime = runtime_for_member(member);
    let runtime_session_id = runtime.as_ref().map(|_| session_id.clone());

    session_registry.upsert_runtime_for_team(
        team,
        &member.name,
        &session_id,
        pid,
        runtime,
        runtime_session_id,
        member.tmux_pane_id.clone(),
        None,
    );
}

fn derive_canonical_member_state(
    team: &str,
    member: &AgentMember,
    tracker_state: Option<AgentState>,
    session: Option<&crate::daemon::session_registry::SessionRecord>,
    tracker_meta: Option<&crate::plugins::worker_adapter::TransitionMeta>,
) -> CanonicalMemberState {
    let agent = member.name.as_str();
    if let Some(session) = session {
        let session_alive = session.state == crate::daemon::session_registry::SessionState::Active;
        if !session_alive {
            return CanonicalMemberState {
                agent: agent.to_string(),
                state: "offline".to_string(),
                activity: "unknown".to_string(),
                session_id: Some(session.session_id.clone()),
                process_id: Some(session.process_id),
                last_alive_at: session.last_alive_at.clone(),
                reason: "session inactive or pid dead".to_string(),
                source: "session_registry".to_string(),
                in_config: true,
            };
        }
        let validation = validate_pid_backend(member, session.process_id);
        if validation.is_alive_mismatch() {
            let mismatch_reason = format!(
                "pid/backend mismatch: backend='{}' expected='{}' actual='{}' pid={}",
                validation.backend,
                validation.expected_display(),
                validation.actual_display(),
                validation.pid
            );
            let already_reported = tracker_meta.is_some_and(|meta| {
                meta.source == "pid_backend_validation" && meta.reason == mismatch_reason
            });
            if !already_reported {
                emit_pid_process_mismatch(team, agent, &validation, "liveness");
            }
            return CanonicalMemberState {
                agent: agent.to_string(),
                state: "offline".to_string(),
                activity: "unknown".to_string(),
                session_id: Some(session.session_id.clone()),
                process_id: Some(session.process_id),
                last_alive_at: session.last_alive_at.clone(),
                reason: mismatch_reason,
                source: "pid_backend_validation".to_string(),
                in_config: true,
            };
        }
        if matches!(tracker_state, Some(AgentState::Idle)) {
            let reason = tracker_meta
                .map(|m| m.reason.clone())
                .unwrap_or_else(|| "idle lifecycle signal".to_string());
            let source = tracker_meta
                .map(|m| m.source.clone())
                .unwrap_or_else(|| "hook_event".to_string());
            return CanonicalMemberState {
                agent: agent.to_string(),
                state: "idle".to_string(),
                activity: "idle".to_string(),
                session_id: Some(session.session_id.clone()),
                process_id: Some(session.process_id),
                last_alive_at: session.last_alive_at.clone(),
                reason,
                source,
                in_config: true,
            };
        }
        return CanonicalMemberState {
            agent: agent.to_string(),
            state: "active".to_string(),
            activity: "busy".to_string(),
            session_id: Some(session.session_id.clone()),
            process_id: Some(session.process_id),
            last_alive_at: session.last_alive_at.clone(),
            reason: "session active with live pid".to_string(),
            source: "session_registry".to_string(),
            in_config: true,
        };
    }

    match tracker_state {
        Some(AgentState::Idle) => CanonicalMemberState {
            agent: agent.to_string(),
            state: "idle".to_string(),
            activity: "idle".to_string(),
            session_id: None,
            process_id: None,
            last_alive_at: None,
            reason: tracker_meta
                .map(|m| m.reason.clone())
                .unwrap_or_else(|| "idle tracker state".to_string()),
            source: tracker_meta
                .map(|m| m.source.clone())
                .unwrap_or_else(|| "state_tracker".to_string()),
            in_config: true,
        },
        Some(AgentState::Active) => CanonicalMemberState {
            agent: agent.to_string(),
            state: "active".to_string(),
            activity: "busy".to_string(),
            session_id: None,
            process_id: None,
            last_alive_at: None,
            reason: tracker_meta
                .map(|m| m.reason.clone())
                .unwrap_or_else(|| "active tracker state".to_string()),
            source: tracker_meta
                .map(|m| m.source.clone())
                .unwrap_or_else(|| "state_tracker".to_string()),
            in_config: true,
        },
        Some(AgentState::Offline) => CanonicalMemberState {
            agent: agent.to_string(),
            state: "offline".to_string(),
            activity: "unknown".to_string(),
            session_id: None,
            process_id: None,
            last_alive_at: None,
            reason: tracker_meta
                .map(|m| m.reason.clone())
                .unwrap_or_else(|| "offline tracker state".to_string()),
            source: tracker_meta
                .map(|m| m.source.clone())
                .unwrap_or_else(|| "state_tracker".to_string()),
            in_config: true,
        },
        Some(AgentState::Unknown) | None => CanonicalMemberState {
            agent: agent.to_string(),
            state: "unknown".to_string(),
            activity: "unknown".to_string(),
            session_id: None,
            process_id: None,
            last_alive_at: None,
            reason: tracker_meta
                .map(|m| m.reason.clone())
                .unwrap_or_else(|| "no lifecycle/session evidence".to_string()),
            source: tracker_meta
                .map(|m| m.source.clone())
                .unwrap_or_else(|| "state_tracker".to_string()),
            in_config: true,
        },
    }
}

fn derive_unregistered_member_state(
    team: &str,
    session: &crate::daemon::session_registry::SessionRecord,
    tracker_state: Option<AgentState>,
    tracker_meta: Option<&crate::plugins::worker_adapter::TransitionMeta>,
) -> CanonicalMemberState {
    if session.state != crate::daemon::session_registry::SessionState::Active {
        return CanonicalMemberState {
            agent: session.agent_name.clone(),
            state: "offline".to_string(),
            activity: "unknown".to_string(),
            session_id: Some(session.session_id.clone()),
            process_id: Some(session.process_id),
            last_alive_at: session.last_alive_at.clone(),
            reason: "session tracked but member missing from config".to_string(),
            source: "session_registry".to_string(),
            in_config: false,
        };
    }

    if matches!(tracker_state, Some(AgentState::Idle)) {
        return CanonicalMemberState {
            agent: session.agent_name.clone(),
            state: "idle".to_string(),
            activity: "idle".to_string(),
            session_id: Some(session.session_id.clone()),
            process_id: Some(session.process_id),
            last_alive_at: session.last_alive_at.clone(),
            reason: tracker_meta
                .map(|m| m.reason.clone())
                .unwrap_or_else(|| "idle lifecycle signal".to_string()),
            source: tracker_meta
                .map(|m| m.source.clone())
                .unwrap_or_else(|| "hook_event".to_string()),
            in_config: false,
        };
    }

    let validation = validate_pid_runtime(session.runtime.as_deref(), session.process_id);
    if validation.is_alive_mismatch() {
        let mismatch_reason = format!(
            "pid/backend mismatch: backend='{}' expected='{}' actual='{}' pid={}",
            validation.backend,
            validation.expected_display(),
            validation.actual_display(),
            validation.pid
        );
        let already_reported = tracker_meta.is_some_and(|meta| {
            meta.source == "pid_backend_validation" && meta.reason == mismatch_reason
        });
        if !already_reported {
            emit_pid_process_mismatch(team, &session.agent_name, &validation, "liveness");
        }
        return CanonicalMemberState {
            agent: session.agent_name.clone(),
            state: "offline".to_string(),
            activity: "unknown".to_string(),
            session_id: Some(session.session_id.clone()),
            process_id: Some(session.process_id),
            last_alive_at: session.last_alive_at.clone(),
            reason: mismatch_reason,
            source: "pid_backend_validation".to_string(),
            in_config: false,
        };
    }

    CanonicalMemberState {
        agent: session.agent_name.clone(),
        state: "active".to_string(),
        activity: "busy".to_string(),
        session_id: Some(session.session_id.clone()),
        process_id: Some(session.process_id),
        last_alive_at: session.last_alive_at.clone(),
        reason: "session tracked but member missing from config".to_string(),
        source: "session_registry".to_string(),
        in_config: false,
    }
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
    use crate::daemon::dedup::DurableDedupeStore;
    use crate::daemon::session_registry::new_session_registry;
    use crate::plugins::worker_adapter::AgentStateTracker;
    use agent_team_mail_core::control::{CONTROL_SCHEMA_VERSION, ControlAction, ControlRequest};
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};
    use serial_test::serial;
    use std::path::Path;
    use std::time::Duration;
    use tempfile::TempDir;

    fn make_store() -> SharedStateStore {
        std::sync::Arc::new(std::sync::Mutex::new(AgentStateTracker::new()))
    }

    fn make_ps() -> SharedPubSubStore {
        new_pubsub_store()
    }

    fn make_sr() -> SharedSessionRegistry {
        new_session_registry()
    }

    fn make_dd() -> (SharedDedupeStore, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("dedup.jsonl");
        let store = DurableDedupeStore::new(path, Duration::from_secs(600), 1000).unwrap();
        (std::sync::Arc::new(std::sync::Mutex::new(store)), dir)
    }

    fn make_dd_in(dir: &tempfile::TempDir) -> SharedDedupeStore {
        let path = dir.path().join("dedup.jsonl");
        let store = DurableDedupeStore::new(path, Duration::from_secs(600), 1000).unwrap();
        std::sync::Arc::new(std::sync::Mutex::new(store))
    }

    fn make_request(command: &str, payload: serde_json::Value) -> SocketRequest {
        SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-test".to_string(),
            command: command.to_string(),
            payload,
        }
    }

    fn write_gh_monitor_config(home: &Path, team: &str) {
        let cfg_dir = home.join(".config/atm");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let config = format!(
            r#"[core]
default_team = "{team}"
identity = "daemon-test"

[plugins.gh_monitor]
enabled = true
team = "{team}"
agent = "gh-monitor"
repo = "agent-team-mail"
poll_interval_secs = 60
"#
        );
        std::fs::write(cfg_dir.join("config.toml"), config).unwrap();
    }

    fn write_repo_gh_monitor_config(repo_dir: &Path, team: &str) {
        std::fs::create_dir_all(repo_dir).unwrap();
        let config = format!(
            r#"[core]
default_team = "{team}"
identity = "daemon-test"

[plugins.gh_monitor]
enabled = true
team = "{team}"
agent = "gh-monitor"
repo = "agent-team-mail"
poll_interval_secs = 60
"#
        );
        std::fs::write(repo_dir.join(".atm.toml"), config).unwrap();
    }

    fn write_invalid_gh_monitor_config(home: &Path, team: &str) {
        let cfg_dir = home.join(".config/atm");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let config = format!(
            r#"[core]
default_team = "{team}"
identity = "daemon-test"

[plugins.gh_monitor]
enabled = true
team = "{team}"
agent = "gh-monitor"
poll_interval_secs = 1
"#
        );
        std::fs::write(cfg_dir.join("config.toml"), config).unwrap();
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: test-only env mutation, guarded by #[serial] on callers.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: test-only env mutation, guarded by #[serial] on callers.
            unsafe {
                match &self.previous {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[cfg(unix)]
    fn install_fake_gh_script(temp: &TempDir, script_body: &str) -> EnvGuard {
        use std::os::unix::fs::PermissionsExt;

        let script_path = temp.path().join("gh");
        std::fs::write(&script_path, script_body).expect("write fake gh script");
        let mut perms = std::fs::metadata(&script_path)
            .expect("stat fake gh script")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).expect("chmod fake gh script");

        let previous_path = std::env::var("PATH").unwrap_or_default();
        let composed = if previous_path.is_empty() {
            temp.path().display().to_string()
        } else {
            format!("{}:{previous_path}", temp.path().display())
        };
        EnvGuard::set("PATH", &composed)
    }

    #[derive(Clone, Default)]
    struct SharedLogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl SharedLogCapture {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap_or_default()
        }
    }

    struct SharedLogWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for SharedLogWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for SharedLogCapture {
        type Writer = SharedLogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            SharedLogWriter(self.0.clone())
        }
    }

    struct HookAuthFixture {
        _temp: TempDir,
        _atm_home_guard: EnvGuard,
    }

    fn write_hook_auth_team_config(
        home_dir: &std::path::Path,
        team: &str,
        lead: &str,
        members: &[&str],
    ) {
        let team_dir = home_dir.join(".claude/teams").join(team);
        std::fs::create_dir_all(&team_dir).unwrap();
        let mut member_values = Vec::new();
        for m in members {
            member_values.push(serde_json::json!({
                "agentId": format!("{m}@{team}"),
                "name": m,
                "agentType": "general-purpose",
                "model": "unknown",
                "joinedAt": 1739284800000u64,
                "cwd": home_dir.to_string_lossy().to_string(),
                "subscriptions": []
            }));
        }
        let config = serde_json::json!({
            "name": team,
            "description": "test team",
            "createdAt": 1739284800000u64,
            "leadAgentId": format!("{lead}@{team}"),
            "leadSessionId": "test-lead-session",
            "members": member_values,
        });
        {
            use std::io::Write;
            let config_path = team_dir.join("config.json");
            let config_bytes = serde_json::to_string_pretty(&config).unwrap();
            let file = std::fs::File::create(&config_path).unwrap();
            let mut writer = std::io::BufWriter::new(&file);
            writer.write_all(config_bytes.as_bytes()).unwrap();
            writer.flush().unwrap();
            file.sync_all().unwrap();
        }
    }

    #[cfg(unix)]
    fn read_team_inbox_messages(
        home_dir: &std::path::Path,
        team: &str,
        agent: &str,
    ) -> Vec<InboxMessage> {
        let path = home_dir
            .join(".claude/teams")
            .join(team)
            .join("inboxes")
            .join(format!("{agent}.json"));
        if !path.exists() {
            return Vec::new();
        }
        serde_json::from_str::<Vec<InboxMessage>>(&std::fs::read_to_string(path).unwrap())
            .unwrap_or_default()
    }

    fn set_member_backend(
        home_dir: &std::path::Path,
        team: &str,
        member_name: &str,
        backend: &str,
    ) {
        let cfg_path = home_dir
            .join(".claude/teams")
            .join(team)
            .join("config.json");
        let mut cfg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cfg_path).unwrap()).unwrap();
        let members = cfg["members"]
            .as_array_mut()
            .expect("members array in team config");
        let member = members
            .iter_mut()
            .find(|m| m["name"].as_str() == Some(member_name))
            .expect("member exists in team config");
        member["externalBackendType"] = serde_json::json!(backend);
        {
            use std::io::Write;
            let cfg_bytes = serde_json::to_string_pretty(&cfg).unwrap();
            let file = std::fs::File::create(&cfg_path).unwrap();
            let mut writer = std::io::BufWriter::new(&file);
            writer.write_all(cfg_bytes.as_bytes()).unwrap();
            writer.flush().unwrap();
            file.sync_all().unwrap();
        }
        // Spin-wait until the updated externalBackendType is readable — macOS APFS VFS
        // page cache may return stale content immediately after write+sync.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            let visible = std::fs::read_to_string(&cfg_path)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| {
                    v["members"]
                        .as_array()
                        .and_then(|arr| {
                            arr.iter().find(|m| m["name"].as_str() == Some(member_name))
                        })
                        .and_then(|m| m["externalBackendType"].as_str().map(str::to_string))
                })
                .is_some_and(|t| t == backend);
            if visible {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "set_member_backend: externalBackendType='{}' not readable after 500ms: {}",
                backend,
                cfg_path.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn test_member(name: &str, backend: &str) -> agent_team_mail_core::schema::AgentMember {
        serde_json::from_value(serde_json::json!({
            "agentId": format!("{name}@atm-dev"),
            "name": name,
            "agentType": "general-purpose",
            "model": "unknown",
            "joinedAt": 1739284800000u64,
            "cwd": ".",
            "subscriptions": [],
            "externalBackendType": backend
        }))
        .expect("valid member json")
    }

    fn test_active_session(
        team: &str,
        agent: &str,
        runtime: Option<&str>,
    ) -> crate::daemon::session_registry::SessionRecord {
        crate::daemon::session_registry::SessionRecord {
            team: team.to_string(),
            agent_name: agent.to_string(),
            session_id: format!("{agent}-sess"),
            process_id: std::process::id(),
            state: crate::daemon::session_registry::SessionState::Active,
            updated_at: "2026-03-05T00:00:00Z".to_string(),
            last_alive_at: Some("2026-03-05T00:00:00Z".to_string()),
            runtime: runtime.map(str::to_string),
            runtime_session_id: None,
            pane_id: None,
            runtime_home: None,
        }
    }

    fn setup_hook_auth_fixture(team: &str, lead: &str, members: &[&str]) -> HookAuthFixture {
        let temp = TempDir::new().unwrap();
        let atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_hook_auth_team_config(temp.path(), team, lead, members);

        // Spin-wait until config is readable — macOS APFS directory entry visibility
        // is not guaranteed immediately after write+sync without this verification.
        let config_path = temp
            .path()
            .join(".claude/teams")
            .join(team)
            .join("config.json");
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            if std::fs::read_to_string(&config_path).is_ok() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fixture config not readable after 500ms: {}",
                config_path.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        HookAuthFixture {
            _temp: temp,
            _atm_home_guard: atm_home_guard,
        }
    }

    #[cfg(unix)]
    async fn handle_hook_event_with_transient_retry(
        req_json: &str,
        store: &SharedStateStore,
        sr: &SharedSessionRegistry,
    ) -> agent_team_mail_core::daemon_client::SocketResponse {
        let mut attempts = 0u8;
        loop {
            attempts += 1;
            let resp = handle_hook_event_command(req_json, store, sr).await;
            let retry = resp
                .payload
                .as_ref()
                .and_then(|p| p.get("processed").and_then(|v| v.as_bool()))
                .is_some_and(|processed| !processed)
                && resp
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("reason").and_then(|v| v.as_str()))
                    .is_some_and(|reason| reason.contains("team config not found"));
            if retry && attempts < 4 {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                continue;
            }
            return resp;
        }
    }

    #[cfg(unix)]
    async fn handle_hook_event_command_with_dedup_retry(
        req_json: &str,
        store: &SharedStateStore,
        sr: &SharedSessionRegistry,
        dd: &SharedDedupeStore,
    ) -> agent_team_mail_core::daemon_client::SocketResponse {
        let mut attempts = 0u8;
        loop {
            attempts += 1;
            let resp = handle_hook_event_command_with_dedup(req_json, store, sr, dd).await;
            let retry = resp
                .payload
                .as_ref()
                .and_then(|p| p.get("processed").and_then(|v| v.as_bool()))
                .is_some_and(|processed| !processed)
                && resp
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("reason").and_then(|v| v.as_str()))
                    .is_some_and(|reason| reason.contains("team config not found"));
            if retry && attempts < 4 {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                continue;
            }
            return resp;
        }
    }

    #[test]
    #[serial]
    fn test_agent_state_not_found() {
        let _fixture = setup_hook_auth_fixture("t", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        let req = make_request(
            "agent-state",
            serde_json::json!({"agent": "ghost", "team": "t"}),
        );
        let resp = handle_agent_state(&req, &store, &sr);
        assert_eq!(resp.status, "error");
        let err = resp.error.unwrap();
        assert_eq!(err.code, "AGENT_NOT_FOUND");
    }

    #[test]
    #[serial]
    fn test_agent_state_found() {
        use crate::plugins::worker_adapter::AgentState;

        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Idle);
        }

        let req = make_request(
            "agent-state",
            serde_json::json!({"agent": "arch-ctm", "team": "atm-dev"}),
        );
        let resp = handle_agent_state(&req, &store, &sr);
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["state"].as_str().unwrap(), "idle");
    }

    #[test]
    fn test_list_agents_empty() {
        let store = make_store();
        let sr = make_sr();
        let req = make_request("list-agents", serde_json::json!({}));
        let resp = handle_list_agents(&req, &store, &sr);
        assert_eq!(resp.status, "ok");
        let agents = resp.payload.unwrap();
        assert!(agents.as_array().unwrap().is_empty());
    }

    #[test]
    fn test_list_agents_with_entries() {
        use crate::plugins::worker_adapter::AgentState;

        let store = make_store();
        let sr = make_sr();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Idle);
            tracker.register_agent("worker-1");
        }

        let req = make_request("list-agents", serde_json::json!({}));
        let resp = handle_list_agents(&req, &store, &sr);
        assert_eq!(resp.status, "ok");
        let agents = resp.payload.unwrap();
        let arr = agents.as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    #[serial]
    fn test_list_agents_with_entries_includes_state_metadata() {
        use crate::plugins::worker_adapter::AgentState;

        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state_with_context(
                "arch-ctm",
                AgentState::Idle,
                "hook after-agent",
                "hook_watcher",
            );
        }

        let req = make_request("list-agents", serde_json::json!({"team": "atm-dev"}));
        let resp = handle_list_agents(&req, &store, &sr);
        assert_eq!(resp.status, "ok");
        let agents = resp.payload.unwrap();
        let arr = agents.as_array().unwrap();
        assert!(!arr.is_empty());
        let arch = arr
            .iter()
            .find(|a| a["agent"].as_str() == Some("arch-ctm"))
            .expect("arch-ctm entry missing");
        assert_eq!(arch["state"].as_str(), Some("idle"));
        assert!(arch["reason"].as_str().is_some());
        assert!(arch["source"].as_str().is_some());
        assert!(
            arch.get("in_config").is_none() || arch["in_config"].as_bool() == Some(true),
            "configured members should serialize in_config as omitted (default true) or explicit true"
        );
    }

    #[test]
    #[serial]
    fn test_list_agents_team_scope_includes_daemon_only_sessions_as_unregistered() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        {
            let mut reg = sr.lock().unwrap();
            reg.upsert_runtime_for_team(
                "atm-dev",
                "arch-ctm",
                "sess-ghost-1",
                std::process::id(),
                None,
                None,
                None,
                None,
            );
        }

        let req = make_request("list-agents", serde_json::json!({"team": "atm-dev"}));
        let resp = handle_list_agents(&req, &store, &sr);
        assert_eq!(resp.status, "ok");
        let arr = resp.payload.unwrap().as_array().unwrap().clone();

        let ghost = arr
            .iter()
            .find(|a| a["agent"].as_str() == Some("arch-ctm"))
            .expect("daemon-only member missing");
        assert_eq!(ghost["in_config"].as_bool(), Some(false));
        assert_eq!(ghost["state"].as_str(), Some("active"));
    }

    #[test]
    fn test_derive_canonical_member_state_prefers_live_session_over_offline_tracker_state() {
        let member = test_member("arch-ctm", "external");
        let session = test_active_session("atm-dev", "arch-ctm", Some("codex"));

        let state = derive_canonical_member_state(
            "atm-dev",
            &member,
            Some(AgentState::Offline),
            Some(&session),
            None,
        );

        assert_eq!(state.state, "active");
        assert_eq!(state.activity, "busy");
        assert_eq!(state.source, "session_registry");
    }

    #[test]
    fn test_derive_canonical_member_state_active_tracker_without_session_stays_active() {
        let member = test_member("arch-ctm", "external");
        let state =
            derive_canonical_member_state("atm-dev", &member, Some(AgentState::Active), None, None);
        assert_eq!(state.state, "active");
        assert_eq!(state.activity, "busy");
        assert_eq!(state.source, "state_tracker");
    }

    #[test]
    fn test_derive_unregistered_member_state_offline_tracker_with_live_session_prefers_session() {
        let session = test_active_session("atm-dev", "ghost-agent", None);
        let state =
            derive_unregistered_member_state("atm-dev", &session, Some(AgentState::Offline), None);
        assert_eq!(state.state, "active");
        assert_eq!(state.activity, "busy");
        assert_eq!(state.source, "session_registry");
        assert!(!state.in_config);
    }

    #[cfg(unix)]
    #[test]
    fn test_derive_unregistered_member_state_runtime_pid_mismatch_marks_offline() {
        let session = test_active_session("atm-dev", "ghost-codex", Some("codex"));
        let state =
            derive_unregistered_member_state("atm-dev", &session, Some(AgentState::Active), None);
        assert_eq!(state.state, "offline");
        assert_eq!(state.source, "pid_backend_validation");
        assert!(state.reason.contains("backend='codex'"));
        assert!(!state.in_config);
    }

    #[test]
    #[serial]
    fn test_list_agents_bootstraps_session_from_config_process_hint() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let home = std::env::var("ATM_HOME").expect("ATM_HOME set by fixture");
        let config_path = std::path::Path::new(&home).join(".claude/teams/atm-dev/config.json");
        let mut cfg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        let members = cfg["members"].as_array_mut().unwrap();
        let target = members
            .iter_mut()
            .find(|m| m["name"].as_str() == Some("arch-ctm"))
            .expect("arch-ctm in config");
        target["processId"] = serde_json::json!(std::process::id());
        target["sessionId"] = serde_json::json!("hint-session-1");
        target["externalBackendType"] = serde_json::json!("external");
        {
            use std::io::Write;
            let content = serde_json::to_string_pretty(&cfg).unwrap();
            let file = std::fs::File::create(&config_path).unwrap();
            let mut writer = std::io::BufWriter::new(&file);
            writer.write_all(content.as_bytes()).unwrap();
            writer.flush().unwrap();
            file.sync_all().unwrap();
        }

        let store = make_store();
        let sr = make_sr();
        let req = make_request("list-agents", serde_json::json!({"team": "atm-dev"}));
        let resp = handle_list_agents(&req, &store, &sr);
        assert_eq!(resp.status, "ok");
        let arr = resp.payload.unwrap().as_array().unwrap().clone();
        let member = arr
            .iter()
            .find(|a| a["agent"].as_str() == Some("arch-ctm"))
            .expect("arch-ctm entry missing");
        assert_eq!(member["state"].as_str(), Some("active"));
        assert_eq!(member["session_id"].as_str(), Some("hint-session-1"));

        let reg = sr.lock().unwrap();
        let session = reg
            .query_for_team("atm-dev", "arch-ctm")
            .expect("session registry upserted");
        assert_eq!(session.session_id, "hint-session-1");
        assert_eq!(session.process_id, std::process::id());
    }

    #[test]
    #[serial]
    fn test_team_scoped_list_agents_isolated_between_teams() {
        use crate::plugins::worker_adapter::AgentState;

        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_hook_auth_team_config(temp.path(), "team-a", "team-lead-a", &["team-lead-a", "a1"]);
        write_hook_auth_team_config(temp.path(), "team-b", "team-lead-b", &["team-lead-b", "b1"]);

        let store = make_store();
        let sr = make_sr();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("a1");
            tracker.set_state("a1", AgentState::Idle);
            tracker.register_agent("b1");
            tracker.set_state("b1", AgentState::Idle);
        }

        let req_a = make_request("list-agents", serde_json::json!({"team": "team-a"}));
        let resp_a = handle_list_agents(&req_a, &store, &sr);
        assert_eq!(resp_a.status, "ok");
        let arr_a = resp_a.payload.unwrap().as_array().unwrap().clone();
        assert!(arr_a.iter().any(|v| v["agent"].as_str() == Some("a1")));
        assert!(!arr_a.iter().any(|v| v["agent"].as_str() == Some("b1")));

        let req_b = make_request("list-agents", serde_json::json!({"team": "team-b"}));
        let resp_b = handle_list_agents(&req_b, &store, &sr);
        assert_eq!(resp_b.status, "ok");
        let arr_b = resp_b.payload.unwrap().as_array().unwrap().clone();
        assert!(arr_b.iter().any(|v| v["agent"].as_str() == Some("b1")));
        assert!(!arr_b.iter().any(|v| v["agent"].as_str() == Some("a1")));
    }

    #[test]
    #[serial]
    fn test_team_scoped_list_agents_isolated_after_registry_reload() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_hook_auth_team_config(temp.path(), "team-a", "team-lead-a", &["team-lead-a", "a1"]);
        write_hook_auth_team_config(temp.path(), "team-b", "team-lead-b", &["team-lead-b", "b1"]);
        // Avoid test-process backend mismatch (cargo test != claude) so this
        // test exercises team scoping/reload behavior only.
        set_member_backend(temp.path(), "team-a", "a1", "external");
        set_member_backend(temp.path(), "team-b", "b1", "external");

        let persist_path = temp.path().join(".claude/daemon/session-registry.json");
        {
            let mut seeded = crate::daemon::session_registry::SessionRegistry::with_persist_path(
                persist_path.clone(),
            );
            seeded.upsert_for_team("team-a", "a1", "sess-a", std::process::id());
            seeded.upsert_for_team("team-b", "b1", "sess-b", std::process::id());
        }

        let store = make_store();
        let sr = std::sync::Arc::new(std::sync::Mutex::new(
            crate::daemon::session_registry::SessionRegistry::load_or_new(persist_path),
        ));

        let req_a = make_request("list-agents", serde_json::json!({"team": "team-a"}));
        let resp_a = handle_list_agents(&req_a, &store, &sr);
        assert_eq!(resp_a.status, "ok");
        let arr_a = resp_a.payload.unwrap().as_array().unwrap().clone();
        let a1 = arr_a
            .iter()
            .find(|v| v["agent"].as_str() == Some("a1"))
            .expect("a1 should be listed for team-a after restart load");
        assert_eq!(a1["state"].as_str(), Some("active"));
        assert_eq!(a1["session_id"].as_str(), Some("sess-a"));
        assert!(!arr_a.iter().any(|v| v["agent"].as_str() == Some("b1")));

        let req_b = make_request("list-agents", serde_json::json!({"team": "team-b"}));
        let resp_b = handle_list_agents(&req_b, &store, &sr);
        assert_eq!(resp_b.status, "ok");
        let arr_b = resp_b.payload.unwrap().as_array().unwrap().clone();
        let b1 = arr_b
            .iter()
            .find(|v| v["agent"].as_str() == Some("b1"))
            .expect("b1 should be listed for team-b after restart load");
        assert_eq!(b1["state"].as_str(), Some("active"));
        assert_eq!(b1["session_id"].as_str(), Some("sess-b"));
        assert!(!arr_b.iter().any(|v| v["agent"].as_str() == Some("a1")));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_restart_partial_lifecycle_teammate_idle_converges_deterministically() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_hook_auth_team_config(
            temp.path(),
            "atm-dev",
            "team-lead",
            &["team-lead", "arch-ctm"],
        );
        // Keep this restart test focused on lifecycle convergence by avoiding
        // backend validation against the cargo test process.
        set_member_backend(temp.path(), "atm-dev", "arch-ctm", "external");

        let persist_path = temp.path().join(".claude/daemon/session-registry.json");
        {
            let mut seeded = crate::daemon::session_registry::SessionRegistry::with_persist_path(
                persist_path.clone(),
            );
            seeded.upsert_for_team("atm-dev", "arch-ctm", "sess-partial", std::process::id());
        }

        // Simulate daemon restart: rebuild registry from persisted state while
        // state tracker starts empty.
        let store = make_store();
        let sr = std::sync::Arc::new(std::sync::Mutex::new(
            crate::daemon::session_registry::SessionRegistry::load_or_new(persist_path),
        ));

        let req_json = r#"{"version":1,"request_id":"r-restart-idle","command":"hook-event","payload":{"event":"teammate_idle","agent":"arch-ctm","session_id":"sess-partial","team":"atm-dev"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());

        let req = make_request("list-agents", serde_json::json!({"team": "atm-dev"}));
        let list_resp = handle_list_agents(&req, &store, &sr);
        assert_eq!(list_resp.status, "ok");
        let arr = list_resp.payload.unwrap().as_array().unwrap().clone();
        let arch = arr
            .iter()
            .find(|v| v["agent"].as_str() == Some("arch-ctm"))
            .expect("arch-ctm should be listed");
        assert_eq!(arch["state"].as_str(), Some("idle"));
        assert_eq!(arch["activity"].as_str(), Some("idle"));
        assert_eq!(arch["session_id"].as_str(), Some("sess-partial"));
    }

    #[test]
    fn test_launch_command_missing_agent() {
        // parse_and_dispatch receives a "launch" command — it should return INTERNAL_ERROR
        // because the async path should have handled it, but the payload may be inspected.
        let store = make_store();
        let ps = make_ps();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r1","command":"launch","payload":{"agent":"","team":"atm-dev","command":"codex","timeout_secs":30,"env_vars":{}}}"#;
        let resp =
            parse_and_dispatch(req_json, &store, &ps, &sr, &new_stream_state_store()).unwrap();
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
    #[cfg(unix)]
    fn test_is_gh_command_detection() {
        assert!(is_gh_monitor_command(
            r#"{"version":1,"request_id":"r1","command":"gh-monitor","payload":{}}"#
        ));
        assert!(is_gh_monitor_command(
            r#"{"version":1,"request_id":"r1","command": "gh-monitor","payload":{}}"#
        ));
        assert!(is_gh_status_command(
            r#"{"version":1,"request_id":"r1","command":"gh-status","payload":{}}"#
        ));
        assert!(is_gh_status_command(
            r#"{"version":1,"request_id":"r1","command": "gh-status","payload":{}}"#
        ));
        assert!(is_gh_monitor_control_command(
            r#"{"version":1,"request_id":"r1","command":"gh-monitor-control","payload":{}}"#
        ));
        assert!(is_gh_monitor_health_command(
            r#"{"version":1,"request_id":"r1","command":"gh-monitor-health","payload":{}}"#
        ));
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn test_gh_monitor_pr_timeout_zero_returns_ci_not_started_and_status_roundtrip() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_gh_monitor_config(temp.path(), "atm-dev");
        let req_json = r#"{"version":1,"request_id":"r-gh-1","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"pr","target":"123","start_timeout_secs":0}}"#;
        let monitor_resp = handle_gh_monitor_command(req_json, temp.path()).await;
        assert_eq!(monitor_resp.status, "ok");
        let status_payload = monitor_resp.payload.unwrap();
        assert_eq!(status_payload["state"].as_str(), Some("ci_not_started"));
        assert_eq!(status_payload["target_kind"].as_str(), Some("pr"));
        assert_eq!(status_payload["target"].as_str(), Some("123"));

        let status_req = r#"{"version":1,"request_id":"r-gh-2","command":"gh-status","payload":{"team":"atm-dev","target_kind":"pr","target":"123"}}"#;
        let status_resp = handle_gh_status_command(status_req, temp.path()).await;
        assert_eq!(status_resp.status, "ok");
        let status = status_resp.payload.unwrap();
        assert_eq!(status["state"].as_str(), Some("ci_not_started"));
        assert_eq!(status["target_kind"].as_str(), Some("pr"));
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn test_gh_monitor_workflow_requires_reference() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_gh_monitor_config(temp.path(), "atm-dev");
        let req_json = r#"{"version":1,"request_id":"r-gh-3","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"workflow","target":"ci"}}"#;
        let resp = handle_gh_monitor_command(req_json, temp.path()).await;
        assert_eq!(resp.status, "error");
        let err = resp.error.unwrap();
        assert_eq!(err.code, "MISSING_PARAMETER");
        assert!(err.message.contains("reference"));
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn test_preflight_dirty_pr_skips_polling() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_gh_monitor_config(temp.path(), "atm-dev");
        write_hook_auth_team_config(temp.path(), "atm-dev", "team-lead", &["team-lead"]);
        std::fs::create_dir_all(temp.path().join(".claude/teams/atm-dev/inboxes")).unwrap();
        let run_list_marker = temp.path().join("run-list-marker.txt");
        let _marker_guard = EnvGuard::set(
            "ATM_GH_RUN_LIST_MARKER",
            run_list_marker.to_string_lossy().as_ref(),
        );
        let _path_guard = install_fake_gh_script(
            &temp,
            r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  echo '{"mergeStateStatus":"DIRTY","url":"https://github.com/o/r/pull/123"}'
  exit 0
fi
if [ "$1" = "run" ] && [ "$2" = "list" ]; then
  echo "called" > "${ATM_GH_RUN_LIST_MARKER}"
  echo '[{"databaseId":424242}]'
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 1
"#,
        );

        let req_json = r#"{"version":1,"request_id":"r-gh-preflight-dirty","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"pr","target":"123","start_timeout_secs":30}}"#;
        let resp = handle_gh_monitor_command(req_json, temp.path()).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["state"].as_str(), Some("merge_conflict"));
        assert_eq!(payload["run_id"], serde_json::Value::Null);
        assert!(
            !run_list_marker.exists(),
            "preflight DIRTY must skip CI polling"
        );

        let inbox = read_team_inbox_messages(temp.path(), "atm-dev", "team-lead");
        assert!(
            inbox.iter().any(|msg| {
                msg.text.contains("classification: merge_conflict")
                    && msg.text.contains("status: merge_conflict")
                    && msg.text.contains("merge_state_status: DIRTY")
                    && msg.text.contains("pr_url: https://github.com/o/r/pull/123")
            }),
            "team lead should receive merge_conflict alert with required fields"
        );
        assert!(
            !inbox.iter().any(|msg| {
                msg.text.contains("classification: ci_not_started")
                    || msg.text.contains("[ci_not_started]")
            }),
            "DIRTY preflight must suppress ci_not_started alerts"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn test_clean_pr_proceeds_to_polling() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_gh_monitor_config(temp.path(), "atm-dev");
        write_hook_auth_team_config(temp.path(), "atm-dev", "team-lead", &["team-lead"]);
        std::fs::create_dir_all(temp.path().join(".claude/teams/atm-dev/inboxes")).unwrap();
        let _path_guard = install_fake_gh_script(
            &temp,
            r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ] && [ "$5" = "mergeStateStatus,url" ]; then
  echo '{"mergeStateStatus":"CLEAN","url":"https://github.com/o/r/pull/123"}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "view" ] && [ "$5" = "headRefName,headRefOid,createdAt" ]; then
  echo '{"headRefName":"feature/mock","headRefOid":"abcdef1234567890","createdAt":"2026-03-06T00:00:00Z"}'
  exit 0
fi
if [ "$1" = "run" ] && [ "$2" = "list" ]; then
  echo '[{"databaseId":424242,"headSha":"abcdef1234567890","createdAt":"2026-03-06T00:05:00Z"}]'
  exit 0
fi
if [ "$1" = "run" ] && [ "$2" = "view" ]; then
  echo '{"databaseId":424242,"name":"ci","status":"completed","conclusion":"success","headBranch":"feature/mock","headSha":"abcdef1234567890","url":"https://github.com/o/r/actions/runs/424242","jobs":[{"databaseId":1,"name":"tests","status":"completed","conclusion":"success","startedAt":"2026-03-06T00:00:00Z","completedAt":"2026-03-06T00:00:10Z","steps":[],"url":"https://github.com/o/r/actions/runs/424242/job/1"}],"attempt":1,"pullRequests":[]}'
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 1
"#,
        );

        let req_json = r#"{"version":1,"request_id":"r-gh-preflight-clean","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"pr","target":"123","start_timeout_secs":30}}"#;
        let resp = handle_gh_monitor_command(req_json, temp.path()).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["state"].as_str(), Some("monitoring"));
        assert_eq!(payload["run_id"].as_u64(), Some(424242));

        let inbox = read_team_inbox_messages(temp.path(), "atm-dev", "team-lead");
        assert!(
            !inbox
                .iter()
                .any(|msg| msg.text.contains("classification: merge_conflict")),
            "clean preflight should not emit merge_conflict alerts"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn test_post_completion_dirty_check() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_hook_auth_team_config(temp.path(), "atm-dev", "team-lead", &["team-lead"]);
        std::fs::create_dir_all(temp.path().join(".claude/teams/atm-dev/inboxes")).unwrap();
        let _path_guard = install_fake_gh_script(
            &temp,
            r#"#!/bin/sh
if [ "$1" = "run" ] && [ "$2" = "view" ]; then
  echo '{"databaseId":42,"name":"ci","status":"completed","conclusion":"success","headBranch":"feature/mock","headSha":"abcdef1234567890","url":"https://github.com/o/r/actions/runs/42","jobs":[{"databaseId":1,"name":"tests","status":"completed","conclusion":"success","startedAt":"2026-03-06T00:00:00Z","completedAt":"2026-03-06T00:00:10Z","steps":[],"url":"https://github.com/o/r/actions/runs/42/job/1"}],"attempt":1,"pullRequests":[]}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "view" ] && [ "$5" = "mergeStateStatus,url" ]; then
  echo '{"mergeStateStatus":"DIRTY","url":"https://github.com/o/r/pull/123"}'
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 1
"#,
        );

        let status_seed = GhMonitorStatus {
            team: "atm-dev".to_string(),
            configured: true,
            enabled: true,
            config_source: None,
            config_path: None,
            target_kind: GhMonitorTargetKind::Pr,
            target: "123".to_string(),
            state: "monitoring".to_string(),
            run_id: Some(42),
            reference: None,
            updated_at: chrono::Utc::now().to_rfc3339(),
            message: None,
        };
        let gh_request = GhMonitorRequest {
            team: "atm-dev".to_string(),
            target_kind: GhMonitorTargetKind::Pr,
            target: "123".to_string(),
            reference: None,
            start_timeout_secs: Some(120),
            config_cwd: None,
        };

        monitor_gh_run(temp.path(), &status_seed, &gh_request, 42)
            .await
            .expect("monitor_gh_run should complete");

        let inbox = read_team_inbox_messages(temp.path(), "atm-dev", "team-lead");
        assert!(
            inbox.iter().any(|msg| {
                msg.text.contains("classification: merge_conflict")
                    && msg.text.contains("status: merge_conflict")
                    && msg.text.contains("merge_state_status: DIRTY")
                    && msg.text.contains("pr_url: https://github.com/o/r/pull/123")
                    && msg.text.contains("run_conclusion: success")
            }),
            "post-terminal DIRTY check must emit merge_conflict alert with run_conclusion"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn test_post_completion_clean_check_emits_no_merge_conflict_alert() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_hook_auth_team_config(temp.path(), "atm-dev", "team-lead", &["team-lead"]);
        std::fs::create_dir_all(temp.path().join(".claude/teams/atm-dev/inboxes")).unwrap();
        let _path_guard = install_fake_gh_script(
            &temp,
            r#"#!/bin/sh
if [ "$1" = "run" ] && [ "$2" = "view" ]; then
  echo '{"databaseId":42,"name":"ci","status":"completed","conclusion":"success","headBranch":"feature/mock","headSha":"abcdef1234567890","url":"https://github.com/o/r/actions/runs/42","jobs":[{"databaseId":1,"name":"tests","status":"completed","conclusion":"success","startedAt":"2026-03-06T00:00:00Z","completedAt":"2026-03-06T00:00:10Z","steps":[],"url":"https://github.com/o/r/actions/runs/42/job/1"}],"attempt":1,"pullRequests":[]}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "view" ] && [ "$5" = "mergeStateStatus,url" ]; then
  echo '{"mergeStateStatus":"CLEAN","url":"https://github.com/o/r/pull/123"}'
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 1
"#,
        );

        let status_seed = GhMonitorStatus {
            team: "atm-dev".to_string(),
            configured: true,
            enabled: true,
            config_source: None,
            config_path: None,
            target_kind: GhMonitorTargetKind::Pr,
            target: "123".to_string(),
            state: "monitoring".to_string(),
            run_id: Some(42),
            reference: None,
            updated_at: chrono::Utc::now().to_rfc3339(),
            message: None,
        };
        let gh_request = GhMonitorRequest {
            team: "atm-dev".to_string(),
            target_kind: GhMonitorTargetKind::Pr,
            target: "123".to_string(),
            reference: None,
            start_timeout_secs: Some(120),
            config_cwd: None,
        };

        monitor_gh_run(temp.path(), &status_seed, &gh_request, 42)
            .await
            .expect("monitor_gh_run should complete");

        let inbox = read_team_inbox_messages(temp.path(), "atm-dev", "team-lead");
        assert!(
            !inbox
                .iter()
                .any(|msg| msg.text.contains("classification: merge_conflict")),
            "post-terminal CLEAN check must not emit merge_conflict alert"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn test_terminal_failure_bypasses_progress_throttle_window() {
        let temp = TempDir::new().unwrap();
        let counter_path = temp.path().join("gh-counter.txt");
        let _counter_guard = EnvGuard::set(
            "ATM_GH_COUNTER_FILE",
            counter_path.to_string_lossy().as_ref(),
        );
        let _path_guard = install_fake_gh_script(
            &temp,
            r#"#!/bin/sh
COUNTER_FILE="${ATM_GH_COUNTER_FILE}"
count=0
if [ -f "$COUNTER_FILE" ]; then
  count=$(cat "$COUNTER_FILE")
fi
count=$((count + 1))
echo "$count" > "$COUNTER_FILE"

if [ "$1" = "run" ] && [ "$2" = "view" ]; then
  if [ "$count" -eq 1 ]; then
    echo '{"databaseId":42,"name":"ci","status":"in_progress","conclusion":null,"headBranch":"develop","headSha":"abcdef1234567890","url":"https://github.com/o/r/actions/runs/42","jobs":[{"databaseId":11,"name":"clippy","status":"completed","conclusion":"success","startedAt":"2026-03-06T00:00:00Z","completedAt":"2026-03-06T00:00:10Z","steps":[],"url":"https://github.com/o/r/actions/runs/42/job/11"},{"databaseId":12,"name":"tests","status":"in_progress","conclusion":null,"startedAt":"2026-03-06T00:00:00Z","completedAt":null,"steps":[],"url":"https://github.com/o/r/actions/runs/42/job/12"}],"attempt":1,"pullRequests":[]}'
  else
    echo '{"databaseId":42,"name":"ci","status":"completed","conclusion":"failure","headBranch":"develop","headSha":"abcdef1234567890","url":"https://github.com/o/r/actions/runs/42","jobs":[{"databaseId":11,"name":"clippy","status":"completed","conclusion":"success","startedAt":"2026-03-06T00:00:00Z","completedAt":"2026-03-06T00:00:10Z","steps":[],"url":"https://github.com/o/r/actions/runs/42/job/11"},{"databaseId":12,"name":"tests","status":"completed","conclusion":"failure","startedAt":"2026-03-06T00:00:00Z","completedAt":"2026-03-06T00:00:20Z","steps":[{"name":"suite","status":"completed","conclusion":"failure"}],"url":"https://github.com/o/r/actions/runs/42/job/12"}],"attempt":1,"pullRequests":[]}'
  fi
  exit 0
fi

echo "unsupported fake gh invocation: $*" >&2
exit 1
"#,
        );

        let status_seed = GhMonitorStatus {
            team: "atm-dev".to_string(),
            configured: true,
            enabled: true,
            config_source: None,
            config_path: None,
            target_kind: GhMonitorTargetKind::Pr,
            target: "123".to_string(),
            state: "tracking".to_string(),
            run_id: Some(42),
            reference: None,
            updated_at: chrono::Utc::now().to_rfc3339(),
            message: None,
        };
        let gh_request = GhMonitorRequest {
            team: "atm-dev".to_string(),
            target_kind: GhMonitorTargetKind::Pr,
            target: "123".to_string(),
            reference: None,
            start_timeout_secs: Some(120),
            config_cwd: None,
        };

        let started = std::time::Instant::now();
        monitor_gh_run(temp.path(), &status_seed, &gh_request, 42)
            .await
            .expect("monitor_gh_run should complete");
        let elapsed = started.elapsed();

        // First poll emits progress and sleeps 5s. The second poll is terminal failure
        // and must bypass the 60s progress throttle window.
        assert!(
            elapsed < std::time::Duration::from_secs(15),
            "terminal update should bypass progress throttle, elapsed={elapsed:?}"
        );

        let state_map = load_gh_monitor_state_map(temp.path()).expect("state map");
        let key = gh_monitor_key("atm-dev", GhMonitorTargetKind::Pr, "123", None);
        let terminal = state_map.get(&key).expect("status entry");
        assert_eq!(terminal.state, "failure");
    }

    #[test]
    #[cfg(unix)]
    fn test_should_emit_progress_rate_limited_to_one_minute() {
        let now = std::time::Instant::now();
        assert!(should_emit_progress(None, now));
        assert!(!should_emit_progress(
            Some(now - std::time::Duration::from_secs(59)),
            now
        ));
        assert!(should_emit_progress(
            Some(now - std::time::Duration::from_secs(60)),
            now
        ));
    }

    #[test]
    #[cfg(unix)]
    fn test_format_summary_table_contains_required_columns() {
        let run = GhRunView {
            database_id: 42,
            name: "ci".to_string(),
            status: "completed".to_string(),
            conclusion: Some("success".to_string()),
            head_branch: "develop".to_string(),
            head_sha: "abcdef1234567890".to_string(),
            url: "https://github.com/o/r/actions/runs/42".to_string(),
            jobs: vec![GhRunJob {
                database_id: 1,
                name: "clippy".to_string(),
                status: "completed".to_string(),
                conclusion: Some("success".to_string()),
                started_at: Some("2026-03-06T00:00:00Z".to_string()),
                completed_at: Some("2026-03-06T00:00:10Z".to_string()),
                steps: Vec::new(),
                url: Some("https://github.com/o/r/actions/runs/42/job/1".to_string()),
            }],
            attempt: Some(1),
            pull_requests: Vec::new(),
        };

        let table = format_summary_table(&run);
        assert!(table.contains("| Job/Test | Status | Runtime |"));
        assert!(table.contains("| clippy | success |"));
    }

    #[test]
    #[cfg(unix)]
    fn test_derive_pr_url_prefers_pr_target_fallback() {
        let run = GhRunView {
            database_id: 42,
            name: "ci".to_string(),
            status: "completed".to_string(),
            conclusion: Some("failure".to_string()),
            head_branch: "feature/x".to_string(),
            head_sha: "abcdef1234567890".to_string(),
            url: "https://github.com/o/r/actions/runs/42".to_string(),
            jobs: Vec::new(),
            attempt: Some(1),
            pull_requests: Vec::new(),
        };
        let status_seed = GhMonitorStatus {
            team: "atm-dev".to_string(),
            configured: true,
            enabled: true,
            config_source: None,
            config_path: None,
            target_kind: GhMonitorTargetKind::Pr,
            target: "123".to_string(),
            state: "monitoring".to_string(),
            run_id: Some(42),
            reference: None,
            updated_at: "2026-03-06T00:00:00Z".to_string(),
            message: None,
        };
        let request = GhMonitorRequest {
            team: "atm-dev".to_string(),
            target_kind: GhMonitorTargetKind::Pr,
            target: "123".to_string(),
            reference: None,
            start_timeout_secs: Some(120),
            config_cwd: None,
        };
        let pr_url = derive_pr_url(&run, &status_seed, &request);
        assert_eq!(pr_url.as_deref(), Some("https://github.com/o/r/pull/123"));
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn test_wait_for_pr_run_start_success_path_finds_run() {
        let temp = TempDir::new().unwrap();
        let _path_guard = install_fake_gh_script(
            &temp,
            r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  echo '{"headRefName":"feature/mock","headRefOid":"sha-pr-123","createdAt":"2026-03-06T00:00:00Z"}'
  exit 0
fi
if [ "$1" = "run" ] && [ "$2" = "list" ]; then
  echo '[{"databaseId":111111,"headSha":"sha-older","createdAt":"2026-03-05T23:59:59Z"},{"databaseId":222222,"headSha":"sha-pr-123","createdAt":"2026-03-06T00:05:00Z"},{"databaseId":333333,"headSha":"sha-pr-123","createdAt":"2026-03-05T23:00:00Z"}]'
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 1
"#,
        );
        let run_id = wait_for_pr_run_start(123, 1).await.unwrap();
        assert_eq!(run_id, Some(222222));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_build_failure_payload_contains_required_fields() {
        let run = GhRunView {
            database_id: 42,
            name: "ci".to_string(),
            status: "completed".to_string(),
            conclusion: Some("failure".to_string()),
            head_branch: "feature/x".to_string(),
            head_sha: "abcdef1234567890".to_string(),
            url: "https://github.com/o/r/actions/runs/42".to_string(),
            jobs: Vec::new(),
            attempt: Some(2),
            pull_requests: Vec::new(),
        };
        let status_seed = GhMonitorStatus {
            team: "atm-dev".to_string(),
            configured: true,
            enabled: true,
            config_source: None,
            config_path: None,
            target_kind: GhMonitorTargetKind::Pr,
            target: "123".to_string(),
            state: "monitoring".to_string(),
            run_id: Some(42),
            reference: None,
            updated_at: "2026-03-06T00:00:00Z".to_string(),
            message: None,
        };
        let request = GhMonitorRequest {
            team: "atm-dev".to_string(),
            target_kind: GhMonitorTargetKind::Pr,
            target: "123".to_string(),
            reference: None,
            start_timeout_secs: Some(120),
            config_cwd: None,
        };
        let payload = build_failure_payload(&run, &status_seed, &request, "corr-1").await;
        for required in [
            "run_url:",
            "failed_job_urls:",
            "pr_url:",
            "workflow:",
            "job_names:",
            "run_id:",
            "run_attempt:",
            "branch:",
            "commit_short:",
            "commit_full:",
            "classification:",
            "first_failing_step:",
            "log_excerpt:",
            "correlation_id:",
            "next_action_hint:",
        ] {
            assert!(
                payload.contains(required),
                "failure payload missing field marker: {required}"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_classify_failure_infra_when_runner_failure_detected() {
        let run = GhRunView {
            database_id: 88,
            name: "ci".to_string(),
            status: "completed".to_string(),
            conclusion: Some("failure".to_string()),
            head_branch: "main".to_string(),
            head_sha: "abcdef1234567890".to_string(),
            url: "https://github.com/o/r/actions/runs/88".to_string(),
            jobs: vec![GhRunJob {
                database_id: 101,
                name: "Runner provisioning failed".to_string(),
                status: "completed".to_string(),
                conclusion: Some("failure".to_string()),
                started_at: None,
                completed_at: None,
                steps: vec![GhRunStep {
                    name: "Set up runner".to_string(),
                    status: Some("completed".to_string()),
                    conclusion: Some("failure".to_string()),
                }],
                url: None,
            }],
            attempt: Some(1),
            pull_requests: Vec::new(),
        };

        assert_eq!(classify_failure(&run), "infra");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_gh_monitor_run_target_success_status_roundtrip() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_gh_monitor_config(temp.path(), "atm-dev");
        let req_json = r#"{"version":1,"request_id":"r-gh-run","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"run","target":"456789"}}"#;
        let monitor_resp = handle_gh_monitor_command(req_json, temp.path()).await;
        assert_eq!(monitor_resp.status, "ok");
        let payload = monitor_resp.payload.unwrap();
        assert_eq!(payload["target_kind"].as_str(), Some("run"));
        assert_eq!(payload["target"].as_str(), Some("456789"));
        assert_eq!(payload["run_id"].as_u64(), Some(456789));
        assert_eq!(payload["state"].as_str(), Some("monitoring"));

        let status_req = r#"{"version":1,"request_id":"r-gh-run-status","command":"gh-status","payload":{"team":"atm-dev","target_kind":"run","target":"456789"}}"#;
        let status_resp = handle_gh_status_command(status_req, temp.path()).await;
        assert_eq!(status_resp.status, "ok");
        let status = status_resp.payload.unwrap();
        assert_eq!(status["target_kind"].as_str(), Some("run"));
        assert_eq!(status["run_id"].as_u64(), Some(456789));
        assert_eq!(status["state"].as_str(), Some("monitoring"));
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn test_gh_monitor_workflow_success_status_roundtrip() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_gh_monitor_config(temp.path(), "atm-dev");
        let _path_guard = install_fake_gh_script(
            &temp,
            r#"#!/bin/sh
if [ "$1" = "run" ] && [ "$2" = "list" ]; then
  echo '[{"databaseId":987654,"headBranch":"develop","headSha":"abcd1234"}]'
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 1
"#,
        );
        let req_json = r#"{"version":1,"request_id":"r-gh-workflow","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"workflow","target":"ci","reference":"develop","start_timeout_secs":30}}"#;
        let monitor_resp = handle_gh_monitor_command(req_json, temp.path()).await;
        assert_eq!(monitor_resp.status, "ok");
        let payload = monitor_resp.payload.unwrap();
        assert_eq!(payload["target_kind"].as_str(), Some("workflow"));
        assert_eq!(payload["target"].as_str(), Some("ci"));
        assert_eq!(payload["reference"].as_str(), Some("develop"));
        assert_eq!(payload["run_id"].as_u64(), Some(987654));
        assert_eq!(payload["state"].as_str(), Some("monitoring"));

        let status_req = r#"{"version":1,"request_id":"r-gh-workflow-status","command":"gh-status","payload":{"team":"atm-dev","target_kind":"workflow","target":"ci"}}"#;
        let status_resp = handle_gh_status_command(status_req, temp.path()).await;
        assert_eq!(status_resp.status, "ok");
        let status = status_resp.payload.unwrap();
        assert_eq!(status["target_kind"].as_str(), Some("workflow"));
        assert_eq!(status["target"].as_str(), Some("ci"));
        assert_eq!(status["reference"].as_str(), Some("develop"));
        assert_eq!(status["run_id"].as_u64(), Some(987654));
        assert_eq!(status["state"].as_str(), Some("monitoring"));
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn test_gh_monitor_uses_repo_config_source_from_payload_cwd() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        let repo_dir = temp.path().join("repo");
        write_repo_gh_monitor_config(&repo_dir, "atm-dev");

        let req_json = format!(
            r#"{{"version":1,"request_id":"r-gh-repo-src","command":"gh-monitor","payload":{{"team":"atm-dev","target_kind":"run","target":"42","config_cwd":"{}"}}}}"#,
            repo_dir.to_string_lossy()
        );
        let resp = handle_gh_monitor_command(&req_json, temp.path()).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["configured"].as_bool(), Some(true));
        assert_eq!(payload["enabled"].as_bool(), Some(true));
        assert_eq!(payload["config_source"].as_str(), Some("repo"));
        assert_eq!(
            payload["config_path"].as_str(),
            Some(repo_dir.join(".atm.toml").to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn test_gh_status_uses_global_config_source_when_repo_missing() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_gh_monitor_config(temp.path(), "atm-dev");

        let status_seed = GhMonitorStatus {
            team: "atm-dev".to_string(),
            configured: true,
            enabled: true,
            config_source: None,
            config_path: None,
            target_kind: GhMonitorTargetKind::Run,
            target: "9001".to_string(),
            state: "monitoring".to_string(),
            run_id: Some(9001),
            reference: None,
            updated_at: "2026-03-06T00:00:00Z".to_string(),
            message: None,
        };
        upsert_gh_monitor_status(temp.path(), status_seed).unwrap();

        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let req_json = format!(
            r#"{{"version":1,"request_id":"r-gh-global-src","command":"gh-status","payload":{{"team":"atm-dev","target_kind":"run","target":"9001","config_cwd":"{}"}}}}"#,
            outside.to_string_lossy()
        );
        let resp = handle_gh_status_command(&req_json, temp.path()).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["configured"].as_bool(), Some(true));
        assert_eq!(payload["enabled"].as_bool(), Some(true));
        assert_eq!(payload["config_source"].as_str(), Some("global"));
        assert_eq!(
            payload["config_path"].as_str(),
            Some(
                temp.path()
                    .join(".config/atm/config.toml")
                    .to_string_lossy()
                    .as_ref()
            )
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn test_gh_monitor_health_reports_global_config_source() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_gh_monitor_config(temp.path(), "atm-dev");

        let outside = temp.path().join("outside-health");
        std::fs::create_dir_all(&outside).unwrap();
        let req_json = format!(
            r#"{{"version":1,"request_id":"r-gh-health-src","command":"gh-monitor-health","payload":{{"team":"atm-dev","config_cwd":"{}"}}}}"#,
            outside.to_string_lossy()
        );
        let resp = handle_gh_monitor_health_command(&req_json, temp.path()).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["configured"].as_bool(), Some(true));
        assert_eq!(payload["enabled"].as_bool(), Some(true));
        assert_eq!(payload["config_source"].as_str(), Some("global"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_gh_status_workflow_reference_disambiguates_parallel_runs() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_gh_monitor_config(temp.path(), "atm-dev");
        let status_a = GhMonitorStatus {
            team: "atm-dev".to_string(),
            configured: true,
            enabled: true,
            config_source: None,
            config_path: None,
            target_kind: GhMonitorTargetKind::Workflow,
            target: "ci".to_string(),
            state: "monitoring".to_string(),
            run_id: Some(111),
            reference: Some("develop".to_string()),
            updated_at: "2026-03-06T00:00:10Z".to_string(),
            message: None,
        };
        let status_b = GhMonitorStatus {
            team: "atm-dev".to_string(),
            configured: true,
            enabled: true,
            config_source: None,
            config_path: None,
            target_kind: GhMonitorTargetKind::Workflow,
            target: "ci".to_string(),
            state: "monitoring".to_string(),
            run_id: Some(222),
            reference: Some("release/v1".to_string()),
            updated_at: "2026-03-06T00:00:11Z".to_string(),
            message: None,
        };
        upsert_gh_monitor_status(temp.path(), status_a).unwrap();
        upsert_gh_monitor_status(temp.path(), status_b).unwrap();

        let status_req = r#"{"version":1,"request_id":"r-gh-workflow-ref","command":"gh-status","payload":{"team":"atm-dev","target_kind":"workflow","target":"ci","reference":"release/v1"}}"#;
        let status_resp = handle_gh_status_command(status_req, temp.path()).await;
        assert_eq!(status_resp.status, "ok");
        let status = status_resp.payload.unwrap();
        assert_eq!(status["reference"].as_str(), Some("release/v1"));
        assert_eq!(status["run_id"].as_u64(), Some(222));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_gh_monitor_control_start_stop_restart_and_health() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_gh_monitor_config(temp.path(), "atm-dev");

        let start_req = r#"{"version":1,"request_id":"r-gh-start","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"start"}}"#;
        let start_resp = handle_gh_monitor_control_command(start_req, temp.path()).await;
        assert_eq!(start_resp.status, "ok");
        let start = start_resp.payload.unwrap();
        assert_eq!(start["lifecycle_state"].as_str(), Some("running"));

        let stop_req = r#"{"version":1,"request_id":"r-gh-stop","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"stop","drain_timeout_secs":1}}"#;
        let stop_resp = handle_gh_monitor_control_command(stop_req, temp.path()).await;
        assert_eq!(stop_resp.status, "ok");
        let stop = stop_resp.payload.unwrap();
        assert_eq!(stop["lifecycle_state"].as_str(), Some("stopped"));

        let restart_req = r#"{"version":1,"request_id":"r-gh-restart","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"restart","drain_timeout_secs":1}}"#;
        let restart_resp = handle_gh_monitor_control_command(restart_req, temp.path()).await;
        assert_eq!(restart_resp.status, "ok");
        let restart = restart_resp.payload.unwrap();
        assert_eq!(restart["lifecycle_state"].as_str(), Some("running"));

        let health_req = r#"{"version":1,"request_id":"r-gh-health","command":"gh-monitor-health","payload":{"team":"atm-dev"}}"#;
        let health_resp = handle_gh_monitor_health_command(health_req, temp.path()).await;
        assert_eq!(health_resp.status, "ok");
        let health = health_resp.payload.unwrap();
        assert_eq!(health["team"].as_str(), Some("atm-dev"));
        assert_eq!(health["lifecycle_state"].as_str(), Some("running"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_gh_monitor_restart_reloads_updated_config_without_daemon_restart() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_gh_monitor_config(temp.path(), "atm-dev");

        let start_req = r#"{"version":1,"request_id":"r-gh-start-reload","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"start"}}"#;
        let start_resp = handle_gh_monitor_control_command(start_req, temp.path()).await;
        assert_eq!(start_resp.status, "ok");

        // Edit config to invalid state, then ensure restart surfaces deterministic
        // config error without requiring a daemon process restart.
        write_invalid_gh_monitor_config(temp.path(), "atm-dev");
        let restart_req = r#"{"version":1,"request_id":"r-gh-restart-invalid","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"restart","drain_timeout_secs":1}}"#;
        let restart_resp = handle_gh_monitor_control_command(restart_req, temp.path()).await;
        assert_eq!(restart_resp.status, "error");
        let err = restart_resp
            .error
            .expect("restart should return config error");
        assert_eq!(err.code, "CONFIG_ERROR");
        assert!(
            err.message.contains("gh_monitor unavailable after reload"),
            "unexpected restart error: {}",
            err.message
        );

        let health_req = r#"{"version":1,"request_id":"r-gh-health-invalid","command":"gh-monitor-health","payload":{"team":"atm-dev"}}"#;
        let health_resp = handle_gh_monitor_health_command(health_req, temp.path()).await;
        assert_eq!(health_resp.status, "ok");
        let health = health_resp.payload.unwrap();
        assert_eq!(health["lifecycle_state"].as_str(), Some("stopped"));
        assert_eq!(
            health["availability_state"].as_str(),
            Some("disabled_config_error")
        );
        assert!(
            !health["message"].as_str().unwrap_or_default().is_empty(),
            "expected actionable config error message in health payload"
        );

        // Repair config and restart again; health should recover to running/healthy.
        write_gh_monitor_config(temp.path(), "atm-dev");
        let restart_recover_req = r#"{"version":1,"request_id":"r-gh-restart-recover","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"restart","drain_timeout_secs":1}}"#;
        let restart_recover_resp =
            handle_gh_monitor_control_command(restart_recover_req, temp.path()).await;
        assert_eq!(restart_recover_resp.status, "ok");
        let restart_recover = restart_recover_resp.payload.unwrap();
        assert_eq!(restart_recover["lifecycle_state"].as_str(), Some("running"));
        assert_eq!(
            restart_recover["availability_state"].as_str(),
            Some("healthy")
        );

        let health_recover_req = r#"{"version":1,"request_id":"r-gh-health-recover","command":"gh-monitor-health","payload":{"team":"atm-dev"}}"#;
        let health_recover_resp =
            handle_gh_monitor_health_command(health_recover_req, temp.path()).await;
        assert_eq!(health_recover_resp.status, "ok");
        let health_recover = health_recover_resp.payload.unwrap();
        assert_eq!(health_recover["lifecycle_state"].as_str(), Some("running"));
        assert_eq!(
            health_recover["availability_state"].as_str(),
            Some("healthy")
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_gh_monitor_command_rejected_when_lifecycle_stopped() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        write_gh_monitor_config(temp.path(), "atm-dev");
        let _ = set_gh_monitor_health_state(
            temp.path(),
            "atm-dev",
            Some("stopped"),
            Some("healthy"),
            Some(0),
            Some("manually stopped for test".to_string()),
            None,
        );

        let req_json = r#"{"version":1,"request_id":"r-gh-stopped","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"run","target":"42"}}"#;
        let resp = handle_gh_monitor_command(req_json, temp.path()).await;
        assert_eq!(resp.status, "error");
        let err = resp.error.unwrap();
        assert_eq!(err.code, "MONITOR_STOPPED");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_gh_monitor_invalid_config_transitions_to_disabled_config_error() {
        let temp = TempDir::new().unwrap();
        write_invalid_gh_monitor_config(temp.path(), "atm-dev");

        let req_json = r#"{"version":1,"request_id":"r-gh-config","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"run","target":"42"}}"#;
        let resp = handle_gh_monitor_command(req_json, temp.path()).await;
        assert_eq!(resp.status, "error");
        let err = resp.error.unwrap();
        assert_eq!(err.code, "CONFIG_ERROR");

        let health_req = r#"{"version":1,"request_id":"r-gh-health","command":"gh-monitor-health","payload":{"team":"atm-dev"}}"#;
        let health_resp = handle_gh_monitor_health_command(health_req, temp.path()).await;
        assert_eq!(health_resp.status, "ok");
        let health = health_resp.payload.unwrap();
        assert_eq!(
            health["availability_state"].as_str(),
            Some("disabled_config_error")
        );
    }

    #[tokio::test]
    #[cfg(not(unix))]
    async fn test_gh_monitor_non_unix_returns_unsupported_platform() {
        let req_json = r#"{"version":1,"request_id":"r-gh-stub","command":"gh-monitor","payload":{"team":"atm-dev","target_kind":"run","target":"1"}}"#;
        let monitor_resp = handle_gh_monitor_command(req_json, std::path::Path::new(".")).await;
        assert_eq!(monitor_resp.status, "error");
        let error = monitor_resp.error.unwrap();
        assert_eq!(error.code, "UNSUPPORTED_PLATFORM");
    }

    #[tokio::test]
    #[cfg(not(unix))]
    async fn test_gh_status_non_unix_returns_unsupported_platform() {
        let req_json = r#"{"version":1,"request_id":"r-gh-status-stub","command":"gh-status","payload":{"team":"atm-dev","target_kind":"run","target":"1"}}"#;
        let status_resp = handle_gh_status_command(req_json, std::path::Path::new(".")).await;
        assert_eq!(status_resp.status, "error");
        let error = status_resp.error.unwrap();
        assert_eq!(error.code, "UNSUPPORTED_PLATFORM");
    }

    #[tokio::test]
    #[cfg(not(unix))]
    async fn test_gh_monitor_control_non_unix_returns_unsupported_platform() {
        let req_json = r#"{"version":1,"request_id":"r-gh-control-stub","command":"gh-monitor-control","payload":{"team":"atm-dev","action":"stop"}}"#;
        let resp = handle_gh_monitor_control_command(req_json, std::path::Path::new(".")).await;
        assert_eq!(resp.status, "error");
        let error = resp.error.unwrap();
        assert_eq!(error.code, "UNSUPPORTED_PLATFORM");
    }

    #[tokio::test]
    #[cfg(not(unix))]
    async fn test_gh_monitor_health_non_unix_returns_unsupported_platform() {
        let req_json = r#"{"version":1,"request_id":"r-gh-health-stub","command":"gh-monitor-health","payload":{"team":"atm-dev"}}"#;
        let resp = handle_gh_monitor_health_command(req_json, std::path::Path::new(".")).await;
        assert_eq!(resp.status, "error");
        let error = resp.error.unwrap();
        assert_eq!(error.code, "UNSUPPORTED_PLATFORM");
    }

    #[test]
    fn test_parse_and_dispatch_unknown_command() {
        let store = make_store();
        let ps = make_ps();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r1","command":"bogus","payload":{}}"#;
        let resp =
            parse_and_dispatch(req_json, &store, &ps, &sr, &new_stream_state_store()).unwrap();
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "UNKNOWN_COMMAND");
    }

    #[test]
    fn test_parse_and_dispatch_register_hint_missing_team() {
        let store = make_store();
        let ps = make_ps();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r1","command":"register-hint","payload":{"agent":"arch-ctm","session_id":"s1","process_id":1234}}"#;
        let resp =
            parse_and_dispatch(req_json, &store, &ps, &sr, &new_stream_state_store()).unwrap();
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "MISSING_PARAMETER");
    }

    #[test]
    fn test_parse_and_dispatch_register_hint_rejects_whitespace_session_id() {
        let store = make_store();
        let ps = make_ps();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r1","command":"register-hint","payload":{"team":"atm-dev","agent":"arch-ctm","session_id":"   ","process_id":1234}}"#;
        let resp =
            parse_and_dispatch(req_json, &store, &ps, &sr, &new_stream_state_store()).unwrap();
        assert_eq!(resp.status, "error");
        let err = resp.error.unwrap();
        assert_eq!(err.code, "MISSING_PARAMETER");
        assert!(
            err.message.contains("session_id"),
            "error must mention missing session_id, got: {}",
            err.message
        );
    }

    #[test]
    #[serial]
    fn test_handle_register_hint_registers_external_member_session() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        let team_dir = temp.path().join(".claude/teams/atm-dev");
        std::fs::create_dir_all(&team_dir).unwrap();
        let config = serde_json::json!({
            "name": "atm-dev",
            "description": "test",
            "createdAt": 1739284800000u64,
            "leadAgentId": "team-lead@atm-dev",
            "leadSessionId": "lead-sess",
            "members": [{
                "agentId": "arch-ctm@atm-dev",
                "name": "arch-ctm",
                "agentType": "codex",
                "model": "gpt5.3-codex",
                "joinedAt": 1739284800000u64,
                "cwd": temp.path().to_string_lossy().to_string(),
                "subscriptions": [],
                "externalBackendType": "external"
            }]
        });
        {
            use std::io::Write;
            let content = serde_json::to_string_pretty(&config).unwrap();
            let path = team_dir.join("config.json");
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = std::io::BufWriter::new(&file);
            writer.write_all(content.as_bytes()).unwrap();
            writer.flush().unwrap();
            file.sync_all().unwrap();
        }

        let store = make_store();
        let sr = make_sr();
        let req = make_request(
            "register-hint",
            serde_json::json!({
                "team": "atm-dev",
                "agent": "arch-ctm",
                "session_id": "local:arch-ctm:sess:1234",
                "process_id": std::process::id(),
                "runtime": "codex",
                "runtime_session_id": "local:arch-ctm:sess:1234"
            }),
        );
        let resp = handle_register_hint(&req, &store, &sr);
        assert_eq!(resp.status, "ok");

        let session = sr
            .lock()
            .unwrap()
            .query_for_team("atm-dev", "arch-ctm")
            .cloned()
            .expect("session should be registered");
        assert_eq!(session.session_id, "local:arch-ctm:sess:1234");
        assert_eq!(session.process_id, std::process::id());
        assert_eq!(session.runtime.as_deref(), Some("codex"));

        let tracker_state = store.lock().unwrap().get_state("arch-ctm");
        assert_eq!(tracker_state, Some(AgentState::Active));
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_handle_register_hint_rejects_codex_backend_pid_mismatch_with_warn_log() {
        let fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        set_member_backend(fixture._temp.path(), "atm-dev", "arch-ctm", "codex");
        let store = make_store();
        let sr = make_sr();

        let capture = SharedLogCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_target(false)
            .with_writer(capture.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let req = make_request(
            "register-hint",
            serde_json::json!({
                "team": "atm-dev",
                "agent": "arch-ctm",
                "session_id": "codex:sess-mismatch",
                "process_id": std::process::id(),
                "runtime": "codex",
            }),
        );
        let resp = handle_register_hint(&req, &store, &sr);
        assert_eq!(resp.status, "error");
        let err = resp.error.expect("error payload");
        assert_eq!(err.code, "PID_PROCESS_MISMATCH");
        assert!(err.message.contains("backend='codex'"));

        let logs = capture.contents();
        assert!(logs.contains("pid/backend mismatch at register_hint"));
        assert!(logs.contains("backend='codex'"));
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_handle_register_hint_rejects_claude_backend_pid_mismatch_with_warn_log() {
        let fixture =
            setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "team-lead-2"]);
        set_member_backend(
            fixture._temp.path(),
            "atm-dev",
            "team-lead-2",
            "claude-code",
        );
        let store = make_store();
        let sr = make_sr();

        let capture = SharedLogCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_target(false)
            .with_writer(capture.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let req = make_request(
            "register-hint",
            serde_json::json!({
                "team": "atm-dev",
                "agent": "team-lead-2",
                "session_id": "claude:sess-mismatch",
                "process_id": std::process::id(),
                "runtime": "claude",
            }),
        );
        let resp = handle_register_hint(&req, &store, &sr);
        assert_eq!(resp.status, "error");
        let err = resp.error.expect("error payload");
        assert_eq!(err.code, "PID_PROCESS_MISMATCH");
        assert!(err.message.contains("backend='claude-code'"));

        let logs = capture.contents();
        assert!(logs.contains("pid/backend mismatch at register_hint"));
        assert!(logs.contains("backend='claude-code'"));
    }

    #[test]
    #[serial]
    fn test_handle_register_hint_recovers_mismatch_offline_baseline_to_active() {
        let fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        set_member_backend(fixture._temp.path(), "atm-dev", "arch-ctm", "external");
        let store = make_store();
        let sr = make_sr();
        // Must be >1 to satisfy register-hint payload validation.
        // Use a non-live high PID to keep this test focused on mismatch-baseline
        // recovery behavior rather than backend process identity matching.
        let hint_pid: u32 = u32::MAX - 7;

        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state_with_context(
                "arch-ctm",
                AgentState::Offline,
                "pid/backend mismatch: backend='codex' expected='comm=codex' actual='zsh' pid=9999",
                "pid_backend_validation",
            );
        }

        let req = make_request(
            "register-hint",
            serde_json::json!({
                "team": "atm-dev",
                "agent": "arch-ctm",
                "session_id": "local:arch-ctm:recover:1",
                "process_id": hint_pid,
                "runtime": "codex",
                "runtime_session_id": "local:arch-ctm:recover:1"
            }),
        );
        let resp = handle_register_hint(&req, &store, &sr);
        assert_eq!(resp.status, "ok");

        let tracker_state = store.lock().unwrap().get_state("arch-ctm");
        assert_eq!(
            tracker_state,
            Some(AgentState::Active),
            "register-hint should transition mismatch-offline baseline to active"
        );
    }

    #[test]
    #[serial]
    fn test_handle_register_hint_rejects_cross_identity_session_write() {
        let temp = TempDir::new().unwrap();
        let _atm_home_guard = EnvGuard::set("ATM_HOME", temp.path().to_str().unwrap());
        let team_dir = temp.path().join(".claude/teams/atm-dev");
        std::fs::create_dir_all(&team_dir).unwrap();
        let config = serde_json::json!({
            "name": "atm-dev",
            "description": "test",
            "createdAt": 1739284800000u64,
            "leadAgentId": "team-lead@atm-dev",
            "leadSessionId": "lead-sess",
            "members": [{
                "agentId": "arch-ctm@atm-dev",
                "name": "arch-ctm",
                "agentType": "codex",
                "model": "gpt5.3-codex",
                "joinedAt": 1739284800000u64,
                "cwd": temp.path().to_string_lossy().to_string(),
                "subscriptions": [],
                "externalBackendType": "external"
            }]
        });
        {
            use std::io::Write;
            let content = serde_json::to_string_pretty(&config).unwrap();
            let path = team_dir.join("config.json");
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = std::io::BufWriter::new(&file);
            writer.write_all(content.as_bytes()).unwrap();
            writer.flush().unwrap();
            file.sync_all().unwrap();
        }

        let store = make_store();
        let sr = make_sr();
        let req = make_request(
            "register-hint",
            serde_json::json!({
                "team": "atm-dev",
                "agent": "arch-ctm",
                "identity": "team-lead",
                "session_id": "local:arch-ctm:sess:9999",
                "process_id": std::process::id(),
            }),
        );
        let resp = handle_register_hint(&req, &store, &sr);
        assert_eq!(resp.status, "error");
        let err = resp.error.expect("error payload required");
        assert_eq!(err.code, "PERMISSION_DENIED");
        assert!(err.message.contains("not allowed to update sessionId"));

        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "arch-ctm")
                .is_none(),
            "cross-identity register-hint must not write session record"
        );
    }

    #[test]
    fn test_parse_and_dispatch_malformed_json() {
        let store = make_store();
        let ps = make_ps();
        let sr = make_sr();
        let resp =
            parse_and_dispatch("not-json{{", &store, &ps, &sr, &new_stream_state_store()).unwrap();
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "INVALID_REQUEST");
    }

    #[test]
    fn test_parse_and_dispatch_version_mismatch() {
        let store = make_store();
        let ps = make_ps();
        let sr = make_sr();
        let req_json = r#"{"version":99,"request_id":"r1","command":"agent-state","payload":{}}"#;
        let resp =
            parse_and_dispatch(req_json, &store, &ps, &sr, &new_stream_state_store()).unwrap();
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "VERSION_MISMATCH");
    }

    #[test]
    fn test_agent_state_missing_agent_field() {
        let store = make_store();
        let sr = make_sr();
        let req = make_request("agent-state", serde_json::json!({}));
        let resp = handle_agent_state(&req, &store, &sr);
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
        let log_path = std::env::temp_dir().join("arch-ctm.log");
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_pane_info("arch-ctm", "%42", &log_path);
        }

        let req = make_request("agent-pane", serde_json::json!({"agent": "arch-ctm"}));
        let resp = handle_agent_pane(&req, &store);
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["pane_id"].as_str().unwrap(), "%42");
        assert_eq!(
            payload["log_path"].as_str().unwrap(),
            log_path.to_str().unwrap()
        );
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
    async fn test_control_stdin_enqueues_payload() {
        use crate::plugins::worker_adapter::AgentState;
        use uuid::Uuid;

        let tmp = tempfile::TempDir::new().unwrap();

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
            elicitation_id: None,
            decision: None,
        };

        let dd = make_dd_in(&tmp);
        let ack = process_control_request(req, tmp.path(), &state_store, &sr, &dd).await;
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
    async fn test_control_elicitation_response_enqueues_payload() {
        use crate::plugins::worker_adapter::AgentState;
        use uuid::Uuid;

        let tmp = tempfile::TempDir::new().unwrap();
        let state_store = make_store();
        {
            let mut tracker = state_store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Active);
        }
        let sr = make_sr();
        {
            sr.lock()
                .unwrap()
                .upsert("arch-ctm", "sess-elicit", std::process::id());
        }

        let req = ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: Uuid::new_v4().to_string(),
            msg_type: "control.elicitation.response".to_string(),
            signal: None,
            sent_at: chrono::Utc::now().to_rfc3339(),
            team: "atm-dev".to_string(),
            session_id: "sess-elicit".to_string(),
            agent_id: "arch-ctm".to_string(),
            sender: "tui".to_string(),
            action: ControlAction::ElicitationResponse,
            payload: Some("allow with guard".to_string()),
            content_ref: None,
            elicitation_id: Some("req-77".to_string()),
            decision: Some("approve".to_string()),
        };
        let dd = make_dd_in(&tmp);
        let ack = process_control_request(req, tmp.path(), &state_store, &sr, &dd).await;
        assert_eq!(ack.result, agent_team_mail_core::control::ControlResult::Ok);
        let qdir = tmp
            .path()
            .join(".config/atm/agent-sessions/atm-dev/arch-ctm/elicitation_queue");
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
    async fn test_control_duplicate_does_not_reenqueue() {
        use crate::plugins::worker_adapter::AgentState;
        use uuid::Uuid;

        let tmp = tempfile::TempDir::new().unwrap();

        let state_store = make_store();
        {
            let mut tracker = state_store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Active);
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
            elicitation_id: None,
            decision: None,
        };

        let dd = make_dd_in(&tmp);
        let ack1 = process_control_request(mk_req(), tmp.path(), &state_store, &sr, &dd).await;
        let ack2 = process_control_request(mk_req(), tmp.path(), &state_store, &sr, &dd).await;
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
            elicitation_id: None,
            decision: None,
        };
        let (dd, _dd_dir) = make_dd();
        let ack = process_control_request(req, _dd_dir.path(), &state_store, &sr, &dd).await;
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
            elicitation_id: None,
            decision: None,
        };

        let (dd, _dd_dir) = make_dd();
        let ack1 = process_control_request(mk_req(), _dd_dir.path(), &state_store, &sr, &dd).await;
        let ack2 = process_control_request(mk_req(), _dd_dir.path(), &state_store, &sr, &dd).await;
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
            elicitation_id: None,
            decision: None,
        };
        let (dd, _dd_dir) = make_dd();
        let ack = process_control_request(req, _dd_dir.path(), &state_store, &sr, &dd).await;
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
        let daemon_lock = {
            let path = home_dir.join(".config/atm/daemon.lock");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            agent_team_mail_core::io::lock::acquire_lock(&path, 0).unwrap()
        };

        // Set up state store with one agent
        let state_store = make_store();
        {
            let mut tracker = state_store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Idle);
        }

        // Start the socket server
        let launch_tx = new_launch_sender();
        let (dd, _dd_dir) = make_dd();
        let _handle = start_socket_server(
            home_dir.clone(),
            state_store,
            new_pubsub_store(),
            launch_tx,
            make_sr(),
            dd,
            new_stream_state_store(),
            new_stream_event_sender(),
            crate::daemon::new_log_event_queue(),
            &daemon_lock,
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
        let daemon_lock = {
            let path = home_dir.join(".config/atm/daemon.lock");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            agent_team_mail_core::io::lock::acquire_lock(&path, 0).unwrap()
        };

        let state_store = make_store();
        {
            let mut tracker = state_store.lock().unwrap();
            tracker.register_agent("agent-a");
            tracker.register_agent("agent-b");
            tracker.set_state("agent-b", AgentState::Active);
        }

        let launch_tx = new_launch_sender();
        let (dd, _dd_dir) = make_dd();
        let _handle = start_socket_server(
            home_dir.clone(),
            state_store,
            new_pubsub_store(),
            launch_tx,
            make_sr(),
            dd,
            new_stream_state_store(),
            new_stream_event_sender(),
            crate::daemon::new_log_event_queue(),
            &daemon_lock,
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
        let daemon_lock = {
            let path = home_dir.join(".config/atm/daemon.lock");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            agent_team_mail_core::io::lock::acquire_lock(&path, 0).unwrap()
        };

        let launch_tx = new_launch_sender();
        let (dd, _dd_dir) = make_dd();
        let _handle = start_socket_server(
            home_dir.clone(),
            make_store(),
            new_pubsub_store(),
            launch_tx,
            make_sr(),
            dd,
            new_stream_state_store(),
            new_stream_event_sender(),
            crate::daemon::new_log_event_queue(),
            &daemon_lock,
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
        let daemon_lock = {
            let path = home_dir.join(".config/atm/daemon.lock");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            agent_team_mail_core::io::lock::acquire_lock(&path, 0).unwrap()
        };

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
        let (dd, _dd_dir) = make_dd();
        let _handle = start_socket_server(
            home_dir.clone(),
            state_store,
            new_pubsub_store(),
            launch_tx,
            sr,
            dd,
            new_stream_state_store(),
            new_stream_event_sender(),
            crate::daemon::new_log_event_queue(),
            &daemon_lock,
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
        let daemon_lock = {
            let path = home_dir.join(".config/atm/daemon.lock");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            agent_team_mail_core::io::lock::acquire_lock(&path, 0).unwrap()
        };
        let state_store = make_store();

        let launch_tx = new_launch_sender();
        let (dd, _dd_dir) = make_dd();
        let _handle = start_socket_server(
            home_dir.clone(),
            state_store,
            new_pubsub_store(),
            launch_tx,
            make_sr(),
            dd,
            new_stream_state_store(),
            new_stream_event_sender(),
            crate::daemon::new_log_event_queue(),
            &daemon_lock,
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
    #[cfg(unix)]
    fn test_collect_member_transition_events_emits_online_change_once() {
        let events = collect_member_transition_events(
            Some(AgentState::Offline),
            AgentState::Active,
            "session_start",
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "member_state_change");
        assert_eq!(events[0].level, "info");
        assert_eq!(events[0].old, "Offline");
        assert_eq!(events[0].new, "Online");
        assert_eq!(events[0].reason, "session_start");
    }

    #[test]
    #[cfg(unix)]
    fn test_collect_member_transition_events_emits_busy_idle_at_debug_only() {
        let events = collect_member_transition_events(
            Some(AgentState::Active),
            AgentState::Idle,
            "heartbeat",
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "member_activity_change");
        assert_eq!(events[0].level, "debug");
        assert_eq!(events[0].old, "Busy");
        assert_eq!(events[0].new, "Idle");
        assert_eq!(events[0].reason, "heartbeat");
    }

    #[test]
    #[cfg(unix)]
    fn test_collect_member_transition_events_no_duplicate_when_state_unchanged() {
        let events =
            collect_member_transition_events(Some(AgentState::Idle), AgentState::Idle, "noop");
        assert!(events.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn test_session_identity_change_flags_detects_changes() {
        let sr = make_sr();
        {
            sr.lock()
                .unwrap()
                .upsert_for_team("atm-dev", "arch-ctm", "sess-1", 1111);
        }
        let record = sr
            .lock()
            .unwrap()
            .query_for_team("atm-dev", "arch-ctm")
            .cloned();
        let (session_changed, pid_changed) =
            session_identity_change_flags(record.as_ref(), "sess-2", Some(2222));
        assert!(session_changed);
        assert!(pid_changed);
    }

    #[test]
    #[cfg(unix)]
    fn test_session_identity_change_flags_no_change_when_values_match() {
        let sr = make_sr();
        {
            sr.lock()
                .unwrap()
                .upsert_for_team("atm-dev", "arch-ctm", "sess-1", 1111);
        }
        let record = sr
            .lock()
            .unwrap()
            .query_for_team("atm-dev", "arch-ctm")
            .cloned();
        let (session_changed, pid_changed) =
            session_identity_change_flags(record.as_ref(), "sess-1", Some(1111));
        assert!(!session_changed);
        assert!(!pid_changed);
    }

    #[test]
    #[cfg(unix)]
    fn test_hook_action_name_includes_compact_events() {
        assert_eq!(
            hook_action_name("session_start"),
            Some("hook.session_start")
        );
        assert_eq!(
            hook_action_name("permission_request"),
            Some("hook.permission_request")
        );
        assert_eq!(hook_action_name("stop"), Some("hook.stop"));
        assert_eq!(
            hook_action_name("notification_idle_prompt"),
            Some("hook.notification_idle_prompt")
        );
        assert_eq!(hook_action_name("pre_compact"), Some("hook.pre_compact"));
        assert_eq!(
            hook_action_name("compact_complete"),
            Some("hook.compact_complete")
        );
        assert_eq!(hook_action_name("session_end"), Some("hook.session_end"));
        assert_eq!(hook_action_name("teammate_idle"), None);
    }

    #[test]
    fn test_parse_and_dispatch_hook_event_internal_error() {
        let store = make_store();
        let ps = make_ps();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r-hook","command":"hook-event","payload":{"event":"session_start","agent":"test-agent","session_id":"s1"}}"#;
        let resp =
            parse_and_dispatch(req_json, &store, &ps, &sr, &new_stream_state_store()).unwrap();
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "INTERNAL_ERROR");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_start_updates_registry() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r1","command":"hook-event","payload":{"event":"session_start","agent":"team-lead","team":"atm-dev","session_id":"sess-abc","process_id":0}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());
        assert_eq!(payload["event"].as_str().unwrap(), "session_start");
        assert_eq!(payload["agent"].as_str().unwrap(), "team-lead");

        // Check session registry updated
        let reg = sr.lock().unwrap();
        let record = reg.query("team-lead").unwrap();
        assert_eq!(record.session_id, "sess-abc");
        assert_eq!(record.process_id, 0);

        // Check agent registered in state tracker
        let tracker = store.lock().unwrap();
        assert!(tracker.get_state("team-lead").is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_start_idempotent_if_already_tracked() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        // Pre-register agent as Idle
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("team-lead");
            tracker.set_state("team-lead", AgentState::Idle);
        }
        let req_json = r#"{"version":1,"request_id":"r2","command":"hook-event","payload":{"event":"session_start","agent":"team-lead","team":"atm-dev","session_id":"sess-xyz","process_id":0}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        // State should remain Idle (not reset to Launching) — session_start only registers if not already tracked
        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("team-lead"), Some(AgentState::Idle));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_duplicate_request_id_is_deduped_before_state_mutation() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        let (dd, _dd_dir) = make_dd();
        let req_json = r#"{"version":1,"request_id":"r-dedup-1","command":"hook-event","payload":{"event":"session_start","agent":"team-lead","team":"atm-dev","session_id":"sess-dedup","process_id":0}}"#;

        let first = handle_hook_event_command_with_dedup_retry(req_json, &store, &sr, &dd).await;
        assert_eq!(first.status, "ok");
        let payload1 = first.payload.unwrap();
        assert!(payload1["processed"].as_bool().unwrap());
        assert!(payload1.get("duplicate").is_none());

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;

        let second = handle_hook_event_command_with_dedup_retry(req_json, &store, &sr, &dd).await;
        assert_eq!(second.status, "ok");
        let payload2 = second.payload.unwrap();
        assert!(payload2["processed"].as_bool().unwrap());
        assert_eq!(payload2["duplicate"].as_bool(), Some(true));

        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("team-lead"), Some(AgentState::Active));
        let elapsed = tracker
            .time_since_transition("team-lead")
            .expect("team-lead transition timestamp should exist");
        assert!(
            elapsed >= std::time::Duration::from_millis(20),
            "duplicate hook request should not reset last transition timestamp"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_pre_compact_processed() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r-pre","command":"hook-event","payload":{"event":"pre_compact","agent":"team-lead","team":"atm-dev","session_id":"sess-pre","process_id":4321}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());
        assert_eq!(payload["event"].as_str().unwrap(), "pre_compact");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_compact_complete_processed() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r-compact-complete","command":"hook-event","payload":{"event":"compact_complete","agent":"team-lead","team":"atm-dev","session_id":"sess-compact","process_id":4321}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());
        assert_eq!(payload["event"].as_str().unwrap(), "compact_complete");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_teammate_idle_updates_state() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        // Register agent as Busy first
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Active);
        }
        let req_json = r#"{"version":1,"request_id":"r3","command":"hook-event","payload":{"event":"teammate_idle","agent":"arch-ctm","session_id":"sess-1","team":"atm-dev"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());
        assert_eq!(payload["event"].as_str().unwrap(), "teammate_idle");

        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Idle));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_teammate_idle_rejects_unknown_agent() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        // Agent exists in payload but is not a member of the team config.
        let req_json = r#"{"version":1,"request_id":"r4","command":"hook-event","payload":{"event":"teammate_idle","agent":"new-agent","session_id":"sess-2","team":"atm-dev"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(!payload["processed"].as_bool().unwrap());
        assert_eq!(payload["reason"].as_str().unwrap(), "agent not in team");

        let tracker = store.lock().unwrap();
        assert!(tracker.get_state("new-agent").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_permission_request_rejects_unknown_agent() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r-pr-unknown","command":"hook-event","payload":{"event":"permission_request","agent":"new-agent","session_id":"sess-pr-unknown","team":"atm-dev","tool_name":"Bash"}}"#;
        let resp = handle_hook_event_command(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(!payload["processed"].as_bool().unwrap());
        assert_eq!(payload["reason"].as_str().unwrap(), "agent not in team");
        assert!(store.lock().unwrap().get_state("new-agent").is_none());
        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "new-agent")
                .is_none()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_stop_rejects_unknown_agent() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r-stop-unknown","command":"hook-event","payload":{"event":"stop","agent":"new-agent","session_id":"sess-stop-unknown","team":"atm-dev"}}"#;
        let resp = handle_hook_event_command(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(!payload["processed"].as_bool().unwrap());
        assert_eq!(payload["reason"].as_str().unwrap(), "agent not in team");
        assert!(store.lock().unwrap().get_state("new-agent").is_none());
        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "new-agent")
                .is_none()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_notification_idle_prompt_rejects_unknown_agent() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r-notify-unknown","command":"hook-event","payload":{"event":"notification_idle_prompt","agent":"new-agent","session_id":"sess-notify-unknown","team":"atm-dev"}}"#;
        let resp = handle_hook_event_command(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(!payload["processed"].as_bool().unwrap());
        assert_eq!(payload["reason"].as_str().unwrap(), "agent not in team");
        assert!(store.lock().unwrap().get_state("new-agent").is_none());
        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "new-agent")
                .is_none()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_permission_request_marks_blocked_permission_context_without_liveness_drift()
     {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Idle);
        }
        {
            sr.lock()
                .unwrap()
                .upsert_for_team("atm-dev", "arch-ctm", "sess-pr", 1111);
        }

        let req_json = r#"{"version":1,"request_id":"r-pr","command":"hook-event","payload":{"event":"permission_request","agent":"arch-ctm","session_id":"sess-pr","team":"atm-dev","tool_name":"Bash"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());
        assert_eq!(payload["event"].as_str().unwrap(), "permission_request");

        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Active));
        let meta = tracker
            .transition_meta("arch-ctm")
            .expect("transition metadata should exist");
        assert!(meta.reason.contains("blocked-permission"));

        let reg = sr.lock().unwrap();
        let record = reg.query_for_team("atm-dev", "arch-ctm").unwrap();
        assert_eq!(
            record.state,
            crate::daemon::session_registry::SessionState::Active
        );
        assert_eq!(record.session_id, "sess-pr");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_stop_transitions_to_idle_without_liveness_drift() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Active);
        }
        {
            sr.lock()
                .unwrap()
                .upsert_for_team("atm-dev", "arch-ctm", "sess-stop", 2222);
        }

        let req_json = r#"{"version":1,"request_id":"r-stop","command":"hook-event","payload":{"event":"stop","agent":"arch-ctm","session_id":"sess-stop","team":"atm-dev"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());
        assert_eq!(payload["event"].as_str().unwrap(), "stop");

        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Idle));

        let reg = sr.lock().unwrap();
        let record = reg.query_for_team("atm-dev", "arch-ctm").unwrap();
        assert_eq!(
            record.state,
            crate::daemon::session_registry::SessionState::Active
        );
        assert_eq!(record.session_id, "sess-stop");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_notification_idle_prompt_transitions_to_idle_without_liveness_drift() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("arch-ctm");
            tracker.set_state("arch-ctm", AgentState::Active);
        }
        {
            sr.lock()
                .unwrap()
                .upsert_for_team("atm-dev", "arch-ctm", "sess-notify", 3333);
        }

        let req_json = r#"{"version":1,"request_id":"r-notify","command":"hook-event","payload":{"event":"notification_idle_prompt","agent":"arch-ctm","session_id":"sess-notify","team":"atm-dev"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());
        assert_eq!(
            payload["event"].as_str().unwrap(),
            "notification_idle_prompt"
        );

        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Idle));

        let reg = sr.lock().unwrap();
        let record = reg.query_for_team("atm-dev", "arch-ctm").unwrap();
        assert_eq!(
            record.state,
            crate::daemon::session_registry::SessionState::Active
        );
        assert_eq!(record.session_id, "sess-notify");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_end_marks_dead() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("team-lead");
            tracker.set_state("team-lead", AgentState::Idle);
        }
        {
            sr.lock()
                .unwrap()
                .upsert_for_team("atm-dev", "team-lead", "sess-end", 1111);
        }
        let req_json = r#"{"version":1,"request_id":"r5","command":"hook-event","payload":{"event":"session_end","agent":"team-lead","session_id":"sess-end","team":"atm-dev"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());
        assert_eq!(payload["event"].as_str().unwrap(), "session_end");

        // Session registry should be marked dead
        let reg = sr.lock().unwrap();
        let record = reg.query("team-lead").unwrap();
        assert_eq!(
            record.state,
            crate::daemon::session_registry::SessionState::Dead
        );

        // State tracker should be Killed
        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("team-lead"), Some(AgentState::Offline));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_end_unknown_session_is_noop() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("team-lead");
            tracker.set_state("team-lead", AgentState::Idle);
        }
        let req_json = r#"{"version":1,"request_id":"r5-unknown","command":"hook-event","payload":{"event":"session_end","agent":"team-lead","session_id":"sess-missing","team":"atm-dev"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());

        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "team-lead")
                .is_none(),
            "unknown session_end must not create tombstone state"
        );
        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("team-lead"), Some(AgentState::Idle));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_end_non_lead_unknown_session_is_debug_noop() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();

        let req_json = r#"{"version":1,"request_id":"r5-unknown-non-lead","command":"hook-event","payload":{"event":"session_end","agent":"arch-ctm","session_id":"sess-missing","team":"atm-dev"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(
            payload["processed"].as_bool().unwrap(),
            "unknown-session no-op should not be rejected by team-lead gate"
        );
        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "arch-ctm")
                .is_none(),
            "unknown non-lead session_end must not create session registry rows"
        );
        let tracker = store.lock().unwrap();
        assert!(
            tracker.get_state("arch-ctm").is_none(),
            "unknown non-lead session_end must not mutate activity tracker"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_end_unknown_team_is_strict_noop() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r5-unknown-team","command":"hook-event","payload":{"event":"session_end","agent":"team-lead","session_id":"sess-unknown","team":"unknown-team"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(!payload["processed"].as_bool().unwrap());
        assert!(
            payload["reason"]
                .as_str()
                .unwrap()
                .contains("team config not found")
        );
        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("unknown-team", "team-lead")
                .is_none()
        );
        let tracker = store.lock().unwrap();
        assert!(tracker.get_state("team-lead").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_end_unknown_agent_is_strict_noop() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r5-unknown-agent","command":"hook-event","payload":{"event":"session_end","agent":"arch-ctm","session_id":"sess-unknown-agent","team":"atm-dev"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(!payload["processed"].as_bool().unwrap());
        assert_eq!(payload["reason"].as_str().unwrap(), "agent not in team");
        assert!(
            sr.lock()
                .unwrap()
                .query_for_team("atm-dev", "arch-ctm")
                .is_none()
        );
        let tracker = store.lock().unwrap();
        assert!(tracker.get_state("arch-ctm").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_end_already_dead_is_noop() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("team-lead");
            tracker.set_state("team-lead", AgentState::Offline);
        }
        {
            let mut reg = sr.lock().unwrap();
            reg.upsert_for_team("atm-dev", "team-lead", "sess-dead", 1111);
            reg.mark_dead_for_team("atm-dev", "team-lead");
        }
        let req_json = r#"{"version":1,"request_id":"r5-dead","command":"hook-event","payload":{"event":"session_end","agent":"team-lead","session_id":"sess-dead","team":"atm-dev"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());

        let reg = sr.lock().unwrap();
        let record = reg.query_for_team("atm-dev", "team-lead").unwrap();
        assert_eq!(
            record.state,
            crate::daemon::session_registry::SessionState::Dead
        );
        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("team-lead"), Some(AgentState::Offline));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_end_mismatched_session_is_noop() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("team-lead");
            tracker.set_state("team-lead", AgentState::Idle);
        }
        {
            sr.lock()
                .unwrap()
                .upsert_for_team("atm-dev", "team-lead", "sess-current", 1111);
        }
        let req_json = r#"{"version":1,"request_id":"r5-mismatch","command":"hook-event","payload":{"event":"session_end","agent":"team-lead","session_id":"sess-other","team":"atm-dev"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());

        let reg = sr.lock().unwrap();
        let record = reg.query_for_team("atm-dev", "team-lead").unwrap();
        assert_eq!(
            record.state,
            crate::daemon::session_registry::SessionState::Active
        );
        assert_eq!(record.session_id, "sess-current");
        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("team-lead"), Some(AgentState::Idle));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_end_without_session_id_is_noop() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        {
            let mut tracker = store.lock().unwrap();
            tracker.register_agent("team-lead");
            tracker.set_state("team-lead", AgentState::Idle);
        }
        {
            sr.lock()
                .unwrap()
                .upsert_for_team("atm-dev", "team-lead", "sess-current", 1111);
        }
        let req_json = r#"{"version":1,"request_id":"r5-no-session","command":"hook-event","payload":{"event":"session_end","agent":"team-lead","team":"atm-dev"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());

        let reg = sr.lock().unwrap();
        let record = reg.query_for_team("atm-dev", "team-lead").unwrap();
        assert_eq!(
            record.state,
            crate::daemon::session_registry::SessionState::Active
        );
        let tracker = store.lock().unwrap();
        assert_eq!(tracker.get_state("team-lead"), Some(AgentState::Idle));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_unknown_type_returns_ok_not_processed() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r6","command":"hook-event","payload":{"event":"some_future_event","agent":"team-lead","team":"atm-dev","session_id":"s1"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(!payload["processed"].as_bool().unwrap());
        assert!(
            payload["reason"]
                .as_str()
                .unwrap()
                .contains("unknown event type")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_missing_agent_returns_not_processed() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r7","command":"hook-event","payload":{"event":"session_start","agent":"","team":"atm-dev","session_id":"sess-1"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
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
        let req_json = r#"{"version":99,"request_id":"r8","command":"hook-event","payload":{"event":"session_start","agent":"team-lead","team":"atm-dev","session_id":"s1"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "VERSION_MISMATCH");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_missing_team_returns_not_processed() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r-missing-team","command":"hook-event","payload":{"event":"session_start","agent":"team-lead","session_id":"s1"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(!payload["processed"].as_bool().unwrap());
        assert_eq!(payload["reason"].as_str().unwrap(), "missing team");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_non_lead_session_start_accepted() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r-non-lead-start","command":"hook-event","payload":{"event":"session_start","agent":"arch-ctm","team":"atm-dev","session_id":"sess-x"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(payload["processed"].as_bool().unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_start_rejects_non_member() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        let req_json = r#"{"version":1,"request_id":"r-non-member-start","command":"hook-event","payload":{"event":"session_start","agent":"rogue-member","team":"atm-dev","session_id":"sess-rogue"}}"#;
        let resp = handle_hook_event_with_transient_retry(req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(!payload["processed"].as_bool().unwrap());
        assert_eq!(payload["reason"].as_str().unwrap(), "agent not in team");

        let reg = sr.lock().unwrap();
        assert!(
            reg.query_for_team("atm-dev", "rogue-member").is_none(),
            "non-member session_start must not register daemon session state"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_start_rejects_backend_pid_mismatch() {
        let fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);

        // Mark arch-ctm as codex backend in team config.
        let team_cfg = fixture
            ._temp
            .path()
            .join(".claude/teams/atm-dev/config.json");
        let mut cfg: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&team_cfg).unwrap()).unwrap();
        let members = cfg["members"].as_array_mut().unwrap();
        let arch = members
            .iter_mut()
            .find(|m| m["name"].as_str() == Some("arch-ctm"))
            .unwrap();
        arch["externalBackendType"] = serde_json::json!("codex");
        {
            use std::io::Write;
            let content = serde_json::to_string_pretty(&cfg).unwrap();
            let file = std::fs::File::create(&team_cfg).unwrap();
            let mut writer = std::io::BufWriter::new(&file);
            writer.write_all(content.as_bytes()).unwrap();
            writer.flush().unwrap();
            file.sync_all().unwrap();
        }

        let store = make_store();
        let sr = make_sr();
        let req_json = format!(
            "{{\"version\":1,\"request_id\":\"r-backend-mismatch\",\"command\":\"hook-event\",\"payload\":{{\"event\":\"session_start\",\"agent\":\"arch-ctm\",\"team\":\"atm-dev\",\"session_id\":\"sess-mismatch\",\"process_id\":{}}}}}",
            std::process::id()
        );
        let resp = handle_hook_event_with_transient_retry(&req_json, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(!payload["processed"].as_bool().unwrap());
        assert!(
            payload["reason"]
                .as_str()
                .unwrap()
                .contains("pid/backend mismatch")
        );

        let reg = sr.lock().unwrap();
        assert!(
            reg.query_for_team("atm-dev", "arch-ctm").is_none(),
            "mismatched pid/backend must not upsert session registry"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_socket_server_hook_event_roundtrip() {
        use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio_util::sync::CancellationToken;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let home_dir = temp_dir.path().to_path_buf();
        let _env = EnvGuard::set("ATM_HOME", home_dir.to_str().unwrap());
        write_hook_auth_team_config(&home_dir, "atm-dev", "team-lead", &["team-lead"]);
        let cancel = CancellationToken::new();
        let daemon_lock = {
            let path = home_dir.join(".config/atm/daemon.lock");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            agent_team_mail_core::io::lock::acquire_lock(&path, 0).unwrap()
        };

        let state_store = make_store();
        let session_registry = make_sr();

        let launch_tx = new_launch_sender();
        let (dd, _dd_dir) = make_dd();
        let _handle = start_socket_server(
            home_dir.clone(),
            state_store.clone(),
            new_pubsub_store(),
            launch_tx,
            session_registry.clone(),
            dd,
            new_stream_state_store(),
            new_stream_event_sender(),
            crate::daemon::new_log_event_queue(),
            &daemon_lock,
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
                // This roundtrip test validates socket lifecycle plumbing, not
                // backend-specific process signature checks.
                "process_id": 0,
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
        assert!(
            resp.is_ok(),
            "session_start hook-event failed: {:?}",
            resp.error
        );
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
        assert!(
            resp2.is_ok(),
            "teammate_idle hook-event failed: {:?}",
            resp2.error
        );

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
    #[serial]
    async fn test_socket_server_hook_event_session_end_roundtrip() {
        use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio_util::sync::CancellationToken;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let home_dir = temp_dir.path().to_path_buf();
        let _env = EnvGuard::set("ATM_HOME", home_dir.to_str().unwrap());
        write_hook_auth_team_config(&home_dir, "atm-dev", "team-lead", &["team-lead"]);
        let cancel = CancellationToken::new();
        let daemon_lock = {
            let path = home_dir.join(".config/atm/daemon.lock");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            agent_team_mail_core::io::lock::acquire_lock(&path, 0).unwrap()
        };

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
            reg.upsert_for_team(
                "atm-dev",
                "team-lead",
                "sess-end-roundtrip",
                std::process::id(),
            );
        }

        let launch_tx = new_launch_sender();
        let (dd, _dd_dir) = make_dd();
        let _handle = start_socket_server(
            home_dir.clone(),
            state_store.clone(),
            new_pubsub_store(),
            launch_tx,
            session_registry.clone(),
            dd,
            new_stream_state_store(),
            new_stream_event_sender(),
            crate::daemon::new_log_event_queue(),
            &daemon_lock,
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
            "offline",
            "Agent state must be 'offline' after session_end"
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
        let daemon_lock = {
            let path = home_dir.join(".config/atm/daemon.lock");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            agent_team_mail_core::io::lock::acquire_lock(&path, 0).unwrap()
        };
        let state_store = make_store();

        let socket_path = home_dir.join(".claude/daemon/atm-daemon.sock");

        {
            let launch_tx = new_launch_sender();
            let (dd, _dd_dir) = make_dd();
            let _handle = start_socket_server(
                home_dir.clone(),
                state_store,
                new_pubsub_store(),
                launch_tx,
                make_sr(),
                dd,
                new_stream_state_store(),
                new_stream_event_sender(),
                crate::daemon::new_log_event_queue(),
                &daemon_lock,
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
        assert!(
            payload["last_alive_at"].as_str().is_some(),
            "live session-query responses should expose last_alive_at"
        );
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
        let reg = sr.lock().unwrap();
        let state = reg
            .query("stale-agent")
            .expect("stale agent should remain tracked for dead-state diagnostics")
            .state
            .clone();
        assert_eq!(
            state,
            crate::daemon::session_registry::SessionState::Dead,
            "dead pid should converge to dead state after query"
        );
    }

    #[test]
    fn test_session_query_includes_runtime_metadata_fields() {
        let sr = make_sr();
        let runtime_home = std::env::temp_dir()
            .join("runtime/gemini/atm-dev/arch-ctm/home")
            .to_string_lossy()
            .into_owned();
        {
            let mut reg = sr.lock().unwrap();
            reg.upsert_runtime_for_team(
                "atm-dev",
                "arch-ctm",
                "sess-gem-1",
                4242,
                Some("gemini".to_string()),
                Some("gemini-session-123".to_string()),
                Some("%42".to_string()),
                Some(runtime_home.clone()),
            );
        }
        let req = make_request(
            "session-query",
            serde_json::json!({"name": "arch-ctm", "team": "atm-dev"}),
        );
        let resp = handle_session_query(&req, &sr);
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["runtime"].as_str(), Some("gemini"));
        assert_eq!(
            payload["runtime_session_id"].as_str(),
            Some("gemini-session-123")
        );
        assert_eq!(
            payload["runtime_home"].as_str(),
            Some(runtime_home.as_str())
        );
    }

    // ── hook-event session_start with empty session_id tests ──────────────────

    /// When session_id is empty in a session_start event, the handler must
    /// return processed=false immediately without mutating session registry or
    /// agent state tracker.
    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_start_empty_session_id_returns_not_processed() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead"]);
        let state_store = make_store();
        let sr = make_sr();

        let request = SocketRequest {
            version: agent_team_mail_core::daemon_client::PROTOCOL_VERSION,
            request_id: "req-empty-sid".to_string(),
            command: "hook-event".to_string(),
            payload: serde_json::json!({
                "event": "session_start",
                "agent": "team-lead",
                "team": "atm-dev",
                "session_id": "",
                "process_id": 12345_u32,
            }),
        };
        let req_str = serde_json::to_string(&request).unwrap();

        let resp = handle_hook_event_with_transient_retry(&req_str, &state_store, &sr).await;

        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(
            !payload["processed"].as_bool().unwrap(),
            "processed must be false when session_id is empty"
        );
        assert_eq!(payload["reason"].as_str().unwrap(), "missing session_id",);

        // Session registry must remain empty — no upsert occurred
        let reg = sr.lock().unwrap();
        assert!(
            reg.query("team-lead").is_none(),
            "session registry must not be mutated when session_id is empty"
        );
        drop(reg);

        // State tracker must remain empty — no register_agent occurred
        let tracker = state_store.lock().unwrap();
        assert!(
            tracker.get_state("team-lead").is_none(),
            "agent state tracker must not be mutated when session_id is empty"
        );
    }

    /// Confirm that the agent is NOT registered in the state tracker when
    /// session_id is absent, even if the agent field is present.
    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_session_start_no_agent_registration_without_session_id() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let state_store = make_store();
        let sr = make_sr();

        let request = SocketRequest {
            version: agent_team_mail_core::daemon_client::PROTOCOL_VERSION,
            request_id: "req-no-reg".to_string(),
            command: "hook-event".to_string(),
            payload: serde_json::json!({
                "event": "session_start",
                "agent": "arch-ctm",
                "team": "atm-dev",
                "session_id": "",
            }),
        };
        let req_str = serde_json::to_string(&request).unwrap();

        let _resp = handle_hook_event_with_transient_retry(&req_str, &state_store, &sr).await;

        // Agent must NOT appear in the tracker after an empty-session_id event
        let tracker = state_store.lock().unwrap();
        assert!(
            tracker.get_state("arch-ctm").is_none(),
            "arch-ctm must not be registered when session_id is empty"
        );
    }

    // ── sent_at skew validation unit tests ────────────────────────────────────

    /// Helper: build a minimal valid ControlRequest with the given sent_at string.
    #[cfg(unix)]
    fn make_control_req_with_sent_at(sent_at: &str) -> ControlRequest {
        ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: "req-skew-test".to_string(),
            msg_type: "control.stdin.request".to_string(),
            signal: None,
            sent_at: sent_at.to_string(),
            team: "atm-dev".to_string(),
            session_id: "sess-skew".to_string(),
            agent_id: "arch-ctm".to_string(),
            sender: "team-lead".to_string(),
            action: ControlAction::Stdin,
            payload: Some("hello".to_string()),
            content_ref: None,
            elicitation_id: None,
            decision: None,
        }
    }

    /// A `sent_at` timestamp 400 seconds in the past exceeds the default 300s window.
    #[test]
    #[cfg(unix)]
    fn test_validate_sent_at_too_old_rejected() {
        let old = chrono::Utc::now() - chrono::Duration::seconds(400);
        let req = make_control_req_with_sent_at(&old.to_rfc3339());
        let err = validate_control_request(&req);
        assert!(err.is_some(), "should be rejected");
        assert!(err.unwrap().contains("skew"), "error should mention skew");
    }

    /// A `sent_at` timestamp within the default 300s window is accepted.
    #[test]
    #[cfg(unix)]
    fn test_validate_sent_at_within_window_accepted() {
        let recent = chrono::Utc::now() - chrono::Duration::seconds(100);
        let req = make_control_req_with_sent_at(&recent.to_rfc3339());
        let err = validate_control_request(&req);
        assert!(err.is_none(), "should be accepted, got: {:?}", err);
    }

    /// A `sent_at` timestamp 400 seconds in the future exceeds the default 300s window.
    #[test]
    #[cfg(unix)]
    fn test_validate_sent_at_future_skew_rejected() {
        let future = chrono::Utc::now() + chrono::Duration::seconds(400);
        let req = make_control_req_with_sent_at(&future.to_rfc3339());
        let err = validate_control_request(&req);
        assert!(err.is_some(), "future skew should be rejected");
        assert!(err.unwrap().contains("skew"), "error should mention skew");
    }

    /// A `sent_at` timestamp at "now" (within a few seconds) is accepted.
    #[test]
    #[cfg(unix)]
    fn test_validate_sent_at_now_accepted() {
        let now = chrono::Utc::now();
        let req = make_control_req_with_sent_at(&now.to_rfc3339());
        let err = validate_control_request(&req);
        assert!(
            err.is_none(),
            "current timestamp should be accepted, got: {:?}",
            err
        );
    }

    /// A malformed `sent_at` value fails RFC3339 parse → rejected.
    #[test]
    #[cfg(unix)]
    fn test_validate_sent_at_malformed_rejected() {
        let req = make_control_req_with_sent_at("not-a-timestamp");
        let err = validate_control_request(&req);
        assert!(err.is_some(), "malformed sent_at should be rejected");
    }

    // ── source-aware lifecycle validation ────────────────────────────────────

    /// `claude_hook` source: `session_start` from non-lead member is accepted.
    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_claude_hook_source_non_lead_session_start_accepted() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();

        let req = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-claude-hook".to_string(),
            command: "hook-event".to_string(),
            payload: serde_json::json!({
                "event": "session_start",
                "agent": "arch-ctm",
                "team": "atm-dev",
                "session_id": "sess-non-lead",
                "source": {"kind": "claude_hook"},
            }),
        };
        let req_str = serde_json::to_string(&req).unwrap();
        let resp = handle_hook_event_with_transient_retry(&req_str, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(
            payload["processed"].as_bool().unwrap(),
            "non-lead claude_hook session_start must be accepted"
        );
    }

    /// `atm_mcp` source: `session_start` from a non-lead member is accepted.
    ///
    /// MCP proxies manage their own Codex agent sessions, so any team member
    /// may emit lifecycle events via this source.
    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_atm_mcp_source_non_lead_session_start_accepted() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();

        let req = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-atm-mcp".to_string(),
            command: "hook-event".to_string(),
            payload: serde_json::json!({
                "event": "session_start",
                "agent": "arch-ctm",
                "team": "atm-dev",
                "session_id": "codex:abc-session-1",
                "source": {"kind": "atm_mcp"},
            }),
        };
        let req_str = serde_json::to_string(&req).unwrap();
        let resp = handle_hook_event_with_transient_retry(&req_str, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(
            payload["processed"].as_bool().unwrap(),
            "atm_mcp non-lead session_start must be accepted; got: {payload}"
        );
        // Verify the session was actually registered.
        let reg = sr.lock().unwrap();
        let record = reg
            .query("arch-ctm")
            .expect("arch-ctm must be in session registry");
        assert_eq!(record.session_id, "codex:abc-session-1");
    }

    /// `unknown` source still accepts non-lead `session_start`.
    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_unknown_source_non_lead_session_start_accepted() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();

        // Explicitly set source.kind = "unknown"
        let req = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-unknown-src".to_string(),
            command: "hook-event".to_string(),
            payload: serde_json::json!({
                "event": "session_start",
                "agent": "arch-ctm",
                "team": "atm-dev",
                "session_id": "sess-unknown",
                "source": {"kind": "unknown"},
            }),
        };
        let req_str = serde_json::to_string(&req).unwrap();
        let resp = handle_hook_event_with_transient_retry(&req_str, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(
            payload["processed"].as_bool().unwrap(),
            "unknown source must accept non-lead session_start"
        );
    }

    /// Missing `source` field also accepts non-lead `session_start`.
    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_absent_source_non_lead_session_start_accepted() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();

        // No "source" field in payload.
        let req = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-no-src".to_string(),
            command: "hook-event".to_string(),
            payload: serde_json::json!({
                "event": "session_start",
                "agent": "arch-ctm",
                "team": "atm-dev",
                "session_id": "sess-no-src",
            }),
        };
        let req_str = serde_json::to_string(&req).unwrap();
        let resp = handle_hook_event_with_transient_retry(&req_str, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(
            payload["processed"].as_bool().unwrap(),
            "absent source must default to Unknown and accept non-lead session_start"
        );
    }

    /// Legacy E.3 payloads used a flat string for `source` on session_start
    /// (for example `"init"` / `"compact"`). The E.7 parser expects an object
    /// (`{"kind":"..."}`), so legacy string payloads must degrade gracefully
    /// to `unknown` while still accepting non-lead `session_start`.
    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_legacy_flat_string_source_degrades_to_unknown() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();

        let req = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-legacy-flat-source".to_string(),
            command: "hook-event".to_string(),
            payload: serde_json::json!({
                "event": "session_start",
                "agent": "arch-ctm",
                "team": "atm-dev",
                "session_id": "sess-legacy-flat",
                "source": "init",
            }),
        };
        let req_str = serde_json::to_string(&req).unwrap();
        let resp = handle_hook_event_with_transient_retry(&req_str, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(
            payload["processed"].as_bool().unwrap(),
            "legacy flat-string source must degrade to Unknown and still accept session_start"
        );
    }

    /// `agent_hook` source: `session_start` from a non-lead member is accepted
    /// (same policy as `atm_mcp`).
    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_agent_hook_source_non_lead_session_start_accepted() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();

        let req = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-agent-hook".to_string(),
            command: "hook-event".to_string(),
            payload: serde_json::json!({
                "event": "session_start",
                "agent": "arch-ctm",
                "team": "atm-dev",
                "session_id": "codex:agent-hook-sess",
                "source": {"kind": "agent_hook"},
            }),
        };
        let req_str = serde_json::to_string(&req).unwrap();
        let resp = handle_hook_event_with_transient_retry(&req_str, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(
            payload["processed"].as_bool().unwrap(),
            "agent_hook non-lead session_start must be accepted"
        );
    }

    /// `atm_mcp` source: `session_end` from a non-lead member is accepted.
    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn test_hook_event_atm_mcp_source_non_lead_session_end_accepted() {
        let _fixture = setup_hook_auth_fixture("atm-dev", "team-lead", &["team-lead", "arch-ctm"]);
        let store = make_store();
        let sr = make_sr();
        // Pre-populate registry so mark_dead has something to work with.
        sr.lock()
            .unwrap()
            .upsert_for_team("atm-dev", "arch-ctm", "codex:sess-end-test", 0);

        let req = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-mcp-end".to_string(),
            command: "hook-event".to_string(),
            payload: serde_json::json!({
                "event": "session_end",
                "agent": "arch-ctm",
                "team": "atm-dev",
                "session_id": "codex:sess-end-test",
                "source": {"kind": "atm_mcp"},
            }),
        };
        let req_str = serde_json::to_string(&req).unwrap();
        let resp = handle_hook_event_with_transient_retry(&req_str, &store, &sr).await;
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert!(
            payload["processed"].as_bool().unwrap(),
            "atm_mcp non-lead session_end must be accepted"
        );
        // Verify session marked dead.
        use crate::daemon::session_registry::SessionState;
        let reg = sr.lock().unwrap();
        let record = reg
            .query("arch-ctm")
            .expect("arch-ctm must remain in registry");
        assert_eq!(record.state, SessionState::Dead);
    }

    // ── G.7 stream-event and agent-stream-state tests ────────────────────────

    #[test]
    #[cfg(unix)]
    fn test_is_stream_event_command_detection() {
        assert!(is_stream_event_command(
            r#"{"version":1,"request_id":"r1","command":"stream-event","payload":{}}"#
        ));
        assert!(is_stream_event_command(
            r#"{"version":1,"request_id":"r1","command": "stream-event","payload":{}}"#
        ));
        assert!(!is_stream_event_command(
            r#"{"version":1,"request_id":"r1","command":"agent-state","payload":{}}"#
        ));
        assert!(!is_stream_event_command(
            r#"{"version":1,"request_id":"r1","command":"hook-event","payload":{}}"#
        ));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_stream_event_command_accepts_turn_started_and_updates_store() {
        use agent_team_mail_core::daemon_stream::StreamTurnStatus;

        let store = new_stream_state_store();
        let req_json = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "request_id": "se-1",
            "command": "stream-event",
            "payload": {
                "kind": "turn_started",
                "agent": "arch-ctm",
                "thread_id": "th-1",
                "turn_id": "turn-abc",
                "transport": "app-server"
            }
        });
        let req_str = serde_json::to_string(&req_json).unwrap();

        let resp = handle_stream_event_command(&req_str, &store, &new_stream_event_sender()).await;
        assert_eq!(resp.status, "ok", "stream-event should succeed");
        assert!(
            resp.payload.unwrap()["ok"].as_bool().unwrap(),
            "response should contain ok: true"
        );

        // Verify the state store was updated.
        let guard = store.lock().unwrap();
        let state = guard.get("arch-ctm").expect("agent should be in store");
        assert_eq!(state.turn_status, StreamTurnStatus::Busy);
        assert_eq!(state.turn_id.as_deref(), Some("turn-abc"));
        assert_eq!(state.thread_id.as_deref(), Some("th-1"));
        assert_eq!(state.transport.as_deref(), Some("app-server"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_stream_event_command_turn_completed_sets_terminal() {
        use agent_team_mail_core::daemon_stream::StreamTurnStatus;

        let store = new_stream_state_store();

        // First send TurnStarted so agent is Busy.
        let started = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "request_id": "se-2a",
            "command": "stream-event",
            "payload": {
                "kind": "turn_started",
                "agent": "worker-1",
                "thread_id": "th-2",
                "turn_id": "turn-def",
                "transport": "cli-json"
            }
        });
        let resp = handle_stream_event_command(
            &serde_json::to_string(&started).unwrap(),
            &store,
            &new_stream_event_sender(),
        )
        .await;
        assert_eq!(resp.status, "ok");

        // Now TurnCompleted.
        let completed = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "request_id": "se-2b",
            "command": "stream-event",
            "payload": {
                "kind": "turn_completed",
                "agent": "worker-1",
                "thread_id": "th-2",
                "turn_id": "turn-def",
                "status": "completed",
                "transport": "cli-json"
            }
        });
        let resp = handle_stream_event_command(
            &serde_json::to_string(&completed).unwrap(),
            &store,
            &new_stream_event_sender(),
        )
        .await;
        assert_eq!(resp.status, "ok");

        let guard = store.lock().unwrap();
        let state = guard.get("worker-1").unwrap();
        assert_eq!(state.turn_status, StreamTurnStatus::Terminal);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_stream_event_command_invalid_payload() {
        let store = new_stream_state_store();
        let req_json = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "request_id": "se-bad",
            "command": "stream-event",
            "payload": {"invalid": true}
        });
        let req_str = serde_json::to_string(&req_json).unwrap();
        let resp = handle_stream_event_command(&req_str, &store, &new_stream_event_sender()).await;
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "INVALID_PAYLOAD");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_stream_event_command_accepts_stream_error_observability_event() {
        use agent_team_mail_core::daemon_stream::StreamTurnStatus;

        let store = new_stream_state_store();
        let req_json = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "request_id": "se-err",
            "command": "stream-event",
            "payload": {
                "kind": "stream_error",
                "agent_id": "arch-ctm",
                "session_id": "th-err",
                "error_summary": "socket closed"
            }
        });
        let req_str = serde_json::to_string(&req_json).unwrap();
        let resp = handle_stream_event_command(&req_str, &store, &new_stream_event_sender()).await;
        assert_eq!(resp.status, "ok");

        // StreamError should not mutate turn-state beyond creating an agent entry.
        let guard = store.lock().unwrap();
        let state = guard.get("arch-ctm").expect("agent should be tracked");
        assert_eq!(state.turn_status, StreamTurnStatus::Idle);
        assert!(state.turn_id.is_none());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_stream_event_command_accepts_dropped_counters_event() {
        use agent_team_mail_core::daemon_stream::StreamTurnStatus;

        let store = new_stream_state_store();
        let req_json = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "request_id": "se-counters",
            "command": "stream-event",
            "payload": {
                "kind": "dropped_counters",
                "agent_id": "proxy:all",
                "dropped": 5,
                "unknown": 2
            }
        });
        let req_str = serde_json::to_string(&req_json).unwrap();
        let resp = handle_stream_event_command(&req_str, &store, &new_stream_event_sender()).await;
        assert_eq!(resp.status, "ok");

        let guard = store.lock().unwrap();
        let state = guard.get("proxy:all").expect("agent should be tracked");
        assert_eq!(state.turn_status, StreamTurnStatus::Idle);
        assert!(state.turn_id.is_none());
    }

    #[test]
    fn test_agent_stream_state_returns_state_after_event() {
        use agent_team_mail_core::daemon_stream::{AgentStreamState, DaemonStreamEvent};

        let store = new_stream_state_store();
        // Pre-populate the store with a known state.
        {
            let mut guard = store.lock().unwrap();
            let mut state = AgentStreamState::default();
            state.apply(&DaemonStreamEvent::TurnStarted {
                agent: "test-agent".to_string(),
                thread_id: "th-x".to_string(),
                turn_id: "t-99".to_string(),
                transport: "mcp".to_string(),
            });
            guard.insert("test-agent".to_string(), state);
        }

        let req = make_request(
            "agent-stream-state",
            serde_json::json!({"agent": "test-agent"}),
        );
        let resp = handle_agent_stream_state(&req, &store);
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["turn_status"].as_str(), Some("busy"));
        assert_eq!(payload["turn_id"].as_str(), Some("t-99"));
        assert_eq!(payload["thread_id"].as_str(), Some("th-x"));
        assert_eq!(payload["transport"].as_str(), Some("mcp"));
    }

    #[test]
    fn test_agent_stream_state_missing_agent_field() {
        let store = new_stream_state_store();
        let req = make_request("agent-stream-state", serde_json::json!({}));
        let resp = handle_agent_stream_state(&req, &store);
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "MISSING_PARAMETER");
    }

    #[test]
    fn test_agent_stream_state_unknown_agent() {
        let store = new_stream_state_store();
        let req = make_request("agent-stream-state", serde_json::json!({"agent": "ghost"}));
        let resp = handle_agent_stream_state(&req, &store);
        assert_eq!(resp.status, "error");
        assert_eq!(resp.error.unwrap().code, "AGENT_NOT_FOUND");
    }

    #[test]
    fn test_agent_stream_state_via_parse_and_dispatch() {
        use agent_team_mail_core::daemon_stream::{AgentStreamState, DaemonStreamEvent};

        let store = make_store();
        let ps = make_ps();
        let sr = make_sr();
        let ss = new_stream_state_store();

        // Pre-populate stream state.
        {
            let mut guard = ss.lock().unwrap();
            let mut state = AgentStreamState::default();
            state.apply(&DaemonStreamEvent::TurnIdle {
                agent: "worker-2".to_string(),
                turn_id: "t-idle".to_string(),
                transport: "app-server".to_string(),
            });
            guard.insert("worker-2".to_string(), state);
        }

        let req_json = format!(
            r#"{{"version":{},"request_id":"r1","command":"agent-stream-state","payload":{{"agent":"worker-2"}}}}"#,
            PROTOCOL_VERSION
        );
        let resp = parse_and_dispatch(&req_json, &store, &ps, &sr, &ss).unwrap();
        assert_eq!(resp.status, "ok");
        let payload = resp.payload.unwrap();
        assert_eq!(payload["turn_status"].as_str(), Some("idle"));
    }

    // ── Broadcast channel tests ───────────────────────────────────────────────

    /// Creating a sender should produce a valid channel that accepts subscribers.
    #[test]
    fn test_new_stream_event_sender_creates_valid_channel() {
        let sender = new_stream_event_sender();
        // subscribing must not panic
        let _rx = sender.subscribe();
        // receiver count is at least 1 (the one we just created)
        // This confirms the channel is alive.
    }

    /// Sending a stream-event command publishes the event to broadcast subscribers.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_stream_event_broadcast_received_by_subscriber() {
        use agent_team_mail_core::daemon_stream::DaemonStreamEvent;

        let store = new_stream_state_store();
        let sender = new_stream_event_sender();
        let mut rx = sender.subscribe();

        let req_json = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "request_id": "bcast-1",
            "command": "stream-event",
            "payload": {
                "kind": "turn_started",
                "agent": "arch-ctm",
                "thread_id": "th-bcast",
                "turn_id": "turn-bcast-1",
                "transport": "mcp"
            }
        });
        let req_str = serde_json::to_string(&req_json).unwrap();

        let resp = handle_stream_event_command(&req_str, &store, &sender).await;
        assert_eq!(resp.status, "ok");

        // The subscriber should receive the event without blocking.
        let event = rx
            .try_recv()
            .expect("event should be immediately available");
        assert!(
            matches!(
                &event,
                DaemonStreamEvent::TurnStarted { agent, turn_id, .. }
                if agent == "arch-ctm" && turn_id == "turn-bcast-1"
            ),
            "unexpected event: {event:?}"
        );
    }

    // ── handle_log_event_command tests ───────────────────────────────────────

    /// Build a valid log-event socket request JSON string.
    #[cfg(unix)]
    fn make_log_event_request(request_id: &str, payload: serde_json::Value) -> String {
        serde_json::to_string(&serde_json::json!({
            "version": PROTOCOL_VERSION,
            "request_id": request_id,
            "command": "log-event",
            "payload": payload
        }))
        .unwrap()
    }

    /// A valid `LogEventV1` payload for use in tests.
    #[cfg(unix)]
    fn valid_log_event_payload() -> serde_json::Value {
        serde_json::json!({
            "v": 1,
            "ts": "2026-02-23T00:00:00Z",
            "level": "info",
            "source_binary": "atm",
            "hostname": "testhost",
            "pid": 1234,
            "target": "atm::test",
            "action": "test_action",
            "fields": {},
            "spans": []
        })
    }

    /// Valid event is accepted and response has `accepted: true`.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_handle_log_event_valid_accepted() {
        use crate::daemon::log_writer::new_log_event_queue;

        let queue = new_log_event_queue();
        let req_str = make_log_event_request("test-1", valid_log_event_payload());

        let resp = handle_log_event_command(&req_str, &queue).await;
        assert_eq!(resp.status, "ok", "expected ok status, got: {resp:?}");
        let payload = resp.payload.expect("response should have a payload");
        assert_eq!(
            payload["accepted"].as_bool(),
            Some(true),
            "event should be accepted"
        );
    }

    /// When the queue is full, response has `accepted: false`.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_handle_log_event_queue_full_returns_accepted_false() {
        use crate::daemon::log_writer::BoundedQueue;

        // Build a tiny queue (capacity 1) and fill it.
        let queue = std::sync::Arc::new(tokio::sync::Mutex::new(BoundedQueue::new(1)));
        {
            let event_payload = valid_log_event_payload();
            let event: agent_team_mail_core::logging_event::LogEventV1 =
                serde_json::from_value(event_payload).unwrap();
            let mut q = queue.lock().await;
            q.push(event);
        }

        let req_str = make_log_event_request("test-full", valid_log_event_payload());
        let resp = handle_log_event_command(&req_str, &queue).await;
        assert_eq!(resp.status, "ok", "status should be ok even on full queue");
        let payload = resp.payload.expect("response should have a payload");
        assert_eq!(
            payload["accepted"].as_bool(),
            Some(false),
            "event should not be accepted when queue is full"
        );
    }

    /// A `LogEventV1` with `v: 2` triggers a `VERSION_MISMATCH` error.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_handle_log_event_version_mismatch() {
        use crate::daemon::log_writer::new_log_event_queue;

        let queue = new_log_event_queue();
        let mut payload = valid_log_event_payload();
        payload["v"] = serde_json::json!(2); // unsupported schema version
        let req_str = make_log_event_request("test-ver", payload);

        let resp = handle_log_event_command(&req_str, &queue).await;
        assert_eq!(resp.status, "error");
        assert_eq!(
            resp.error.unwrap().code,
            "VERSION_MISMATCH",
            "wrong schema version should produce VERSION_MISMATCH"
        );
    }

    /// Malformed JSON (not a valid `SocketRequest`) triggers `INVALID_PAYLOAD`.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_handle_log_event_malformed_json() {
        use crate::daemon::log_writer::new_log_event_queue;

        let queue = new_log_event_queue();
        let resp = handle_log_event_command("not-json{{", &queue).await;
        assert_eq!(resp.status, "error");
        assert_eq!(
            resp.error.unwrap().code,
            "INVALID_PAYLOAD",
            "malformed JSON should produce INVALID_PAYLOAD"
        );
    }

    /// A `SocketRequest` whose `payload` is missing the required `action` field
    /// triggers `INVALID_PAYLOAD`.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_handle_log_event_missing_required_field() {
        use crate::daemon::log_writer::new_log_event_queue;

        let queue = new_log_event_queue();
        // Build a payload without the required `action` field.
        let payload = serde_json::json!({
            "v": 1,
            "ts": "2026-02-23T00:00:00Z",
            "level": "info",
            "source_binary": "atm",
            "hostname": "testhost",
            "pid": 1234,
            "target": "atm::test"
            // "action" intentionally omitted
        });
        let req_str = make_log_event_request("test-missing", payload);

        let resp = handle_log_event_command(&req_str, &queue).await;
        assert_eq!(resp.status, "error");
        assert_eq!(
            resp.error.unwrap().code,
            "INVALID_PAYLOAD",
            "missing required field should produce INVALID_PAYLOAD"
        );
    }

    /// A `LogEventV1` with an empty `action` field fails validation and
    /// triggers `INVALID_PAYLOAD`.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_handle_log_event_empty_action_fails_validation() {
        use crate::daemon::log_writer::new_log_event_queue;

        let queue = new_log_event_queue();
        let mut payload = valid_log_event_payload();
        payload["action"] = serde_json::json!(""); // empty action fails validate()
        let req_str = make_log_event_request("test-empty-action", payload);

        let resp = handle_log_event_command(&req_str, &queue).await;
        assert_eq!(resp.status, "error");
        assert_eq!(
            resp.error.unwrap().code,
            "INVALID_PAYLOAD",
            "empty action should produce INVALID_PAYLOAD"
        );
    }
}
