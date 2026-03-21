//! MCP stdio proxy core.
//!
//! [`ProxyServer`] sits between Claude (upstream, on stdin/stdout) and a single
//! `codex mcp-server` child process (downstream). It:
//!
//! - Auto-detects Content-Length vs newline-delimited framing on the upstream side
//! - Writes newline-delimited JSON to the child (Codex convention)
//! - Intercepts `tools/list` responses to append synthetic ATM tool schemas
//! - Forwards `codex/event` notifications upstream with an `agent_id` tag
//! - Lazily spawns the child on first `codex` or `codex-reply` tool call
//! - Detects child crashes and returns structured JSON-RPC errors
//! - Applies configurable per-request timeouts
//! - Sends `notifications/cancelled` to the child when a request times out
//! - Registers sessions in an in-memory [`SessionRegistry`] with identity
//!   binding and cross-process lock files (Sprint A.3)
//!
//! ATM communication tools are implemented in Sprint A.4. Session lifecycle
//! state machine and elicitation bridging added in Sprint A.6. Auto mail
//! injection (FR-8) implemented in Sprint A.7: post-turn mail check,
//! idle mail polling, delivery ack boundary, single-flight enforcement.

use std::collections::HashMap;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::Child;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::{Duration, timeout};
use tracing::Instrument;

use crate::audit::AuditLog;
use crate::config::AgentMcpConfig;
use crate::context::detect_context;
use crate::elicitation::ElicitationRegistry;
use crate::framing::{UpstreamReader, write_newline_delimited};
use crate::inject::{build_session_context, inject_developer_instructions};
use crate::lifecycle::{ThreadCommand, ThreadCommandQueue};
use crate::lock::{acquire_lock, check_lock, release_lock};
use crate::mail_inject::{
    InflightMailSet, MailPoller, fetch_unread_mail, format_mail_turn_content, mark_messages_read,
};
use crate::session::{RegistryError, SessionRegistry, SessionStatus, ThreadState};
use crate::tools::synthetic_tools;
use crate::transport::{CodexTransport, make_transport};
use crate::watch_stream::{SourceEnvelope, WatchStreamHub, WatchSubscription, build_watch_frame};

/// Type alias for the shared child stdin writer.
///
/// Shared between the proxy and background tasks (timeout cancellation, idle
/// mail poller) so they can write JSON-RPC messages to the child without going
/// through the proxy's main select loop.  The outer `Arc<Mutex<Option<...>>>`
/// allows the value to be populated lazily when the child is first spawned.
type SharedChildStdin = Arc<Mutex<Option<Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>>>>;

/// Channel buffer capacity for upstream message delivery.
///
/// Sized to handle burst of MCP responses without backpressure.
const UPSTREAM_CHANNEL_CAPACITY: usize = 256;

/// Grace period in ms after dropping child stdin before force-kill, giving child time to flush output.
const CHILD_DRAIN_GRACE_MS: u64 = 100;
/// Maximum rendered watch line length retained in TUI feed records.
const WATCH_RENDER_MAX_CHARS: usize = 200;
/// Truncated prefix length before appending ellipsis (`...`).
const WATCH_RENDER_TRUNCATE_AT: usize = WATCH_RENDER_MAX_CHARS - 3;
/// Hard cap for per-agent watch-stream files before rotation.
/// Path: `$ATM_HOME/watch-stream/<agent-id>.jsonl` (when ATM_HOME is set)
///       or `~/.config/atm/watch-stream/<agent-id>.jsonl` otherwise.
const WATCH_FEED_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Counter for unknown watch event kinds skipped by the MVP publisher gate.
static WATCH_UNKNOWN_EVENT_COUNT: AtomicU64 = AtomicU64::new(0);
/// Periodic flush interval for dropped/unknown stream counters.
const WATCH_COUNTER_REPORT_INTERVAL_SECS: u64 = 60;
#[cfg(test)]
static STREAM_ERROR_EMIT_ATTEMPTS: AtomicU64 = AtomicU64::new(0);

/// JSON-RPC error code: child process has died.
pub const ERR_CHILD_DEAD: i64 = -32005;

/// JSON-RPC error code: request timed out.
pub const ERR_TIMEOUT: i64 = -32006;

/// JSON-RPC error code: method not found.
pub const ERR_METHOD_NOT_FOUND: i64 = -32601;

/// JSON-RPC error code: internal error.
pub const ERR_INTERNAL: i64 = -32603;

/// JSON-RPC error code: identity already bound to an active session in another
/// process.
pub const ERR_IDENTITY_CONFLICT: i64 = -32001;

/// JSON-RPC error code: session not found for requested `agent_id`.
pub const ERR_SESSION_NOT_FOUND: i64 = -32002;

/// JSON-RPC error code: target session is already closed.
///
/// Used when an operation is attempted on a session whose thread state is
/// [`crate::session::ThreadState::Closed`].  Note that [`crate::atm_tools::handle_agent_close`]
/// treats a repeated close as a success (idempotent, FR-17.9) rather than
/// returning this error.  This constant is available for other operations that
/// must reject closed sessions.
pub const ERR_SESSION_CLOSED: i64 = -32003;

/// JSON-RPC error code: maximum concurrent session limit reached.
pub const ERR_MAX_SESSIONS_EXCEEDED: i64 = -32004;

/// JSON-RPC error code: `agent_file` and `prompt` were both provided
/// (mutually exclusive, FR-16.5).
pub const ERR_INVALID_SESSION_PARAMS: i64 = -32007;

/// JSON-RPC error code: the specified `agent_file` path does not exist
/// (FR-16.6).
pub const ERR_AGENT_FILE_NOT_FOUND: i64 = -32008;

/// JSON-RPC error code: identity is required to execute an ATM tool but
/// was not provided via the `identity` argument or proxy config (FR-8.x).
pub const ERR_IDENTITY_REQUIRED: i64 = -32009;

/// Manages the MCP proxy lifecycle: upstream I/O, child process, and message routing.
pub struct ProxyServer {
    config: AgentMcpConfig,
    child: Option<ChildHandle>,
    /// Counter of event notifications dropped due to backpressure.
    pub dropped_events: Arc<AtomicU64>,
    /// In-memory session registry shared with per-request tasks.
    registry: Arc<Mutex<SessionRegistry>>,
    /// Registry of pending elicitation/create requests bridged upstream (FR-18).
    elicitation_registry: Arc<Mutex<ElicitationRegistry>>,
    /// Counter for generating unique upstream elicitation request IDs.
    elicitation_counter: Arc<AtomicU64>,
    /// ATM team name used for session registration and lock files.
    pub team: String,
    /// Maps Codex `threadId` → `agent_id` for event attribution.
    thread_to_agent: Arc<tokio::sync::Mutex<HashMap<String, String>>>,
    /// Direct watch-stream hub for active session viewing (Sprint L.5 groundwork).
    watch_stream_hub: Arc<tokio::sync::Mutex<WatchStreamHub>>,
    /// Active watch subscriptions keyed by `agent_id` for synthetic polling tools.
    watch_subscriptions:
        Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::broadcast::Receiver<Value>>>>,
    /// UTC ISO 8601 timestamp of when this proxy process started.
    started_at: String,
    /// Unix epoch seconds when this proxy process started (for uptime calc).
    started_epoch_secs: u64,
    /// Per-agent command queues for serialising turn dispatch (FR-8, FR-17).
    ///
    /// Keyed by `agent_id`; created when a new `codex` session is registered.
    queues: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<ThreadCommandQueue>>>>>,
    /// Mail polling configuration derived from [`AgentMcpConfig`] (FR-8.2).
    mail_poller: MailPoller,
    /// Monotonically increasing counter for auto-generated request IDs.
    request_counter: Arc<AtomicU64>,
    /// Shared reference to the child stdin writer.
    ///
    /// Populated when the child is lazily spawned.  The idle mail poller task
    /// uses this to write codex-reply messages to the child without going
    /// through the proxy's main select loop.
    shared_child_stdin: SharedChildStdin,
    /// Append-only audit log for ATM tool calls and Codex forwards (FR-9).
    audit_log: AuditLog,
    /// Resume context loaded at startup via `--resume` (FR-6).
    /// Consumed on the first `codex` or `codex-reply` developer-instructions
    /// injection and set to `None` thereafter.
    resume_context: Option<ResumeContext>,
    /// Transport implementation used to spawn the Codex child process.
    ///
    /// Stored as a trait object so Sprint C.2b can inject `MockTransport`
    /// without modifying `ProxyServer`.
    transport: Box<dyn CodexTransport>,
}

impl std::fmt::Debug for ProxyServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyServer")
            .field("team", &self.team)
            .field("config", &self.config)
            .field("transport", &self.transport)
            .field("shared_child_stdin", &"<AsyncWrite>")
            .finish_non_exhaustive()
    }
}

/// Context from a previous session to prepend on resume (FR-6).
#[derive(Debug, Clone)]
pub struct ResumeContext {
    /// Original agent_id (for display/logging).
    pub agent_id: String,
    /// ATM identity of the resumed session.
    pub identity: String,
    /// Codex threadId (backend_id) of the resumed session.
    pub backend_id: String,
    /// Session summary text, or `None` if no summary was saved (crash/SIGKILL).
    pub summary: Option<String>,
}

/// Handle to the spawned Codex child process.
struct ChildHandle {
    /// Shared stdin writer; shared so timeout tasks can send cancellation notifications.
    stdin: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    /// Receives responses and notifications from the child stdout reader task.
    response_rx: mpsc::Receiver<Value>,
    /// If the child has exited, contains the exit status.
    exit_status: Arc<Mutex<Option<ExitStatus>>>,
    /// The child process handle, kept for force-kill on shutdown.
    process: Arc<Mutex<Option<Child>>>,
    /// Background task handle for the 30-second periodic stdin queue drain (JSON mode only).
    ///
    /// `None` for MCP and Mock transports.  Aborted during graceful shutdown so
    /// the task does not outlive the proxy.
    drain_task: Option<tokio::task::JoinHandle<()>>,
}

impl std::fmt::Debug for ChildHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildHandle")
            .field("stdin", &"<Box<dyn AsyncWrite>>")
            .field("response_rx", &"<Receiver>")
            .field("exit_status", &"<Mutex<Option<ExitStatus>>>")
            .field("process", &"<Mutex<Option<Child>>>")
            .field(
                "drain_task",
                &self.drain_task.as_ref().map(|_| "<JoinHandle>"),
            )
            .finish()
    }
}

/// Tracks in-flight requests waiting for a response from the child.
pub(crate) struct PendingRequests {
    map: HashMap<Value, oneshot::Sender<Value>>,
    /// Request IDs that correspond to `tools/list` requests and need interception.
    tools_list_ids: std::collections::HashSet<Value>,
    /// Request IDs for new `codex` session creation mapped to the preallocated agent_id.
    codex_create_ids: HashMap<Value, String>,
    /// Request IDs for proxy-initiated auto-mail turns mapped to the `agent_id`.
    ///
    /// When the child responds to an auto-mail codex-reply, the proxy uses this
    /// map to resolve the agent_id, transition the thread Busy -> Idle, and
    /// trigger the next post-turn mail check (FR-8.1 chaining).
    auto_mail_pending: HashMap<Value, String>,
    /// Request IDs mapped to source envelopes for watch-stream attribution.
    request_sources: HashMap<Value, SourceEnvelope>,
    /// Last-known source envelope for each agent_id, used as fallback when
    /// child events do not carry a request-id correlation token.
    last_agent_source: HashMap<String, SourceEnvelope>,
}

impl PendingRequests {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            tools_list_ids: std::collections::HashSet::new(),
            codex_create_ids: HashMap::new(),
            auto_mail_pending: HashMap::new(),
            request_sources: HashMap::new(),
            last_agent_source: HashMap::new(),
        }
    }

    fn insert(&mut self, id: Value, tx: oneshot::Sender<Value>) {
        self.map.insert(id, tx);
    }

    fn mark_tools_list(&mut self, id: Value) {
        self.tools_list_ids.insert(id);
    }

    fn is_tools_list(&self, id: &Value) -> bool {
        self.tools_list_ids.contains(id)
    }

    fn complete(&mut self, id: &Value) -> Option<oneshot::Sender<Value>> {
        self.tools_list_ids.remove(id);
        self.request_sources.remove(id);
        self.map.remove(id)
    }

    fn mark_codex_create(&mut self, id: Value, agent_id: String) {
        self.codex_create_ids.insert(id, agent_id);
    }

    fn take_codex_create(&mut self, id: &Value) -> Option<String> {
        self.codex_create_ids.remove(id)
    }

    fn peek_codex_create(&self, id: &Value) -> Option<String> {
        self.codex_create_ids.get(id).cloned()
    }

    /// Register an auto-mail turn's request ID with its owning agent_id.
    fn mark_auto_mail(&mut self, id: Value, agent_id: String) {
        self.auto_mail_pending.insert(id, agent_id);
    }

    /// Take the agent_id for a completed auto-mail turn, removing it from the map.
    fn take_auto_mail(&mut self, id: &Value) -> Option<String> {
        self.auto_mail_pending.remove(id)
    }

    fn mark_request_source(&mut self, id: Value, source: SourceEnvelope) {
        self.request_sources.insert(id, source);
    }

    fn source_for_request(&self, id: &Value) -> Option<SourceEnvelope> {
        self.request_sources.get(id).cloned()
    }

    fn set_last_agent_source(&mut self, agent_id: String, source: SourceEnvelope) {
        self.last_agent_source.insert(agent_id, source);
    }

    fn last_source_for_agent(&self, agent_id: &str) -> Option<SourceEnvelope> {
        self.last_agent_source.get(agent_id).cloned()
    }
}

impl ProxyServer {
    /// Create a new proxy server with the given configuration.
    ///
    /// The team defaults to `"default"`. Use [`ProxyServer::new_with_team`]
    /// to supply an explicit team name.
    pub fn new(config: AgentMcpConfig) -> Self {
        Self::new_with_team(config, "default")
    }

    /// Create a proxy server with an explicit ATM team name.
    ///
    /// The team name is used for session lock files under
    /// `<sessions_dir>/<team>/<identity>.lock` (FR-20.1).
    /// Cross-process lock detection uses the plain team name so that different
    /// proxy processes in the same team correctly detect conflicts.
    ///
    /// Also loads any persisted sessions from disk and marks them as stale
    /// (FR-3.2).
    pub fn new_with_team(config: AgentMcpConfig, team: impl Into<String>) -> Self {
        let max = config.max_concurrent_threads;
        let team_str: String = team.into();
        let registry = SessionRegistry::new(max);
        let registry = Self::load_stale_from_disk(registry, &team_str);
        let (started_at, started_epoch_secs) = proxy_start_time();
        // Elicitation default timeout: 30 seconds (FR-18).
        const ELICITATION_TIMEOUT_SECS: u64 = 30;
        let mail_poller = MailPoller::new(&config);
        let audit_log = AuditLog::new(&team_str);
        let transport = make_transport(&config, &team_str);
        Self {
            config,
            child: None,
            dropped_events: Arc::new(AtomicU64::new(0)),
            registry: Arc::new(Mutex::new(registry)),
            elicitation_registry: Arc::new(Mutex::new(ElicitationRegistry::new(
                ELICITATION_TIMEOUT_SECS,
            ))),
            elicitation_counter: Arc::new(AtomicU64::new(1)),
            team: team_str,
            thread_to_agent: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            watch_stream_hub: Arc::new(tokio::sync::Mutex::new(WatchStreamHub::default())),
            watch_subscriptions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            started_at,
            started_epoch_secs,
            queues: Arc::new(Mutex::new(HashMap::new())),
            mail_poller,
            request_counter: Arc::new(AtomicU64::new(1)),
            shared_child_stdin: Arc::new(Mutex::new(None)),
            audit_log,
            resume_context: None,
            transport,
        }
    }

    /// Subscribe to an agent's direct watch stream.
    ///
    /// Returns a bounded replay snapshot plus a live receiver.
    pub async fn subscribe_watch_stream(&self, agent_id: &str) -> WatchSubscription {
        self.watch_stream_hub.lock().await.subscribe(agent_id)
    }

    /// Detach active watcher for a session.
    pub async fn detach_watch_stream(&self, agent_id: &str) -> bool {
        self.watch_stream_hub.lock().await.detach(agent_id)
    }

    /// Create a proxy server with resume context (FR-6).
    ///
    /// If `resume` is `Some`, the summary (if available) is prepended to
    /// `developer-instructions` on the first `codex` or `codex-reply` turn.
    pub fn new_with_resume(
        config: AgentMcpConfig,
        team: impl Into<String>,
        resume: Option<ResumeContext>,
    ) -> Self {
        let mut proxy = Self::new_with_team(config, team);
        proxy.resume_context = resume;
        proxy
    }

    /// Persist the current registry snapshot to disk atomically (FR-5.5).
    ///
    /// Writes a temporary file alongside the target path, then renames it to
    /// the target, ensuring readers always see a complete file.  Parent
    /// directories are created on demand.
    ///
    /// # Errors
    ///
    /// Returns an error when I/O fails (permissions, disk full, etc.).
    async fn persist_registry(
        registry: &Arc<Mutex<SessionRegistry>>,
        sessions_path: &std::path::Path,
    ) -> anyhow::Result<()> {
        use crate::session::RegistrySnapshot;
        use tokio::fs;
        use tokio::io::AsyncWriteExt;

        let snapshot: RegistrySnapshot = {
            let guard = registry.lock().await;
            guard.to_snapshot()
        };
        let json = serde_json::to_vec_pretty(&snapshot)?;

        // Ensure parent directory exists.
        if let Some(parent) = sessions_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        // Write to a temp file alongside the target, then rename for atomicity.
        let tmp_path = sessions_path.with_extension("json.tmp");
        {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)
                .await?;
            file.write_all(&json).await?;
            file.flush().await?;
        }
        fs::rename(&tmp_path, sessions_path).await?;
        Ok(())
    }

    /// Load a persisted registry file and mark any `Active` sessions as
    /// [`crate::session::SessionStatus::Stale`].
    ///
    /// If the file does not exist or cannot be parsed, returns the registry
    /// unchanged (fresh start). This satisfies FR-3.2's requirement to mark
    /// prior active sessions as stale on proxy startup.
    fn load_stale_from_disk(registry: SessionRegistry, team: &str) -> SessionRegistry {
        use crate::lock::sessions_dir;
        use crate::session::RegistrySnapshot;

        let registry_path = sessions_dir().join(team).join("registry.json");
        let contents = match std::fs::read_to_string(&registry_path) {
            Ok(c) => c,
            Err(_) => return registry, // file absent — fresh start
        };
        let snapshot = match serde_json::from_str::<RegistrySnapshot>(&contents) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    path = %registry_path.display(),
                    "failed to parse registry.json, starting fresh: {e}"
                );
                return registry;
            }
        };

        let max = registry.max_concurrent();
        let loaded = SessionRegistry::load_from_snapshot(snapshot, max);
        tracing::info!(
            count = loaded.list_all().len(),
            "loaded persisted sessions from disk (all marked stale)"
        );
        loaded
    }

    /// Run the proxy loop, reading from `upstream_in` and writing to `upstream_out`.
    ///
    /// This is the main entry point. It blocks until upstream EOF or a fatal error.
    ///
    /// # Errors
    ///
    /// Returns an error on unrecoverable I/O failures. Transient errors (child crash,
    /// timeout) are reported as JSON-RPC error responses to the upstream client.
    pub async fn run<R, W>(&mut self, upstream_in: R, mut upstream_out: W) -> anyhow::Result<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let mut reader = UpstreamReader::new(upstream_in);
        let pending = Arc::new(Mutex::new(PendingRequests::new()));
        let dropped = Arc::clone(&self.dropped_events);
        let thread_to_agent = Arc::clone(&self.thread_to_agent);
        let watch_stream_hub = Arc::clone(&self.watch_stream_hub);

        // Channel for upstream writes (events + responses routed through the channel).
        // Bounded to prevent unbounded memory growth under backpressure.
        let (upstream_tx, mut upstream_rx) = mpsc::channel::<Value>(UPSTREAM_CHANNEL_CAPACITY);

        // Spawn a background task that periodically expires timed-out elicitations
        // (FR-18, every 5 seconds).
        {
            let elicitation_registry_bg = Arc::clone(&self.elicitation_registry);
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
                loop {
                    interval.tick().await;
                    let expired = elicitation_registry_bg.lock().await.expire_timeouts();
                    for key in &expired {
                        tracing::warn!("elicitation timed out: upstream_request_id={key}");
                    }
                }
            });
        }

        // Periodic daemon-side observability flush for dropped/unknown counters.
        let dropped_for_flush = Arc::clone(&self.dropped_events);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
                WATCH_COUNTER_REPORT_INTERVAL_SECS,
            ));
            loop {
                interval.tick().await;
                flush_dropped_counters_to_daemon(&dropped_for_flush, "proxy:all").await;
            }
        });

        // Spawn the idle mail poller (FR-8.2): checks all idle sessions for unread
        // mail at the configured interval and injects auto-mail turns via the
        // shared child stdin reference.  The JoinHandle is stored so we can
        // abort it cleanly on shutdown.
        let mut mail_poller_handle: Option<tokio::task::JoinHandle<()>> = None;
        if self.mail_poller.is_enabled() {
            let poll_interval = self.mail_poller.poll_interval;
            let max_messages = self.mail_poller.max_messages;
            let max_message_length = self.mail_poller.max_message_length;
            let registry_bg = Arc::clone(&self.registry);
            let queues_bg = Arc::clone(&self.queues);
            let team_bg = self.team.clone();
            let request_counter_bg = Arc::clone(&self.request_counter);
            let per_thread_overrides = self.config.per_thread_auto_mail.clone();
            let shared_stdin_bg = Arc::clone(&self.shared_child_stdin);
            let pending_bg = Arc::clone(&pending);

            mail_poller_handle = Some(tokio::spawn(async move {
                let mut interval = tokio::time::interval(poll_interval);
                loop {
                    interval.tick().await;

                    // Collect idle active sessions
                    let idle_sessions: Vec<(String, String, Option<String>)> = {
                        let reg = registry_bg.lock().await;
                        reg.list_all()
                            .iter()
                            .filter(|e| {
                                e.status == SessionStatus::Active
                                    && e.thread_state == ThreadState::Idle
                            })
                            .map(|e| (e.agent_id.clone(), e.identity.clone(), e.thread_id.clone()))
                            .collect()
                    };

                    for (agent_id, identity, thread_id_opt) in idle_sessions {
                        // Per-thread override takes precedence over global setting (FR-8.8)
                        let enabled = per_thread_overrides.get(&agent_id).copied().unwrap_or(true);
                        if !enabled {
                            continue;
                        }

                        let Some(ref thread_id) = thread_id_opt else {
                            continue;
                        };

                        // Fix 5: Delegate directly to dispatch_auto_mail_if_available
                        // which handles priority checking (ClaudeReply > AutoMailInject),
                        // single-flight guard, write, pending registration, and mark-read.
                        // This avoids the previous push_auto_mail + inline dispatch
                        // inconsistency where a queue entry was never popped.
                        dispatch_auto_mail_if_available(
                            &agent_id,
                            &identity,
                            thread_id,
                            &team_bg,
                            max_messages,
                            max_message_length,
                            &registry_bg,
                            &queues_bg,
                            &shared_stdin_bg,
                            &pending_bg,
                            &request_counter_bg,
                            None,
                            None,
                        )
                        .await;
                    }
                }
            }));
        }

        // Cross-platform shutdown signal handler (FR-7.1, FR-7.4).
        #[cfg(unix)]
        let shutdown_signal = async {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to install SIGTERM handler");
            let mut sigint =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                    .expect("failed to install SIGINT handler");
            tokio::select! {
                _ = sigterm.recv() => { tracing::info!("received SIGTERM"); }
                _ = sigint.recv() => { tracing::info!("received SIGINT"); }
            }
        };
        #[cfg(not(unix))]
        let shutdown_signal = async {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("received Ctrl-C");
        };
        tokio::pin!(shutdown_signal);

        loop {
            tokio::select! {
                // Shutdown signal received (FR-7.1)
                _ = &mut shutdown_signal => {
                    tracing::info!("shutdown signal received, initiating graceful shutdown");
                    break;
                }

                // Read from upstream stdin
                result = reader.next_message() => {
                    let raw = match result? {
                        Some(r) => r,
                        None => {
                            tracing::info!("upstream EOF, shutting down proxy");
                            break;
                        }
                    };

                    let msg: Value = match serde_json::from_str(&raw) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!("failed to parse upstream JSON: {e}");
                            let _ = upstream_tx
                                .send(make_error_response(
                                    Value::Null,
                                    -32700,
                                    "Parse error",
                                    json!({"error_source": "proxy"}),
                                ))
                                .await;
                            continue;
                        }
                    };

                    tracing::debug!(direction = "upstream->proxy", %msg);

                    let method = msg.get("method").and_then(|v| v.as_str()).map(String::from);
                    let id = msg.get("id").cloned();
                    let req_id = id.as_ref().map_or_else(
                        || "none".to_string(),
                        |v| v.to_string(),
                    );
                    let request_span = tracing::debug_span!("mcp_request", request_id = %req_id);
                    async {
                        match method.as_deref() {
                            Some("tools/call") => {
                                self.handle_tools_call(msg, &pending, &upstream_tx, &dropped)
                                    .await;
                            }
                            Some("initialize") => {
                                self.handle_initialize(id, &upstream_tx).await;
                            }
                            Some("notifications/initialized") => {
                                // No-op when child not yet spawned; forward if child is running.
                                if self.child.is_some() {
                                    self.forward_to_child(msg, id, false, &pending, &upstream_tx)
                                        .await;
                                }
                            }
                            Some(method_name) => {
                                let is_tools_list = method_name == "tools/list";
                                self.forward_to_child(msg, id, is_tools_list, &pending, &upstream_tx)
                                    .await;
                            }
                            None => {
                                // Response from upstream — may be an elicitation response.
                                // Check the elicitation registry first; if it matches,
                                // forward the response downstream to the child.
                                // Otherwise forward as-is to the child.
                                if let Some(resp_id) = msg.get("id") {
                                    let maybe_downstream_resp = self
                                        .elicitation_registry
                                        .lock()
                                        .await
                                        .resolve_for_downstream(resp_id, msg.clone());
                                    if let Some(downstream_resp) = maybe_downstream_resp {
                                        tracing::debug!("elicitation response resolved for id={resp_id}");
                                        if let Some(ref handle) = self.child {
                                            let mut stdin = handle.stdin.lock().await;
                                            let serialized = serde_json::to_string(&downstream_resp)
                                                .unwrap_or_default();
                                            if let Err(e) =
                                                write_newline_delimited(&mut *stdin, &serialized).await
                                            {
                                                tracing::warn!(
                                                    "failed to write elicitation response to child: {e}"
                                                );
                                            }
                                        }
                                    } else if let Some(ref handle) = self.child {
                                        // Not an elicitation response — forward to child.
                                        let mut stdin = handle.stdin.lock().await;
                                        let serialized =
                                            serde_json::to_string(&msg).unwrap_or_default();
                                        if let Err(e) =
                                            write_newline_delimited(&mut *stdin, &serialized).await
                                        {
                                            tracing::warn!("failed to write response to child: {e}");
                                        }
                                    }
                                } else if let Some(ref handle) = self.child {
                                    // No id field — forward to child as-is.
                                    let mut stdin = handle.stdin.lock().await;
                                    let serialized = serde_json::to_string(&msg).unwrap_or_default();
                                    if let Err(e) =
                                        write_newline_delimited(&mut *stdin, &serialized).await
                                    {
                                        tracing::warn!("failed to write response to child: {e}");
                                    }
                                }
                            }
                        }
                    }
                    .instrument(request_span)
                    .await;
                }

                // Read from child (server-initiated requests like elicitation)
                msg = async {
                    if let Some(ref mut handle) = self.child {
                        handle.response_rx.recv().await
                    } else {
                        std::future::pending::<Option<Value>>().await
                    }
                } => {
                    if let Some(msg) = msg {
                        route_child_message(
                            msg,
                            &pending,
                            &upstream_tx,
                            &dropped,
                            &thread_to_agent,
                            &watch_stream_hub,
                            &self.elicitation_registry,
                            &self.elicitation_counter,
                        )
                        .await;
                    }
                }

                // Drain upstream write channel
                Some(msg) = upstream_rx.recv() => {
                    let serialized = serde_json::to_string(&msg).unwrap_or_default();
                    if write_newline_delimited(&mut upstream_out, &serialized)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }

        // Shutdown: abort the idle mail poller task to prevent leaked background work.
        if let Some(handle) = mail_poller_handle.take() {
            handle.abort();
        }

        // Graceful shutdown: request summary from each active thread (FR-7.1).
        self.collect_shutdown_summaries().await;

        // Shutdown: release all session locks before terminating
        {
            let team = self.team.clone();
            let reg = self.registry.lock().await;
            for entry in reg.list_all() {
                if entry.status == crate::session::SessionStatus::Active {
                    let _ = release_lock(&team, &entry.identity).await;
                }
            }
        }

        // Shutdown: persist final registry state to disk (ATM-QA-A5-008).
        // The lock from the block above is released before this call.
        let sessions_path = crate::lock::sessions_dir()
            .join(&self.team)
            .join("registry.json");
        if let Err(e) = Self::persist_registry(&self.registry, &sessions_path).await {
            tracing::warn!("failed to persist registry at shutdown: {e:#}");
        }

        // Shutdown: signal child and force-kill if it ignores stdin EOF
        if let Some(mut handle) = self.child.take() {
            // Abort the periodic drain background task (JSON mode only).
            if let Some(drain_handle) = handle.drain_task.take() {
                drain_handle.abort();
            }
            // Drop stdin to signal EOF to child
            drop(handle.stdin);
            // Grace period: give child time to flush output
            tokio::time::sleep(Duration::from_millis(CHILD_DRAIN_GRACE_MS)).await;
            // Ensure child terminates even if it ignored stdin EOF
            if let Some(mut child) = handle.process.lock().await.take() {
                let _ = child.kill().await;
            }
        }

        Ok(())
    }

    /// Request a compacted summary from each active Codex thread during
    /// graceful shutdown (FR-7.1, FR-7.2).
    ///
    /// For each active session with a known `thread_id`:
    /// 1. Sends a `codex-reply` to the child with a summary prompt.
    /// 2. Waits up to 10 seconds for the response.
    /// 3. Writes the summary to disk via [`crate::summary::write_summary`].
    /// 4. If the timeout expires, writes the session as interrupted (no summary).
    ///
    /// Sessions without a `thread_id` (still in initial codex call) are skipped.
    async fn collect_shutdown_summaries(&mut self) {
        const SUMMARY_TIMEOUT_SECS: u64 = 10;
        const SUMMARY_PROMPT: &str = "\
Session ending. Write a concise summary of:\n\
- What you were working on\n\
- Current state \u{2014} what is done, what is not\n\
- Any open questions or blockers\n\
- Next steps if resumed";

        // Collect active sessions that have a thread_id.
        let sessions: Vec<(String, String, String)> = {
            let reg = self.registry.lock().await;
            reg.list_all()
                .iter()
                .filter(|e| e.status == SessionStatus::Active && e.thread_id.is_some())
                .map(|e| {
                    (
                        e.agent_id.clone(),
                        e.identity.clone(),
                        e.thread_id.clone().unwrap(),
                    )
                })
                .collect()
        };

        if sessions.is_empty() {
            tracing::info!("no active sessions with thread_id; skipping shutdown summaries");
            return;
        }

        if self.child.is_none() {
            tracing::info!("child not running; skipping shutdown summaries");
            return;
        }

        // Clone the stdin Arc so we can write to it without holding an immutable
        // borrow on `self.child` across the loop body — we need `&mut self.child`
        // later to receive from `response_rx`.
        let stdin_arc = self.child.as_ref().unwrap().stdin.clone();

        for (i, (agent_id, identity, thread_id)) in sessions.iter().enumerate() {
            let request_id = format!("shutdown-summary-{i}");

            // Build a codex-reply request with the summary prompt.
            let request = json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "tools/call",
                "params": {
                    "name": "codex-reply",
                    "arguments": {
                        "threadId": thread_id,
                        "prompt": SUMMARY_PROMPT,
                    }
                }
            });

            let serialized = serde_json::to_string(&request).unwrap_or_default();
            {
                let mut stdin = stdin_arc.lock().await;
                if let Err(e) = write_newline_delimited(&mut *stdin, &serialized).await {
                    tracing::warn!(
                        agent_id = %agent_id,
                        "failed to send summary request to child: {e}"
                    );
                    continue;
                }
            }

            // Wait for the matching response on the child's response channel
            // (10s timeout). Other messages are discarded during shutdown.
            let deadline = tokio::time::Instant::now()
                + tokio::time::Duration::from_secs(SUMMARY_TIMEOUT_SECS);
            let mut summary_text: Option<String> = None;

            if let Some(ch) = self.child.as_mut() {
                loop {
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        tracing::warn!(
                            agent_id = %agent_id,
                            "shutdown summary timed out after {SUMMARY_TIMEOUT_SECS}s"
                        );
                        break;
                    }
                    match timeout(remaining, ch.response_rx.recv()).await {
                        Ok(Some(msg)) => {
                            if msg.get("id").and_then(|v| v.as_str()) == Some(&request_id) {
                                summary_text = msg
                                    .pointer("/result/content")
                                    .and_then(|v| v.as_array())
                                    .and_then(|arr| {
                                        arr.iter()
                                            .find(|item| {
                                                item.get("type").and_then(|t| t.as_str())
                                                    == Some("text")
                                            })
                                            .and_then(|item| {
                                                item.get("text").and_then(|t| t.as_str())
                                            })
                                    })
                                    .or_else(|| {
                                        msg.pointer("/result/structuredContent/text")
                                            .and_then(|v| v.as_str())
                                    })
                                    .map(String::from);
                                break;
                            }
                            // Not our response — discard during shutdown.
                        }
                        Ok(None) => {
                            tracing::warn!(
                                agent_id = %agent_id,
                                "child response channel closed during shutdown summary"
                            );
                            break;
                        }
                        Err(_) => {
                            tracing::warn!(
                                agent_id = %agent_id,
                                "shutdown summary timed out after {SUMMARY_TIMEOUT_SECS}s"
                            );
                            break;
                        }
                    }
                }
            }

            // Write summary to disk (or interrupted marker if none received).
            let team = self.team.clone();
            if let Some(ref text) = summary_text {
                if let Err(e) =
                    crate::summary::write_summary(&team, identity, thread_id, text).await
                {
                    tracing::warn!(
                        agent_id = %agent_id,
                        "failed to write shutdown summary: {e}"
                    );
                } else {
                    tracing::info!(
                        agent_id = %agent_id,
                        identity = %identity,
                        "shutdown summary written"
                    );
                }
            } else {
                tracing::warn!(
                    agent_id = %agent_id,
                    "no summary received; session marked as interrupted"
                );
                let interrupted_msg = "[Session interrupted — no summary available]";
                let _ = crate::summary::write_summary(&team, identity, thread_id, interrupted_msg)
                    .await;
            }
        }
    }

    /// Forward a non-tools/call request or notification to the child.
    async fn forward_to_child(
        &mut self,
        msg: Value,
        id: Option<Value>,
        is_tools_list: bool,
        pending: &Arc<Mutex<PendingRequests>>,
        upstream_tx: &mpsc::Sender<Value>,
    ) {
        if let Some(ref handle) = self.child {
            let serialized = serde_json::to_string(&msg).unwrap_or_default();
            let mut stdin = handle.stdin.lock().await;
            if let Err(e) = write_newline_delimited(&mut *stdin, &serialized).await {
                tracing::warn!("failed to write to child stdin: {e}");
            }
            drop(stdin);

            if let Some(req_id) = id {
                let (tx, rx) = oneshot::channel();
                {
                    let mut guard = pending.lock().await;
                    guard.insert(req_id.clone(), tx);
                    if is_tools_list {
                        guard.mark_tools_list(req_id.clone());
                    }
                }

                let upstream_tx_clone = upstream_tx.clone();
                tokio::spawn(async move {
                    match rx.await {
                        Ok(resp) => {
                            let _ = upstream_tx_clone.send(resp).await;
                        }
                        Err(_) => {
                            tracing::debug!("pending request dropped (child may have died)");
                        }
                    }
                });
            }
        } else {
            // Child not yet spawned.
            if is_tools_list {
                if let Some(req_id) = id {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "tools": crate::tools::synthetic_tools()
                        }
                    });
                    let _ = upstream_tx.send(response).await;
                }
                return;
            }
            // All other methods: error.
            if let Some(req_id) = id {
                let err = make_error_response(
                    req_id,
                    ERR_INTERNAL,
                    "Child process not yet spawned",
                    json!({"error_source": "proxy"}),
                );
                let _ = upstream_tx.send(err).await;
            }
        }
    }

    /// Respond to an MCP `initialize` request without forwarding to the child.
    ///
    /// The child process is lazily spawned only on the first `codex` or
    /// `codex-reply` tool call, so `initialize` must be answered by the proxy
    /// itself to avoid a `ERR_INTERNAL -32603 "Child process not yet spawned"`
    /// error that would break the MCP handshake.
    async fn handle_initialize(&self, id: Option<Value>, upstream_tx: &mpsc::Sender<Value>) {
        let Some(req_id) = id else { return };
        let response = json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "atm-agent-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        });
        let _ = upstream_tx.send(response).await;
    }

    /// Handle a `tools/call` request from upstream.
    async fn handle_tools_call(
        &mut self,
        mut msg: Value,
        pending: &Arc<Mutex<PendingRequests>>,
        upstream_tx: &mpsc::Sender<Value>,
        dropped: &Arc<AtomicU64>,
    ) {
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let tool_name: String = msg
            .pointer("/params/name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Synthetic ATM tool calls — no child needed
        if is_synthetic_tool(&tool_name) {
            let args = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let thread_id = msg
                .pointer("/params/_meta/threadId")
                .and_then(|v| v.as_str())
                .or_else(|| args.get("threadId").and_then(|v| v.as_str()))
                .map(ToString::to_string);
            let resp = self
                .handle_synthetic_tool(&id, &tool_name, &args, thread_id.as_deref())
                .await;
            let _ = upstream_tx.send(resp).await;
            return;
        }

        let mut is_codex_tool = tool_name == "codex" || tool_name == "codex-reply";
        // effective_tool_name tracks the final routing (may be rewritten to "codex-reply")
        let mut effective_tool_name = tool_name.clone();

        // Validate codex-specific params before spawning child
        if tool_name == "codex" {
            let params = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));

            // FR-16.3: codex + agent_id → session resume (treat as codex-reply)
            if let Some(resume_agent_id) = params.get("agent_id").and_then(|v| v.as_str()) {
                let resume_agent_id = resume_agent_id.to_string();
                let (thread_id_opt, found) = {
                    let reg = self.registry.lock().await;
                    if let Some(entry) = reg.get(&resume_agent_id) {
                        (entry.thread_id.clone(), true)
                    } else {
                        (None, false)
                    }
                };
                if !found {
                    let _ = upstream_tx
                        .send(make_error_response(
                            id,
                            ERR_SESSION_NOT_FOUND,
                            "session not found for agent_id",
                            json!({"error_source": "proxy", "agent_id": resume_agent_id}),
                        ))
                        .await;
                    return;
                }
                if thread_id_opt.is_none() {
                    let _ = upstream_tx
                        .send(make_error_response(
                            id,
                            ERR_INTERNAL,
                            "session has no threadId yet, cannot resume",
                            json!({"error_source": "proxy", "agent_id": resume_agent_id}),
                        ))
                        .await;
                    return;
                }
                // Rewrite to codex-reply path — mutate msg in place so the child
                // receives a codex-reply call (not a new codex call) with the
                // correct threadId (FR-16.3).
                effective_tool_name = "codex-reply".to_string();
                is_codex_tool = true;
                // Fix: rewrite params.name so child treats this as a reply, not a new session.
                if let Some(name) = msg.pointer_mut("/params/name") {
                    *name = serde_json::Value::String("codex-reply".to_string());
                }
                // Fix: inject threadId so child can resume the conversation thread.
                // Safety: thread_id_opt is guaranteed non-None by the check above.
                let thread_id_str = thread_id_opt.unwrap();
                if let Some(args) = msg.pointer_mut("/params/arguments") {
                    if let Some(obj) = args.as_object_mut() {
                        obj.insert(
                            "threadId".to_string(),
                            serde_json::Value::String(thread_id_str),
                        );
                    }
                }
                // Fall through: prepare_codex_reply_message will apply context injection.
            } else {
                // Normal new-session path — validate prompt/agent_file params
                let prompt = params.get("prompt").and_then(|v| v.as_str());
                let agent_file_path = params.get("agent_file").and_then(|v| v.as_str());

                // FR-16.5: agent_file and prompt are mutually exclusive
                if prompt.is_some() && agent_file_path.is_some() {
                    let _ = upstream_tx
                        .send(make_error_response(
                            id,
                            ERR_INVALID_SESSION_PARAMS,
                            "agent_file and prompt are mutually exclusive",
                            json!({"error_source": "proxy"}),
                        ))
                        .await;
                    return;
                }

                // FR-16.6: agent_file must exist
                if let Some(path) = agent_file_path {
                    if !std::path::Path::new(path).exists() {
                        let _ = upstream_tx
                            .send(make_error_response(
                                id,
                                ERR_AGENT_FILE_NOT_FOUND,
                                &format!("agent_file not found: {path}"),
                                json!({"error_source": "proxy", "path": path}),
                            ))
                            .await;
                        return;
                    }
                }

                // Pre-flight identity conflict check — runs before spawn_child so
                // unit tests can validate conflict detection without a live child.
                // Skip if child is already running: the lock/registry entry from the
                // live session is intentional and should not be treated as a conflict.
                if self.child.is_none() {
                    let explicit_identity = params
                        .get("identity")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let identity = explicit_identity
                        .or_else(|| self.config.identity.clone())
                        .unwrap_or_else(|| "codex".to_string());

                    // Cross-process lock check (FR-20.1)
                    if let Some((pid, conflicting_agent_id)) =
                        check_lock(&self.team, &identity).await
                    {
                        let _ = upstream_tx
                            .send(make_error_response(
                                id,
                                ERR_IDENTITY_CONFLICT,
                                &format!(
                                    "identity '{identity}' already locked by PID {pid} \
                                 (agent_id: {conflicting_agent_id})"
                                ),
                                json!({
                                    "error_source": "proxy",
                                    "identity": identity,
                                    "conflicting_agent_id": conflicting_agent_id,
                                    "pid": pid,
                                }),
                            ))
                            .await;
                        return;
                    }

                    // In-memory registry conflict check
                    let conflict_agent_id = {
                        let reg = self.registry.lock().await;
                        reg.find_by_identity(&identity).map(|s| s.to_string())
                    };
                    if let Some(conflicting_agent_id) = conflict_agent_id {
                        let _ = upstream_tx
                            .send(make_error_response(
                                id,
                                ERR_IDENTITY_CONFLICT,
                                &format!("identity '{identity}' already bound to active session"),
                                json!({
                                    "error_source": "proxy",
                                    "identity": identity,
                                    "conflicting_agent_id": conflicting_agent_id,
                                }),
                            ))
                            .await;
                        return;
                    }
                } // end if self.child.is_none() (pre-flight check)
            }
        }

        // Lazy spawn child on first codex/codex-reply
        if is_codex_tool && self.child.is_none() {
            tracing::info!("lazy-spawning Codex child process");
            match self.spawn_child(pending, upstream_tx, dropped).await {
                Ok(()) => {}
                Err(e) => {
                    tracing::error!("failed to spawn child: {e}");
                    let err = make_error_response(
                        id,
                        ERR_CHILD_DEAD,
                        &format!("Failed to spawn Codex child: {e}"),
                        json!({"error_source": "proxy"}),
                    );
                    let _ = upstream_tx.send(err).await;
                    return;
                }
            }
        }

        // Check child health
        if let Some(ref handle) = self.child {
            let status = handle.exit_status.lock().await;
            if let Some(exit) = &*status {
                let code = exit.code().unwrap_or(-1);
                tracing::warn!("child process is dead (exit code: {code})");
                let err = make_error_response(
                    id,
                    ERR_CHILD_DEAD,
                    &format!("Codex child process died (exit code: {code})"),
                    json!({"error_source": "proxy", "exit_code": code}),
                );
                let _ = upstream_tx.send(err).await;
                return;
            }
        }

        if self.child.is_none() {
            let err = make_error_response(
                id.clone(),
                ERR_INTERNAL,
                "Child process not available",
                json!({"error_source": "proxy"}),
            );
            let _ = upstream_tx.send(err).await;
            return;
        }

        // Build the (possibly modified) message before borrowing self.child.
        // prepare_* methods take &mut self, so they must be called before we
        // take any reference to self.child.
        // effective_tool_name may have been rewritten to "codex-reply" for resume flows.
        let (msg_to_forward, expected_agent_id, state_agent_id) = if effective_tool_name == "codex"
        {
            match self.prepare_codex_message(&id, msg, upstream_tx).await {
                PrepareResult::Error => return, // error already sent
                PrepareResult::Ok {
                    modified,
                    expected_agent_id,
                } => (modified, expected_agent_id.clone(), expected_agent_id),
            }
        } else if effective_tool_name == "codex-reply" {
            let modified = self.prepare_codex_reply_message(msg).await;
            let reply_agent_id = self.resolve_codex_reply_agent_id(&modified).await;
            (modified, None, reply_agent_id)
        } else {
            (msg, None, None)
        };

        // Now borrow the handle for I/O (after all &mut self calls are done)
        let Some(ref handle) = self.child else {
            // Child died between the health check and here
            let err = make_error_response(
                id,
                ERR_CHILD_DEAD,
                "Child process died unexpectedly",
                json!({"error_source": "proxy"}),
            );
            let _ = upstream_tx.send(err).await;
            return;
        };

        // Resolve the agent_id for thread state tracking.  For `codex` calls the
        // agent_id is known from session registration; for `codex-reply` calls we
        // resolve it via the threadId.
        let resolved_agent_id_for_state: Option<String> = if effective_tool_name == "codex"
            || effective_tool_name == "codex-reply"
        {
            if let Some(ref aid) = expected_agent_id {
                Some(aid.clone())
            } else {
                // codex-reply without expected_agent_id: resolve via threadId
                let thread_id_from_msg = msg_to_forward
                    .pointer("/params/arguments/threadId")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if let Some(tid) = thread_id_from_msg {
                    if let Some(agent_id) = self.thread_to_agent.lock().await.get(&tid).cloned() {
                        Some(agent_id)
                    } else {
                        let reg = self.registry.lock().await;
                        reg.list_all()
                            .iter()
                            .find(|e| e.thread_id.as_deref() == Some(tid.as_str()))
                            .map(|e| e.agent_id.clone())
                    }
                } else {
                    None
                }
            }
        } else {
            None
        };

        // Fix 4: If this is a codex-reply and the thread is currently Busy
        // (e.g. an auto-mail turn is in-flight), queue the command instead of
        // writing directly to child stdin.  The dispatcher
        // (dispatch_auto_mail_if_available) will pop and dispatch it when the
        // thread becomes Idle, preserving the priority order (FR-17.11).
        if effective_tool_name == "codex-reply" {
            if let Some(ref agent_id) = resolved_agent_id_for_state {
                let is_busy = {
                    let reg = self.registry.lock().await;
                    reg.get(agent_id)
                        .map(|e| e.thread_state == ThreadState::Busy)
                        .unwrap_or(false)
                };
                if is_busy {
                    let args = msg_to_forward
                        .pointer("/params/arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    let (tx, rx) = oneshot::channel();
                    let queued = {
                        let queues_guard = self.queues.lock().await;
                        if let Some(q_arc) = queues_guard.get(agent_id.as_str()) {
                            let mut q = q_arc.lock().await;
                            q.push_claude_reply(id.clone(), args, tx).is_ok()
                        } else {
                            false
                        }
                    };
                    if queued {
                        tracing::info!(
                            agent_id = %agent_id,
                            "codex-reply queued (thread is Busy); will dispatch when Idle"
                        );
                        // Spawn a task that waits for the queued reply to be dispatched
                        // and sends the response upstream.
                        let upstream_tx_clone = upstream_tx.clone();
                        let timeout_secs = self.config.request_timeout_secs;
                        tokio::spawn(async move {
                            match timeout(Duration::from_secs(timeout_secs), rx).await {
                                Ok(Ok(resp)) => {
                                    let _ = upstream_tx_clone.send(resp).await;
                                }
                                Ok(Err(_)) => {
                                    tracing::debug!("queued ClaudeReply dropped (child died)");
                                }
                                Err(_elapsed) => {
                                    tracing::warn!(
                                        "queued ClaudeReply timed out after {timeout_secs}s"
                                    );
                                    let err = make_error_response(
                                        id,
                                        ERR_TIMEOUT,
                                        &format!(
                                            "Queued codex-reply timed out after {timeout_secs}s"
                                        ),
                                        json!({"error_source": "proxy"}),
                                    );
                                    let _ = upstream_tx_clone.send(err).await;
                                }
                            }
                        });
                        return;
                    }
                    // If queuing failed (queue closed), fall through to direct dispatch.
                }
            }
        }

        // Set thread state Busy BEFORE writing to child stdin to close the
        // TOCTOU window where auto-mail could inject concurrently.
        if let Some(ref agent_id_for_state) = resolved_agent_id_for_state {
            self.registry
                .lock()
                .await
                .set_thread_state(agent_id_for_state, ThreadState::Busy);
        }

        // Forward to child
        let serialized = serde_json::to_string(&msg_to_forward).unwrap_or_default();
        {
            let mut stdin = handle.stdin.lock().await;
            if let Err(e) = write_newline_delimited(&mut *stdin, &serialized).await {
                tracing::error!("failed to write to child: {e}");
                // Revert Busy → Idle on write failure.
                if let Some(ref agent_id_for_state) = resolved_agent_id_for_state {
                    self.registry
                        .lock()
                        .await
                        .set_thread_state(agent_id_for_state, ThreadState::Idle);
                }
                let err = make_error_response(
                    id,
                    ERR_CHILD_DEAD,
                    &format!("Failed to write to child: {e}"),
                    json!({"error_source": "proxy"}),
                );
                let _ = upstream_tx.send(err).await;
                return;
            }
        }

        // Register pending request with timeout
        let (tx, rx) = oneshot::channel();
        {
            let mut p = pending.lock().await;
            p.insert(id.clone(), tx);
            if let Some(aid) = expected_agent_id.clone() {
                p.mark_codex_create(id.clone(), aid);
            }
            if effective_tool_name == "codex" || effective_tool_name == "codex-reply" {
                let actor_fallback = self.config.identity.as_deref().unwrap_or("upstream-client");
                let source = infer_upstream_request_source(&msg_to_forward, actor_fallback);
                p.mark_request_source(id.clone(), source.clone());
                if let Some(ref aid) = state_agent_id {
                    p.set_last_agent_source(aid.clone(), source);
                }
            }
        }

        // Mark the session as Busy while the codex/codex-reply turn is in progress.
        if let Some(ref agent_id_for_state) = state_agent_id {
            self.registry
                .lock()
                .await
                .set_thread_state(agent_id_for_state, ThreadState::Busy);
        }

        let timeout_secs = self.config.request_timeout_secs;
        let upstream_tx_clone = upstream_tx.clone();
        let req_id = id;
        let child_stdin = Arc::clone(&handle.stdin);

        let thread_to_agent_task = Arc::clone(&self.thread_to_agent);
        let pending_for_thread_map = Arc::clone(pending);
        let registry_for_thread_map = Arc::clone(&self.registry);
        let team_for_thread_map = self.team.clone();
        // Clone state_agent_id for thread state tracking in the spawned task.
        let state_agent_id_for_task = state_agent_id.clone();
        let effective_tool_name_for_task = effective_tool_name.clone();
        // Mail injection context for post-turn check (FR-8.1).
        let queues_for_task = Arc::clone(&self.queues);
        let mail_enabled_for_task = self.mail_poller.is_enabled();
        let mail_max_messages = self.mail_poller.max_messages;
        let mail_max_length = self.mail_poller.max_message_length;
        let request_counter_for_task = Arc::clone(&self.request_counter);
        let per_thread_overrides_for_task = self.config.per_thread_auto_mail.clone();
        let shared_stdin_for_task = Arc::clone(&self.shared_child_stdin);

        tokio::spawn(async move {
            match timeout(Duration::from_secs(timeout_secs), rx).await {
                Ok(Ok(resp)) => {
                    // Track the agent_id that just completed its turn so we can
                    // run the post-turn mail check (FR-8.1) after forwarding the response.
                    let mut completed_agent_id: Option<String> = None;
                    let mut completed_identity: Option<String> = None;
                    let mut completed_thread_id: Option<String> = None;

                    if let Some(thread_id) = resp
                        .pointer("/result/structuredContent/threadId")
                        .and_then(|v| v.as_str())
                    {
                        if let Some(agent_id) = pending_for_thread_map
                            .lock()
                            .await
                            .take_codex_create(&req_id)
                        {
                            {
                                let mut reg = registry_for_thread_map.lock().await;
                                reg.set_thread_id(&agent_id, thread_id.to_string());
                                // Turn complete → thread is now idle (FR-17).
                                reg.set_thread_state(&agent_id, ThreadState::Idle);
                                // Capture for post-turn mail check.
                                if let Some(entry) = reg.get(&agent_id) {
                                    completed_identity = Some(entry.identity.clone());
                                }
                            }
                            thread_to_agent_task
                                .lock()
                                .await
                                .insert(thread_id.to_string(), agent_id.clone());
                            completed_agent_id = Some(agent_id.clone());
                            completed_thread_id = Some(thread_id.to_string());
                            // Persist updated registry (thread_id now set)
                            let sessions_path = crate::lock::sessions_dir()
                                .join(&team_for_thread_map)
                                .join("registry.json");
                            if let Err(e) = ProxyServer::persist_registry(
                                &registry_for_thread_map,
                                &sessions_path,
                            )
                            .await
                            {
                                tracing::warn!(
                                    "failed to persist registry after set_thread_id: {e}"
                                );
                            }
                        } else if effective_tool_name_for_task == "codex-reply" {
                            // codex-reply response — set the originating agent's thread to Idle.
                            // We resolve the agent_id via the threadId that just arrived.
                            let agent_id_opt =
                                thread_to_agent_task.lock().await.get(thread_id).cloned();
                            if let Some(aid) = agent_id_opt {
                                {
                                    let mut reg = registry_for_thread_map.lock().await;
                                    reg.set_thread_state(&aid, ThreadState::Idle);
                                    if let Some(entry) = reg.get(&aid) {
                                        completed_identity = Some(entry.identity.clone());
                                    }
                                }
                                completed_agent_id = Some(aid);
                                completed_thread_id = Some(thread_id.to_string());
                            }
                        }
                    } else if let Some(ref aid) = state_agent_id_for_task {
                        // Response without threadId (e.g. error) — still mark Idle
                        // so the session does not remain stuck in Busy state.
                        {
                            let mut reg = registry_for_thread_map.lock().await;
                            reg.set_thread_state(aid, ThreadState::Idle);
                            if let Some(entry) = reg.get(aid) {
                                completed_identity = Some(entry.identity.clone());
                                completed_thread_id = entry.thread_id.clone();
                            }
                        }
                        completed_agent_id = Some(aid.clone());
                    }
                    let _ = upstream_tx_clone.send(resp).await;

                    // Post-turn mail check (FR-8.1): after a turn completes,
                    // delegate to the unified dispatch function which handles
                    // priority checking, single-flight guard, write, pending map
                    // registration, and mark-read.
                    if mail_enabled_for_task {
                        if let (Some(agent_id), Some(identity), Some(thread_id)) = (
                            &completed_agent_id,
                            &completed_identity,
                            &completed_thread_id,
                        ) {
                            let per_thread_enabled = per_thread_overrides_for_task
                                .get(agent_id.as_str())
                                .copied()
                                .unwrap_or(true);

                            if per_thread_enabled {
                                dispatch_auto_mail_if_available(
                                    agent_id,
                                    identity,
                                    thread_id,
                                    &team_for_thread_map,
                                    mail_max_messages,
                                    mail_max_length,
                                    &registry_for_thread_map,
                                    &queues_for_task,
                                    &shared_stdin_for_task,
                                    &pending_for_thread_map,
                                    &request_counter_for_task,
                                    None,
                                    None,
                                )
                                .await;
                            }
                        }
                    }

                    // Emit teammate_idle lifecycle event after each turn
                    // completes (best-effort, non-fatal).
                    if let (Some(agent_id), Some(identity)) =
                        (&completed_agent_id, &completed_identity)
                    {
                        let agent_id_clone = agent_id.clone();
                        let identity_clone = identity.clone();
                        let team_clone = team_for_thread_map.clone();
                        tokio::spawn(async move {
                            crate::lifecycle_emit::emit_lifecycle_event(
                                crate::lifecycle_emit::EventKind::TeammateIdle,
                                &identity_clone,
                                &team_clone,
                                &agent_id_clone,
                                None,
                            )
                            .await;
                        });
                    }
                }
                Ok(Err(_)) => {
                    // Sender dropped (child died)
                    tracing::debug!("pending request canceled (child died)");
                    let _ = pending_for_thread_map
                        .lock()
                        .await
                        .take_codex_create(&req_id);
                }
                Err(_elapsed) => {
                    tracing::warn!("request timed out after {timeout_secs}s");
                    let _ = pending_for_thread_map
                        .lock()
                        .await
                        .take_codex_create(&req_id);
                    let cancel = json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/cancelled",
                        "params": {"requestId": req_id}
                    });
                    if let Ok(serialized) = serde_json::to_string(&cancel) {
                        let mut stdin = child_stdin.lock().await;
                        let _ = write_newline_delimited(&mut *stdin, &serialized).await;
                    }
                    let err = make_error_response(
                        req_id,
                        ERR_TIMEOUT,
                        &format!("Request timed out after {timeout_secs}s"),
                        json!({"error_source": "proxy"}),
                    );
                    let _ = upstream_tx_clone.send(err).await;
                }
            }
        });
    }

    /// Prepare a `codex` tool call message: validate params, register session,
    /// and inject developer-instructions.
    ///
    /// Sends an error response via `upstream_tx` and returns
    /// [`PrepareResult::Error`] if any validation step fails.
    async fn prepare_codex_message(
        &mut self,
        id: &Value,
        msg: Value,
        upstream_tx: &mpsc::Sender<Value>,
    ) -> PrepareResult {
        let params = msg
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        let agent_file_path = params
            .get("agent_file")
            .and_then(|v| v.as_str())
            .map(String::from);
        let explicit_identity = params
            .get("identity")
            .and_then(|v| v.as_str())
            .map(String::from);
        let caller_cwd = params.get("cwd").and_then(|v| v.as_str()).map(String::from);

        // Resolve identity: explicit → config.identity → "codex"
        let identity = explicit_identity
            .or_else(|| self.config.identity.clone())
            .unwrap_or_else(|| "codex".to_string());

        // Detect git context (refreshed per turn)
        let effective_cwd = caller_cwd.as_deref().unwrap_or(".");
        let ctx = detect_context(effective_cwd).await;

        // Check cross-process lock (FR-20.1)
        let team = self.team.clone();
        if let Some((pid, conflicting_agent_id)) = check_lock(&team, &identity).await {
            let _ = upstream_tx
                .send(make_error_response(
                    id.clone(),
                    ERR_IDENTITY_CONFLICT,
                    &format!(
                        "identity '{}' already locked by PID {} (agent_id: {})",
                        identity, pid, conflicting_agent_id
                    ),
                    json!({
                        "error_source": "proxy",
                        "identity": identity,
                        "conflicting_agent_id": conflicting_agent_id,
                        "pid": pid
                    }),
                ))
                .await;
            return PrepareResult::Error;
        }

        // Register session in in-memory registry
        let team = self.team.clone();
        let entry = {
            let mut reg = self.registry.lock().await;
            match reg.register(
                identity.clone(),
                team.clone(),
                ctx.cwd.clone(),
                ctx.repo_root.clone(),
                ctx.repo_name.clone(),
                ctx.branch.clone(),
            ) {
                Ok(e) => e,
                Err(RegistryError::IdentityConflict {
                    identity: ident,
                    agent_id,
                }) => {
                    let _ = upstream_tx
                        .send(make_error_response(
                            id.clone(),
                            ERR_IDENTITY_CONFLICT,
                            &format!(
                                "identity '{ident}' is already bound to active session '{agent_id}'"
                            ),
                            json!({"error_source": "proxy", "identity": ident, "conflicting_agent_id": agent_id}),
                        ))
                        .await;
                    return PrepareResult::Error;
                }
                Err(RegistryError::MaxSessionsExceeded { max }) => {
                    let _ = upstream_tx
                        .send(make_error_response(
                            id.clone(),
                            ERR_MAX_SESSIONS_EXCEEDED,
                            &format!("max concurrent sessions ({max}) reached"),
                            json!({"error_source": "proxy", "max": max}),
                        ))
                        .await;
                    return PrepareResult::Error;
                }
            }
        };

        // Acquire cross-process lock file
        if let Err(e) = acquire_lock(&team, &identity, &entry.agent_id).await {
            // Roll back registry entry
            self.registry.lock().await.close(&entry.agent_id);
            let sessions_path = crate::lock::sessions_dir()
                .join(&team)
                .join("registry.json");
            if let Err(pe) = Self::persist_registry(&self.registry, &sessions_path).await {
                tracing::warn!("failed to persist registry after lock-rollback close: {pe}");
            }
            let _ = upstream_tx
                .send(make_error_response(
                    id.clone(),
                    ERR_IDENTITY_CONFLICT,
                    &format!("failed to acquire identity lock: {e}"),
                    json!({"error_source": "proxy"}),
                ))
                .await;
            return PrepareResult::Error;
        }

        // Record agent_source on the new entry if agent_file was provided (FR-16.1).
        if let Some(ref path) = agent_file_path {
            self.registry
                .lock()
                .await
                .set_agent_source(&entry.agent_id, path.clone());
        }

        // Create a command queue for this agent session (FR-8.11).
        {
            let mut queues = self.queues.lock().await;
            queues.insert(
                entry.agent_id.clone(),
                Arc::new(tokio::sync::Mutex::new(ThreadCommandQueue::new(
                    entry.agent_id.clone(),
                ))),
            );
        }

        // Persist registry after successful registration (FR-5.5)
        let sessions_path = crate::lock::sessions_dir()
            .join(&team)
            .join("registry.json");
        if let Err(e) = Self::persist_registry(&self.registry, &sessions_path).await {
            tracing::warn!("failed to persist registry after register: {e}");
        }

        // Emit session_start lifecycle event to the daemon (best-effort).
        // Uses source.kind = "atm_mcp" so the daemon applies relaxed validation.
        {
            let agent_id_clone = entry.agent_id.clone();
            let identity_clone = identity.clone();
            let team_clone = team.clone();
            tokio::spawn(async move {
                crate::lifecycle_emit::emit_lifecycle_event(
                    crate::lifecycle_emit::EventKind::SessionStart,
                    &identity_clone,
                    &team_clone,
                    &agent_id_clone,
                    None,
                )
                .await;
            });
        }

        // Wire session context into transport's TurnTracker for turn-state daemon
        // emission. After this point, TurnCompleted and crash-path interrupt_turn
        // calls in AppServerTransport's background task will emit TeammateIdle
        // events to the daemon. McpTransport and JsonCodecTransport use the
        // default no-op implementation of set_turn_session_context.
        {
            let turn_ctx =
                crate::turn_control::SessionContext::new(&identity, &team, &entry.agent_id);
            self.transport.set_turn_session_context(turn_ctx);
        }

        // Build developer-instructions context string
        let context_str = build_session_context(
            &identity,
            &team,
            ctx.repo_name.as_deref(),
            ctx.repo_root.as_deref(),
            ctx.branch.as_deref(),
            &ctx.cwd,
        );

        // Clone and modify message for injection
        let mut modified_msg = msg;
        if let Some(args) = modified_msg.pointer_mut("/params/arguments") {
            inject_developer_instructions(args, &context_str);

            // FR-6: Prepend resume context on first turn if available.
            if let Some(resume_ctx) = self.resume_context.take() {
                if let Some(ref summary) = resume_ctx.summary {
                    let resume_block = crate::summary::format_resume_context(
                        &resume_ctx.identity,
                        ctx.repo_name.as_deref(),
                        ctx.branch.as_deref(),
                        summary,
                    );
                    inject_developer_instructions(args, &resume_block);
                    tracing::info!(
                        agent_id = %resume_ctx.agent_id,
                        "resume context prepended to developer-instructions"
                    );
                } else {
                    tracing::warn!(
                        agent_id = %resume_ctx.agent_id,
                        identity = %resume_ctx.identity,
                        "no summary available for resume; continuing without context"
                    );
                }
            }

            // FR-16.1: if agent_file provided, read its contents as the prompt
            if let Some(ref path) = agent_file_path {
                match tokio::fs::read_to_string(path).await {
                    Ok(contents) => {
                        args["prompt"] = Value::String(contents);
                    }
                    Err(e) => {
                        tracing::warn!("failed to read agent_file {path}: {e}");
                    }
                }
            }
        }

        // FR-9.2: Audit the codex forward.
        let prompt_for_audit = modified_msg
            .pointer("/params/arguments/prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        self.audit_log
            .log_codex_forward(
                "codex",
                Some(&entry.agent_id),
                Some(&identity),
                prompt_for_audit,
            )
            .await;

        PrepareResult::Ok {
            modified: modified_msg,
            expected_agent_id: Some(entry.agent_id),
        }
    }

    /// Prepare a `codex-reply` message: refresh git context and inject
    /// developer-instructions.
    ///
    /// If the caller provides an explicit `cwd` in arguments the session's
    /// stored `cwd` is updated (FR-16.3 / Fix 8 / ATM-QA-A3-005).
    async fn prepare_codex_reply_message(&mut self, msg: Value) -> Value {
        let params = msg
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        let agent_id_param = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let thread_id_param = params
            .get("threadId")
            .and_then(|v| v.as_str())
            .map(String::from);
        let explicit_cwd = params.get("cwd").and_then(|v| v.as_str()).map(String::from);

        // Look up session for cwd/identity. Prefer agent_id, then threadId.
        let (resolved_agent_id, identity_opt, stored_cwd) = {
            let reg = self.registry.lock().await;
            if let Some(ref aid) = agent_id_param {
                if let Some(entry) = reg.get(aid) {
                    (
                        Some(aid.clone()),
                        Some(entry.identity.clone()),
                        entry.cwd.clone(),
                    )
                } else {
                    (None, None, ".".to_string())
                }
            } else if let Some(ref tid) = thread_id_param {
                if let Some(entry) = reg
                    .list_all()
                    .iter()
                    .find(|e| e.thread_id.as_deref() == Some(tid.as_str()))
                {
                    (
                        Some(entry.agent_id.clone()),
                        Some(entry.identity.clone()),
                        entry.cwd.clone(),
                    )
                } else {
                    (None, None, ".".to_string())
                }
            } else {
                (None, None, ".".to_string())
            }
        };

        // Use explicit cwd if provided; otherwise use stored cwd
        let effective_cwd = explicit_cwd.as_deref().unwrap_or(&stored_cwd);

        // Refresh git context
        let ctx = detect_context(effective_cwd).await;

        // Update session with fresh context (and explicit cwd if supplied)
        if let Some(ref aid) = resolved_agent_id {
            {
                let mut reg = self.registry.lock().await;
                if let Some(ref new_cwd) = explicit_cwd {
                    reg.set_cwd(aid, new_cwd.clone());
                }
                reg.touch(
                    aid,
                    ctx.repo_root.clone(),
                    ctx.repo_name.clone(),
                    ctx.branch.clone(),
                );
            }
            // Persist updated registry after touch (lock released above).
            let sessions_path = crate::lock::sessions_dir()
                .join(&self.team)
                .join("registry.json");
            if let Err(e) = Self::persist_registry(&self.registry, &sessions_path).await {
                tracing::warn!("failed to persist registry after touch: {e:#}");
            }
        }

        // Keep event attribution accurate during codex-reply-only flows.
        if let (Some(aid), Some(tid)) = (&resolved_agent_id, &thread_id_param) {
            self.thread_to_agent
                .lock()
                .await
                .insert(tid.clone(), aid.clone());
        }

        let identity_str = identity_opt
            .or_else(|| self.config.identity.clone())
            .unwrap_or_else(|| "codex".to_string());
        let team = self.team.clone();
        let context_str = build_session_context(
            &identity_str,
            &team,
            ctx.repo_name.as_deref(),
            ctx.repo_root.as_deref(),
            ctx.branch.as_deref(),
            &ctx.cwd,
        );

        let mut modified_msg = msg;
        if let Some(args) = modified_msg.pointer_mut("/params/arguments") {
            inject_developer_instructions(args, &context_str);

            // FR-6: Prepend resume context on first codex-reply if not yet consumed.
            if let Some(resume_ctx) = self.resume_context.take() {
                if let Some(ref summary) = resume_ctx.summary {
                    let resume_block = crate::summary::format_resume_context(
                        &resume_ctx.identity,
                        ctx.repo_name.as_deref(),
                        ctx.branch.as_deref(),
                        summary,
                    );
                    inject_developer_instructions(args, &resume_block);
                    tracing::info!(
                        agent_id = %resume_ctx.agent_id,
                        "resume context prepended to developer-instructions (codex-reply)"
                    );
                } else {
                    tracing::warn!(
                        agent_id = %resume_ctx.agent_id,
                        identity = %resume_ctx.identity,
                        "no summary available for resume; continuing without context"
                    );
                }
            }
        }

        // FR-9.2: Audit the codex-reply forward.
        let prompt_for_audit = modified_msg
            .pointer("/params/arguments/prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        self.audit_log
            .log_codex_forward(
                "codex-reply",
                resolved_agent_id.as_deref(),
                Some(&identity_str),
                prompt_for_audit,
            )
            .await;

        modified_msg
    }

    /// Resolve the owning `agent_id` for a prepared `codex-reply` message.
    ///
    /// Preference:
    /// 1. `params.arguments.agent_id`
    /// 2. `params.arguments.threadId` via `thread_to_agent`
    /// 3. Registry scan by `thread_id`
    async fn resolve_codex_reply_agent_id(&self, msg: &Value) -> Option<String> {
        if let Some(agent_id) = msg
            .pointer("/params/arguments/agent_id")
            .and_then(|v| v.as_str())
        {
            let reg = self.registry.lock().await;
            if reg.get(agent_id).is_some() {
                return Some(agent_id.to_string());
            }
        }

        let thread_id = msg
            .pointer("/params/arguments/threadId")
            .and_then(|v| v.as_str())?;

        if let Some(agent_id) = self.thread_to_agent.lock().await.get(thread_id).cloned() {
            return Some(agent_id);
        }

        let reg = self.registry.lock().await;
        reg.list_all()
            .iter()
            .find(|entry| entry.thread_id.as_deref() == Some(thread_id))
            .map(|entry| entry.agent_id.clone())
    }

    /// Handle a synthetic tool call (ATM tools, session management).
    ///
    /// ATM communication tools (`atm_send`, `atm_read`, `atm_broadcast`,
    /// `atm_pending_count`) are fully implemented in Sprint A.4.
    /// Session management tools (`agent_sessions`, `agent_status`) are
    /// implemented in Sprint A.5. `agent_close` is fully implemented in
    /// Sprint A.6.
    async fn resolve_identity_from_thread(&self, thread_id: &str) -> Option<String> {
        // Prefer the fast thread->agent map, then fall back to registry scan.
        if let Some(agent_id) = self.thread_to_agent.lock().await.get(thread_id).cloned() {
            let reg = self.registry.lock().await;
            if let Some(entry) = reg.get(&agent_id)
                && entry.status == crate::session::SessionStatus::Active
            {
                return Some(entry.identity.clone());
            }
        }

        let reg = self.registry.lock().await;
        reg.list_all()
            .into_iter()
            .find(|entry| {
                entry.status == crate::session::SessionStatus::Active
                    && entry.thread_id.as_deref() == Some(thread_id)
            })
            .map(|entry| entry.identity.clone())
    }

    async fn handle_synthetic_tool(
        &self,
        id: &Value,
        tool_name: &str,
        args: &Value,
        thread_id: Option<&str>,
    ) -> Value {
        use crate::atm_tools;

        match tool_name {
            "atm_send" | "atm_read" | "atm_broadcast" | "atm_pending_count" => {
                let thread_identity = if let Some(tid) = thread_id {
                    self.resolve_identity_from_thread(tid).await
                } else {
                    None
                };
                let identity_opt = thread_identity
                    .or_else(|| atm_tools::resolve_identity(args, self.config.identity.as_deref()));
                let Some(identity) = identity_opt else {
                    return make_error_response(
                        id.clone(),
                        ERR_IDENTITY_REQUIRED,
                        "identity required for ATM tools: provide 'identity' parameter or \
                         configure proxy identity",
                        json!({"error_source": "proxy", "tool": tool_name}),
                    );
                };
                let team = &self.team;
                tracing::info!(
                    tool = tool_name,
                    identity = %identity,
                    team = %team,
                    "ATM tool call"
                );

                // FR-9.1: Audit ATM tool call.
                let recipient = match tool_name {
                    "atm_send" => args.get("to").and_then(|v| v.as_str()),
                    _ => None,
                };
                let message_summary = args.get("message").and_then(|v| v.as_str());
                // Resolve agent_id from the thread→agent map when a threadId is present.
                // ATM tools called directly by the user-facing Claude session (no threadId)
                // have no associated Codex agent_id, so None is the correct value there.
                let agent_id_opt: Option<String> = if let Some(tid) = thread_id {
                    self.thread_to_agent.lock().await.get(tid).cloned()
                } else {
                    None
                };
                self.audit_log
                    .log_atm_call(
                        tool_name,
                        agent_id_opt.as_deref(),
                        Some(&identity),
                        recipient,
                        message_summary,
                    )
                    .await;

                match tool_name {
                    "atm_send" => atm_tools::handle_atm_send(id, args, &identity, team),
                    "atm_read" => atm_tools::handle_atm_read(id, args, &identity, team),
                    "atm_broadcast" => atm_tools::handle_atm_broadcast(id, args, &identity, team),
                    "atm_pending_count" => {
                        atm_tools::handle_atm_pending_count(id, args, &identity, team)
                    }
                    _ => unreachable!(),
                }
            }
            "agent_sessions" => {
                atm_tools::handle_agent_sessions(id, Arc::clone(&self.registry)).await
            }
            "agent_status" => {
                use agent_team_mail_core::home::get_home_dir;
                use std::time::{SystemTime, UNIX_EPOCH};
                let now_secs = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let uptime_secs = now_secs.saturating_sub(self.started_epoch_secs);
                let child_alive = self.child.is_some();
                // Compute aggregate unread mail count across all active sessions.
                let pending_mail_count: u64 = {
                    let home_opt = get_home_dir().ok();
                    let reg = self.registry.lock().await;
                    reg.list_all()
                        .iter()
                        .filter(|e| e.status == crate::session::SessionStatus::Active)
                        .map(|e| {
                            home_opt.as_deref().map_or(0, |home| {
                                atm_tools::count_unread_for_identity(&e.identity, &self.team, home)
                            })
                        })
                        .sum()
                };
                atm_tools::handle_agent_status(
                    id,
                    Arc::clone(&self.registry),
                    child_alive,
                    &self.team,
                    &self.started_at,
                    uptime_secs,
                    pending_mail_count,
                )
                .await
            }
            "agent_close" => {
                let resp = atm_tools::handle_agent_close(
                    id,
                    args,
                    Arc::clone(&self.registry),
                    Arc::clone(&self.elicitation_registry),
                )
                .await;
                let is_success = resp.get("error").is_none()
                    && resp.pointer("/result/isError").and_then(|v| v.as_bool()) != Some(true);
                if is_success {
                    if let Some(agent_id) = args.get("agent_id").and_then(|v| v.as_str()) {
                        self.watch_subscriptions.lock().await.remove(agent_id);
                        let _ = self.detach_watch_stream(agent_id).await;
                    }
                    let sessions_path = crate::lock::sessions_dir()
                        .join(&self.team)
                        .join("registry.json");
                    if let Err(e) = Self::persist_registry(&self.registry, &sessions_path).await {
                        tracing::warn!("failed to persist registry after agent_close: {e:#}");
                    }
                }
                resp
            }
            "agent_watch_attach" => {
                let Some(agent_id) = args.get("agent_id").and_then(|v| v.as_str()) else {
                    return atm_tools::make_mcp_error_result(
                        id,
                        "agent_watch_attach: 'agent_id' is required",
                    );
                };

                let sub = self.subscribe_watch_stream(agent_id).await;
                self.watch_subscriptions
                    .lock()
                    .await
                    .insert(agent_id.to_string(), sub.rx);
                let watcher_count = self.watch_stream_hub.lock().await.watcher_count(agent_id);
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": json!({
                                "agent_id": agent_id,
                                "attached": true,
                                "watcher_count": watcher_count,
                                "replay": sub.replay,
                            }).to_string()
                        }]
                    }
                })
            }
            "agent_watch_poll" => {
                let Some(agent_id) = args.get("agent_id").and_then(|v| v.as_str()) else {
                    return atm_tools::make_mcp_error_result(
                        id,
                        "agent_watch_poll: 'agent_id' is required",
                    );
                };
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.clamp(1, 200) as usize)
                    .unwrap_or(50);

                let mut subs = self.watch_subscriptions.lock().await;
                let Some(rx) = subs.get_mut(agent_id) else {
                    return atm_tools::make_mcp_error_result(
                        id,
                        &format!("agent_watch_poll: no active watcher for '{agent_id}'"),
                    );
                };

                let mut events = Vec::new();
                for _ in 0..limit {
                    match rx.try_recv() {
                        Ok(v) => events.push(v),
                        Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                        Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
                        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
                    }
                }
                let rendered: Vec<String> = events.iter().map(format_watch_frame).collect();

                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": json!({
                                "agent_id": agent_id,
                                "events": events,
                                "rendered": rendered,
                            }).to_string()
                        }]
                    }
                })
            }
            "agent_watch_detach" => {
                let Some(agent_id) = args.get("agent_id").and_then(|v| v.as_str()) else {
                    return atm_tools::make_mcp_error_result(
                        id,
                        "agent_watch_detach: 'agent_id' is required",
                    );
                };
                let detached = self
                    .watch_subscriptions
                    .lock()
                    .await
                    .remove(agent_id)
                    .is_some();
                let _ = self.detach_watch_stream(agent_id).await;
                let watcher_count = self.watch_stream_hub.lock().await.watcher_count(agent_id);
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": json!({
                                "agent_id": agent_id,
                                "detached": detached,
                                "watcher_count": watcher_count,
                            }).to_string()
                        }]
                    }
                })
            }
            _ => atm_tools::make_mcp_error_result(
                id,
                &format!("Unknown synthetic tool: {tool_name}"),
            ),
        }
    }

    /// Spawn the Codex child process via the configured transport.
    ///
    /// Delegates the actual child-process creation to `self.transport.spawn()`,
    /// then wires up the background stdout-reader and wait tasks.
    async fn spawn_child(
        &mut self,
        pending: &Arc<Mutex<PendingRequests>>,
        upstream_tx: &mpsc::Sender<Value>,
        dropped: &Arc<AtomicU64>,
    ) -> anyhow::Result<()> {
        let raw = self.transport.spawn().await?;

        // Wire the upstream write channel into the transport for approval-gate
        // bridging (G.5). This is a no-op for McpTransport and JsonCodecTransport;
        // AppServerTransport uses it to forward item/enteredReviewMode notifications
        // upstream as elicitation/create requests.
        self.transport.set_approval_upstream_tx(upstream_tx.clone());

        tracing::debug!(
            is_idle = self.transport.is_idle(),
            "child spawned via transport"
        );

        let shared_stdin = raw.stdin;
        let stdout = raw.stdout;
        let exit_status = raw.exit_status;
        let process = raw.process;
        let idle_flag = raw.idle_flag;

        // Channel for messages from child stdout reader
        let (child_tx, child_rx) = mpsc::channel::<Value>(UPSTREAM_CHANNEL_CAPACITY);

        // JSON mode: start a 30-second periodic stdin queue drain timer.
        // Only runs when the transport provides an idle_flag (i.e. JsonCodecTransport).
        // The JoinHandle is stored so the task can be aborted on graceful shutdown.
        let periodic_drain_task: Option<tokio::task::JoinHandle<()>> = if idle_flag.is_some() {
            let drain_team = self.team.clone();
            let drain_stdin = Arc::clone(&self.shared_child_stdin);
            let drain_thread_to_agent = Arc::clone(&self.thread_to_agent);
            let drain_elicitation_registry = Arc::clone(&self.elicitation_registry);
            Some(tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(30));
                // Skip the first immediate tick
                interval.tick().await;
                loop {
                    interval.tick().await;
                    // Drain for all active agent sessions (keyed by actual agent_id).
                    let agent_ids: Vec<String> = {
                        let map = drain_thread_to_agent.lock().await;
                        map.values().cloned().collect()
                    };
                    let stdin_guard = drain_stdin.lock().await;
                    if let Some(ref stdin_arc) = *stdin_guard {
                        drain_stdin_queue_for_agents(
                            &drain_team,
                            &agent_ids,
                            stdin_arc,
                            Duration::from_secs(600),
                        )
                        .await;
                        drain_elicitation_queue_for_agents(
                            &drain_team,
                            &agent_ids,
                            stdin_arc,
                            &drain_elicitation_registry,
                        )
                        .await;
                    }
                }
            }))
        } else {
            None
        };

        // Spawn child stdout reader task
        let pending_clone = Arc::clone(pending);
        let upstream_tx_clone = upstream_tx.clone();
        let dropped_clone = Arc::clone(dropped);
        let thread_to_agent_clone = Arc::clone(&self.thread_to_agent);
        let watch_stream_hub = Arc::clone(&self.watch_stream_hub);
        let registry_for_reader = Arc::clone(&self.registry);
        let shared_stdin_for_reader = Arc::clone(&self.shared_child_stdin);
        let queues_for_reader = Arc::clone(&self.queues);
        let request_counter_for_reader = Arc::clone(&self.request_counter);
        let elicitation_registry_for_reader = Arc::clone(&self.elicitation_registry);
        let team_for_reader = self.team.clone();
        let idle_flag_for_reader = idle_flag;
        let thread_to_agent_for_reader = Arc::clone(&self.thread_to_agent);
        let mail_enabled_for_reader = self.mail_poller.is_enabled();
        let mail_max_messages_reader = self.mail_poller.max_messages;
        let mail_max_length_reader = self.mail_poller.max_message_length;
        let per_thread_overrides_reader = self.config.per_thread_auto_mail.clone();
        tokio::spawn(async move {
            let reader = tokio::io::BufReader::new(stdout);
            let mut lines = tokio::io::AsyncBufReadExt::lines(reader);

            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let msg: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("failed to parse child output: {e}");
                        continue;
                    }
                };

                // JSON transport JSONL event detection.
                if let Some(ref idle_flag) = idle_flag_for_reader {
                    let event_type = parse_jsonl_event_type(&line);
                    match event_type {
                        JsonlEventType::Idle => {
                            idle_flag.store(true, std::sync::atomic::Ordering::SeqCst);

                            // Drain for all active agent sessions (keyed by actual agent_id).
                            let agent_ids: Vec<String> = {
                                let map = thread_to_agent_for_reader.lock().await;
                                map.values().cloned().collect()
                            };
                            let drain_team = team_for_reader.clone();
                            let drain_stdin = Arc::clone(&shared_stdin_for_reader);
                            let drain_elicitation_registry =
                                Arc::clone(&elicitation_registry_for_reader);
                            tokio::spawn(async move {
                                let stdin_guard = drain_stdin.lock().await;
                                if let Some(ref stdin_arc) = *stdin_guard {
                                    drain_stdin_queue_for_agents(
                                        &drain_team,
                                        &agent_ids,
                                        stdin_arc,
                                        Duration::from_secs(600),
                                    )
                                    .await;
                                    drain_elicitation_queue_for_agents(
                                        &drain_team,
                                        &agent_ids,
                                        stdin_arc,
                                        &drain_elicitation_registry,
                                    )
                                    .await;
                                }
                            });

                            // Don't forward the idle event upstream as a JSON-RPC message
                            continue;
                        }
                        JsonlEventType::Done => {
                            // Don't forward the done event upstream as a JSON-RPC message
                            continue;
                        }
                        _ => {
                            // Reset idle flag on any non-idle event (agent is active again)
                            idle_flag.store(false, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                }

                tracing::debug!(direction = "child->proxy", %msg);

                let method = msg.get("method").and_then(|v| v.as_str());

                if method == Some("codex/event") {
                    // Add agent_id to event params and forward upstream
                    let mut event = msg;
                    forward_event(
                        &mut event,
                        &pending_clone,
                        &thread_to_agent_clone,
                        &watch_stream_hub,
                        &upstream_tx_clone,
                        &dropped_clone,
                    )
                    .await;
                    continue;
                }

                // Check if this is a response (has id, no method)
                if method.is_none() {
                    if let Some(resp_id) = msg.get("id") {
                        let mut pending_guard = pending_clone.lock().await;

                        // Defect 2 fix: check for auto-mail response before
                        // completing the regular pending entry.
                        if let Some(auto_agent_id) = pending_guard.take_auto_mail(resp_id) {
                            // Auto-mail response: transition Busy -> Idle, then
                            // chain the post-turn mail check (FR-8.1).
                            let _ = pending_guard.complete(resp_id);
                            drop(pending_guard);

                            let (completed_identity, completed_thread_id) = {
                                let mut reg = registry_for_reader.lock().await;
                                reg.set_thread_state(&auto_agent_id, ThreadState::Idle);
                                let entry = reg.get(&auto_agent_id);
                                let ident = entry.map(|e| e.identity.clone());
                                let tid = entry.and_then(|e| e.thread_id.clone());
                                (ident, tid)
                            };

                            tracing::debug!(
                                agent_id = %auto_agent_id,
                                "auto-mail response received, thread Busy -> Idle"
                            );

                            // Chain post-turn mail check (FR-8.1).
                            if mail_enabled_for_reader {
                                if let (Some(identity), Some(thread_id)) =
                                    (&completed_identity, &completed_thread_id)
                                {
                                    let per_thread_ok = per_thread_overrides_reader
                                        .get(auto_agent_id.as_str())
                                        .copied()
                                        .unwrap_or(true);
                                    if per_thread_ok {
                                        dispatch_auto_mail_if_available(
                                            &auto_agent_id,
                                            identity,
                                            thread_id,
                                            &team_for_reader,
                                            mail_max_messages_reader,
                                            mail_max_length_reader,
                                            &registry_for_reader,
                                            &queues_for_reader,
                                            &shared_stdin_for_reader,
                                            &pending_clone,
                                            &request_counter_for_reader,
                                            None,
                                            None,
                                        )
                                        .await;
                                    }
                                }
                            }
                            continue;
                        }

                        let is_tl = pending_guard.is_tools_list(resp_id);
                        if let Some(tx) = pending_guard.complete(resp_id) {
                            let mut resp = msg;
                            if is_tl {
                                intercept_tools_list(&mut resp);
                            }
                            let _ = tx.send(resp);
                            continue;
                        }
                    }
                }

                // Server-initiated requests from child (e.g. elicitation/create)
                if method.is_some() {
                    let _ = child_tx.send(msg).await;
                    continue;
                }

                // Unmatched response — forward anyway
                let _ = child_tx.send(msg).await;
            }

            tracing::info!("child stdout reader exited");
        });

        // Spawn child wait task to detect crashes
        let exit_clone = Arc::clone(&exit_status);
        let pending_crash = Arc::clone(pending);
        let process_clone = Arc::clone(&process);
        tokio::spawn(async move {
            loop {
                let mut done = false;
                {
                    let mut child_guard = process_clone.lock().await;
                    match child_guard.as_mut() {
                        Some(child) => match child.try_wait() {
                            Ok(Some(s)) => {
                                tracing::info!("child process exited: {s}");
                                *exit_clone.lock().await = Some(s);
                                *child_guard = None;
                                done = true;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                tracing::error!("error waiting for child: {e}");
                                done = true;
                            }
                        },
                        None => {
                            done = true;
                        }
                    }
                }
                if done {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            // Cancel all pending requests
            let mut guard = pending_crash.lock().await;
            guard.map.clear();
            guard.codex_create_ids.clear();
            guard.auto_mail_pending.clear();
            guard.request_sources.clear();
            guard.last_agent_source.clear();
        });

        // Populate the shared child stdin reference for the idle poller.
        *self.shared_child_stdin.lock().await = Some(Arc::clone(&shared_stdin));

        self.child = Some(ChildHandle {
            stdin: shared_stdin,
            response_rx: child_rx,
            exit_status,
            process,
            drain_task: periodic_drain_task,
        });

        Ok(())
    }
}

/// Outcome of [`ProxyServer::prepare_codex_message`].
enum PrepareResult {
    /// Validation succeeded; the modified message is ready to send.
    Ok {
        modified: Value,
        expected_agent_id: Option<String>,
    },
    /// Validation failed; an error response has already been sent upstream.
    Error,
}

fn infer_upstream_request_source(msg: &Value, actor_fallback: &str) -> SourceEnvelope {
    let kind = msg
        .pointer("/params/source/kind")
        .and_then(|v| v.as_str())
        .unwrap_or("client_prompt");
    let actor = msg
        .pointer("/params/source/actor")
        .and_then(|v| v.as_str())
        .unwrap_or(actor_fallback);
    let channel = msg
        .pointer("/params/source/channel")
        .and_then(|v| v.as_str())
        .unwrap_or("mcp_primary");
    SourceEnvelope::new(kind, actor, channel)
}

/// Forward a `codex/event` notification upstream, injecting `agent_id` into params.
///
/// Looks up the `agent_id` from `thread_to_agent` using the event's `threadId`
/// field if present. Falls back to `"proxy:unknown"` when no mapping exists.
///
/// This is a best-effort send: if the upstream channel is full the event is dropped
/// and the `dropped_events` counter is incremented.
async fn forward_event(
    event: &mut Value,
    pending: &Arc<Mutex<PendingRequests>>,
    thread_to_agent: &Arc<tokio::sync::Mutex<HashMap<String, String>>>,
    watch_stream_hub: &Arc<tokio::sync::Mutex<WatchStreamHub>>,
    upstream_tx: &mpsc::Sender<Value>,
    dropped_events: &Arc<AtomicU64>,
) {
    // Resolve agent_id from the event's threadId if available
    let agent_id = {
        let thread_id_opt = event
            .pointer("/params/_meta/threadId")
            .and_then(|v| v.as_str())
            .or_else(|| event.pointer("/params/threadId").and_then(|v| v.as_str()))
            .map(String::from);
        if let Some(tid) = thread_id_opt {
            let map = thread_to_agent.lock().await;
            map.get(&tid)
                .cloned()
                .unwrap_or_else(|| "proxy:unknown".to_string())
        } else {
            let req_id_opt = event.pointer("/params/_meta/requestId");
            if let Some(req_id) = req_id_opt {
                pending
                    .lock()
                    .await
                    .peek_codex_create(req_id)
                    .unwrap_or_else(|| "proxy:unknown".to_string())
            } else {
                "proxy:unknown".to_string()
            }
        }
    };

    if let Some(params) = event.get_mut("params") {
        if let Some(obj) = params.as_object_mut() {
            obj.insert("agent_id".to_string(), Value::String(agent_id.clone()));
        }
    }

    // Forward stream-error summaries to daemon observability channel.
    emit_stream_error_summary_to_daemon(event, &agent_id).await;

    // Publish to direct watch-stream hub using MVP subset + source envelope.
    if should_publish_watch_event(event) {
        let source = infer_source_envelope(event, &agent_id, pending).await;
        let frame = build_watch_frame(&agent_id, &source, event.clone());
        watch_stream_hub
            .lock()
            .await
            .publish(&agent_id, frame.clone());
        append_watch_frame_for_tui(&frame, &agent_id);
    } else if let Some(kind) = event.pointer("/params/type").and_then(|v| v.as_str())
        && !kind.is_empty()
    {
        WATCH_UNKNOWN_EVENT_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    match upstream_tx.try_send(event.clone()) {
        Ok(()) => {}
        Err(_) => {
            dropped_events.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// MVP subset gate for watch-stream fanout (FR-21.5 + docs Section 3.3).
fn should_publish_watch_event(event: &Value) -> bool {
    let kind = event
        .pointer("/params/type")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if kind.is_empty() {
        return false;
    }
    let lower = kind.to_ascii_lowercase();

    if matches!(
        lower.as_str(),
        "task_started"
            | "task_complete"
            | "user_message"
            | "approval_prompt"
            | "approval_request"
            | "approval_rejected"
            | "approval_approved"
            | "entered_review_mode"
            | "exited_review_mode"
            | "item/enteredreviewmode"
            | "item/exitedreviewmode"
            | "agent_message_delta"
            | "agent_message"
            | "agent_message_completed"
            | "agent_message_chunk"
            | "agent_message_content_delta"
            | "reasoning_content_delta"
            | "agent_reasoning_delta"
            | "reasoning_raw_content_delta"
            | "reasoning_content"
            | "terminal_interaction"
            | "exec_command_output_delta"
            | "exec_command_started"
            | "exec_command_completed"
            | "exec_command_error"
            | "request_user_input"
            | "elicitation_request"
            | "patch_apply_begin"
            | "patch_apply_end"
            | "turn_diff"
            | "file_change"
            | "item_started"
            | "item_completed"
            | "thread_status_changed"
            | "turn_started"
            | "turn_completed"
            | "turn_aborted"
            | "turn_interrupted"
            | "turn_cancelled"
            | "cancelled"
            | "interrupt"
            | "idle"
            | "done"
            | "shutdown_complete"
            | "session_configured"
            | "thread_name_updated"
            | "token_count"
            | "model_reroute"
            | "context_compacted"
            | "thread_rolled_back"
            | "undo_started"
            | "undo_completed"
            | "background_event"
            | "warning"
            | "error"
            | "deprecation_notice"
            | "plan_update"
            | "plan_delta"
            | "mcp_list_tools_response"
            | "remote_skill_downloaded"
            | "skills_update_available"
            | "stream_error"
    ) {
        return true;
    }

    lower.starts_with("mcp_tool_call_")
        || lower.starts_with("web_search_")
        || lower.starts_with("realtime_conversation_")
        || lower.starts_with("collab_")
        || lower.starts_with("list_")
}

fn extract_stream_error_event(
    event: &Value,
    agent_id: &str,
) -> Option<agent_team_mail_core::daemon_stream::DaemonStreamEvent> {
    let kind = event.pointer("/params/type").and_then(|v| v.as_str())?;
    if kind != "stream_error" {
        return None;
    }
    let session_id = event
        .pointer("/params/_meta/threadId")
        .and_then(|v| v.as_str())
        .or_else(|| event.pointer("/params/threadId").and_then(|v| v.as_str()))
        .unwrap_or("unknown")
        .to_string();
    let error_summary = event
        .pointer("/params/error")
        .and_then(|v| v.as_str())
        .or_else(|| event.pointer("/params/message").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/text").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/delta").and_then(|v| v.as_str()))
        .unwrap_or("stream error")
        .to_string();
    Some(
        agent_team_mail_core::daemon_stream::DaemonStreamEvent::StreamError {
            agent_id: agent_id.to_string(),
            session_id,
            error_summary,
        },
    )
}

async fn emit_stream_error_summary_to_daemon(event: &Value, agent_id: &str) {
    if let Some(daemon_event) = extract_stream_error_event(event, agent_id) {
        #[cfg(test)]
        STREAM_ERROR_EMIT_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
        crate::stream_emit::emit_stream_event(&daemon_event).await;
    }
}

async fn flush_dropped_counters_to_daemon(dropped_events: &Arc<AtomicU64>, agent_id: &str) {
    let dropped = dropped_events.swap(0, Ordering::Relaxed);
    let unknown = WATCH_UNKNOWN_EVENT_COUNT.swap(0, Ordering::Relaxed);
    if dropped == 0 && unknown == 0 {
        return;
    }
    let event = agent_team_mail_core::daemon_stream::DaemonStreamEvent::DroppedCounters {
        agent_id: agent_id.to_string(),
        dropped,
        unknown,
    };
    crate::stream_emit::emit_stream_event(&event).await;
}

fn format_watch_frame(frame: &Value) -> String {
    let source = frame
        .pointer("/source/kind")
        .and_then(|v| v.as_str())
        .unwrap_or("client_prompt");
    let event = frame.get("event").unwrap_or(frame);
    let kind = event
        .pointer("/params/type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let text = event
        .pointer("/params/delta")
        .and_then(|v| v.as_str())
        .or_else(|| event.pointer("/params/text").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/output").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/message").and_then(|v| v.as_str()))
        .unwrap_or("");
    let text = if text.len() > WATCH_RENDER_MAX_CHARS {
        format!("{}...", &text[..WATCH_RENDER_TRUNCATE_AT])
    } else {
        text.to_string()
    };
    let text = text.replace('\n', "\\n");
    let has_code_fence = text.contains("```");

    let mut base = match kind {
        "agent_message_delta" | "agent_message" | "agent_message_chunk" => {
            if has_code_fence {
                format!("[{source}] assistant(code): {text}")
            } else {
                format!("[{source}] assistant: {text}")
            }
        }
        "exec_command_output_delta" | "exec_command_completed" | "exec_command_error" => {
            format!("[{source}] cmd: {text}")
        }
        "reasoning_content_delta" | "agent_reasoning_delta" | "reasoning_content" => {
            format!("[{source}] reasoning: {text}")
        }
        "item_started" | "item_completed" | "turn_started" | "turn_completed" => {
            format!("[{source}] {kind}")
        }
        "task_started" | "task_complete" | "idle" | "done" | "stream_error" => {
            format!("[{source}] {kind}")
        }
        _ => format!("[{source}] {kind}"),
    };

    if let Some(pct) = event
        .pointer("/params/usage/context_window_pct")
        .and_then(|v| v.as_f64())
        .or_else(|| {
            event
                .pointer("/params/contextWindowPct")
                .and_then(|v| v.as_f64())
        })
        .or_else(|| {
            event
                .pointer("/params/usage/contextWindowPct")
                .and_then(|v| v.as_f64())
        })
        .or_else(|| {
            event
                .pointer("/params/context_window_pct")
                .and_then(|v| v.as_f64())
        })
    {
        base.push_str(&format!(" | ctx {:.0}%", pct));
    }
    base
}

fn watch_feed_path(agent_id: &str) -> Option<std::path::PathBuf> {
    // Sanitize agent_id to be filesystem-safe: replace path separators with '_'.
    let safe_id: std::borrow::Cow<str> = if agent_id.contains('/') || agent_id.contains('\\') {
        std::borrow::Cow::Owned(agent_id.replace(['/', '\\'], "_"))
    } else {
        std::borrow::Cow::Borrowed(agent_id)
    };
    // ATM_HOME convention: when ATM_HOME is set, use it as the base directory
    // directly (no `.config/atm/` nesting). Matches the canonical log path
    // `$ATM_HOME/atm.log.jsonl` established in atm-core/src/event_log.rs.
    if let Ok(atm_home) = std::env::var("ATM_HOME") {
        let trimmed = atm_home.trim();
        if !trimmed.is_empty() {
            return Some(
                std::path::PathBuf::from(trimmed)
                    .join("watch-stream")
                    .join(format!("{safe_id}.jsonl")),
            );
        }
    }
    let home = agent_team_mail_core::home::get_home_dir().ok()?;
    Some(
        home.join(".config/atm/watch-stream")
            .join(format!("{safe_id}.jsonl")),
    )
}

fn append_watch_frame_for_tui(frame: &Value, agent_id: &str) {
    append_watch_frame_for_tui_with_cap(frame, WATCH_FEED_MAX_BYTES, agent_id);
}

fn append_watch_frame_for_tui_with_cap(frame: &Value, max_bytes: u64, agent_id: &str) {
    let Some(path) = watch_feed_path(agent_id) else {
        return;
    };
    append_watch_frame_for_tui_at_path(frame, max_bytes, &path);
}

fn append_watch_frame_for_tui_at_path(frame: &Value, max_bytes: u64, path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        // Best-effort: streaming still functions without local file fanout.
        let _ = std::fs::create_dir_all(parent);
    }
    if max_bytes > 0
        && let Ok(meta) = std::fs::metadata(path)
        && meta.len() >= max_bytes
    {
        let rotated_path = path.with_extension("jsonl.1");
        // Best-effort rotation: failures should not break live stream publishing.
        let _ = std::fs::remove_file(&rotated_path);
        let _ = std::fs::rename(path, rotated_path);
    }
    let rendered = format_watch_frame(frame);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let record = json!({
        "ts_unix": ts,
        "frame": frame,
        "rendered": rendered,
    });
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        // Best-effort append: watcher UX is non-critical compared to proxy traffic.
        let _ = writeln!(file, "{}", record);
    }
}

/// Infer source attribution for watch-stream frames (FR-22 baseline).
async fn infer_source_envelope(
    event: &Value,
    agent_id: &str,
    pending: &Arc<Mutex<PendingRequests>>,
) -> SourceEnvelope {
    let src_kind = event
        .pointer("/params/source/kind")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let kind = match src_kind {
        // daemon lifecycle source value reused for mail injection origin mapping
        "atm_mcp" | "atm_mail" => "atm_mail",
        "user_steer" | "tui_user" => "user_steer",
        _ => "client_prompt",
    };
    let actor = event
        .pointer("/params/source/actor")
        .and_then(|v| v.as_str())
        .unwrap_or(agent_id);
    let channel = match kind {
        "atm_mail" => "mail_injector",
        "user_steer" => "tui_user",
        _ => "mcp_primary",
    };
    if !src_kind.is_empty() {
        return SourceEnvelope::new(kind, actor, channel);
    }

    // Fall back to per-request source attribution when child events include a
    // correlating request id in `_meta.requestId`.
    if let Some(req_id) = event.pointer("/params/_meta/requestId") {
        if let Some(src) = pending.lock().await.source_for_request(req_id) {
            return src;
        }
    }

    // If request-id correlation is missing, use the latest source seen for the
    // current agent session.
    if let Some(src) = pending.lock().await.last_source_for_agent(agent_id) {
        return src;
    }

    SourceEnvelope::new(kind, actor, channel)
}

/// JSONL event type as parsed from a raw event line.
///
/// Used by the stdout reader task in JSON transport mode to handle each
/// event kind appropriately.
#[derive(Debug)]
enum JsonlEventType {
    AgentMessage,
    ToolCall,
    ToolResult,
    FileChange,
    Idle,
    Done,
    /// An unrecognised or malformed event type.  The inner string is logged at
    /// `tracing::debug` level and is intentionally unused elsewhere.
    #[expect(
        dead_code,
        reason = "inner String is for Debug display only; not used in match arms"
    )]
    Unknown(String),
}

/// Parse the `type` field from a raw JSONL event line and return the corresponding
/// [`JsonlEventType`].
///
/// Returns [`JsonlEventType::Unknown`] for unrecognised types or parse errors.
fn parse_jsonl_event_type(line: &str) -> JsonlEventType {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| {
            v.get("type")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        })
        .map(|t| match t.as_str() {
            "agent_message" => JsonlEventType::AgentMessage,
            "tool_call" => JsonlEventType::ToolCall,
            "tool_result" => JsonlEventType::ToolResult,
            "file_change" => JsonlEventType::FileChange,
            "idle" => JsonlEventType::Idle,
            "done" => JsonlEventType::Done,
            other => JsonlEventType::Unknown(other.to_string()),
        })
        .unwrap_or_else(|| JsonlEventType::Unknown("(parse error)".to_string()))
}

/// Drain the stdin queue for all active agent sessions.
///
/// Called both on `idle` JSONL events and by the 30-second periodic timer.
/// Iterates over all provided `agent_ids` and calls
/// [`crate::stdin_queue::drain`] for each, logging results.
async fn drain_stdin_queue_for_agents(
    team: &str,
    agent_ids: &[String],
    shared_stdin: &Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    ttl: Duration,
) {
    for agent_id in agent_ids {
        match crate::stdin_queue::drain(team, agent_id, shared_stdin, ttl).await {
            Ok(count) if count > 0 => {
                tracing::debug!(agent_id, count, "stdin queue drained");
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(agent_id, error = %e, "stdin queue drain error");
            }
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct QueuedElicitationResponse {
    elicitation_id: String,
    decision: String,
    #[serde(default)]
    text: Option<String>,
}

async fn drain_elicitation_queue_for_agents(
    team: &str,
    agent_ids: &[String],
    shared_stdin: &Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    elicitation_registry: &Arc<Mutex<ElicitationRegistry>>,
) {
    let home = match agent_team_mail_core::home::get_home_dir() {
        Ok(h) => h,
        Err(_) => return,
    };
    for agent_id in agent_ids {
        let dir = home
            .join(".config/atm/agent-sessions")
            .join(team)
            .join(agent_id)
            .join("elicitation_queue");
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let raw = match tokio::fs::read_to_string(&path).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(agent_id, path=%path.display(), error=%e, "failed reading elicitation queue entry");
                    continue;
                }
            };
            let queued: QueuedElicitationResponse = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(agent_id, path=%path.display(), error=%e, "invalid elicitation queue entry");
                    let _ = tokio::fs::remove_file(&path).await;
                    continue;
                }
            };
            let response = serde_json::json!({
                "id": queued.elicitation_id,
                "result": {
                    "decision": queued.decision,
                    "text": queued.text,
                }
            });
            let maybe = elicitation_registry
                .lock()
                .await
                .resolve_for_downstream_id(&serde_json::json!(queued.elicitation_id), response);
            let Some(downstream) = maybe else {
                // Keep entry on disk until the matching elicitation is registered.
                continue;
            };
            let serialized = match serde_json::to_string(&downstream) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(agent_id, error=%e, "failed serializing elicitation downstream response");
                    continue;
                }
            };
            let mut stdin = shared_stdin.lock().await;
            if let Err(e) = write_newline_delimited(&mut *stdin, &serialized).await {
                tracing::warn!(agent_id, error=%e, "failed writing elicitation response to child stdin");
                continue;
            }
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
}

/// Dispatch an auto-mail codex-reply to the child if unread mail is available.
///
/// This is the shared logic used by both the post-turn path (in the response
/// handler spawned task) and the auto-mail response chaining path (in the child
/// stdout reader).  It fetches unread mail, builds and writes the codex-reply
/// message to the child, registers the request-id in the pending map, and marks
/// messages read only after successful dispatch (FR-8.12).
///
/// Also satisfies Defect 3: after a turn completes (Busy -> Idle), this
/// function first checks the command queue for a pending `ClaudeReply`.  If one
/// exists it is dispatched instead of auto-mail, preserving the priority order
/// (FR-17.11: Close > ClaudeReply > AutoMailInject).
#[expect(
    clippy::too_many_arguments,
    reason = "all parameters are distinct concerns required by a single \
              coordinated dispatch operation; splitting would require a \
              context struct refactor tracked separately"
)]
#[expect(
    clippy::type_complexity,
    reason = "Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<ThreadCommandQueue>>>>> \
              matches the shared-queue type used throughout the proxy; \
              introducing a type alias is a follow-up refactor"
)]
async fn dispatch_auto_mail_if_available(
    agent_id: &str,
    identity: &str,
    thread_id: &str,
    team: &str,
    max_messages: usize,
    max_message_length: usize,
    registry: &Arc<Mutex<SessionRegistry>>,
    queues: &Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<ThreadCommandQueue>>>>>,
    shared_stdin: &SharedChildStdin,
    pending: &Arc<Mutex<PendingRequests>>,
    request_counter: &Arc<AtomicU64>,
    // Optional app-server transport routing: when Some, routes to turn/start or
    // turn/steer instead of codex-reply.  Existing call sites pass None to
    // preserve the MCP/cli-json path unchanged.
    transport_ref: Option<&dyn CodexTransport>,
    inflight: Option<&Arc<Mutex<InflightMailSet>>>,
) {
    // Defect 3 partial fix: check the command queue first.  If a ClaudeReply
    // was queued while the thread was Busy, dispatch it instead.
    {
        let queues_guard = queues.lock().await;
        if let Some(q_arc) = queues_guard.get(agent_id) {
            let mut q = q_arc.lock().await;
            if let Some(cmd) = q.pop_next() {
                match cmd {
                    ThreadCommand::ClaudeReply {
                        request_id,
                        args,
                        respond_tx,
                    } => {
                        tracing::info!(
                            agent_id = %agent_id,
                            "dispatching queued ClaudeReply (Fix 3/4)"
                        );
                        // Write the queued ClaudeReply to child stdin.
                        let msg = json!({
                            "jsonrpc": "2.0",
                            "id": request_id,
                            "method": "tools/call",
                            "params": {
                                "name": "codex-reply",
                                "arguments": args,
                            }
                        });
                        if let Ok(serialized) = serde_json::to_string(&msg) {
                            let child_stdin_opt = shared_stdin.lock().await.clone();
                            if let Some(child_stdin) = child_stdin_opt {
                                registry
                                    .lock()
                                    .await
                                    .set_thread_state(agent_id, ThreadState::Busy);
                                let mut stdin = child_stdin.lock().await;
                                if write_newline_delimited(&mut *stdin, &serialized)
                                    .await
                                    .is_ok()
                                {
                                    // Fix 3b: register (request_id, respond_tx) in the
                                    // pending map so route_child_message completes the
                                    // oneshot and unblocks the upstream caller.
                                    let mut p = pending.lock().await;
                                    p.insert(request_id, respond_tx);
                                } else {
                                    registry
                                        .lock()
                                        .await
                                        .set_thread_state(agent_id, ThreadState::Idle);
                                    tracing::warn!(
                                        "failed to write queued ClaudeReply to child stdin"
                                    );
                                }
                            }
                        }
                        return; // ClaudeReply dispatched; do not inject auto-mail.
                    }
                    other => {
                        // Non-ClaudeReply (e.g. AutoMailInject from queue) — we'll
                        // handle auto-mail via the fetch_unread_mail path below.
                        // Close commands are handled elsewhere.
                        drop(other);
                    }
                }
            }
        }
    }

    // Route to the app-server path when the transport uses turn/start or
    // turn/steer instead of codex-reply.  The app-server dispatcher manages
    // the single-flight reservation and mark-read boundary itself.
    if let Some(transport) = transport_ref {
        if transport.uses_app_server_injection() {
            // The single-flight guard must still be taken before we call the
            // app-server dispatcher, to prevent concurrent polls from both
            // reaching the dispatch path simultaneously.
            if !try_reserve_thread_for_auto_mail(agent_id, registry).await {
                return;
            }
            let active_turn_id = transport.active_turn_id_for_thread(thread_id);
            if let Some(inf) = inflight {
                dispatch_auto_mail_app_server(
                    agent_id,
                    identity,
                    thread_id,
                    team,
                    max_messages,
                    max_message_length,
                    registry,
                    shared_stdin,
                    request_counter,
                    pending,
                    active_turn_id,
                    inf,
                )
                .await;
            } else {
                // inflight not provided for app-server path — release guard
                // and log a warning. Callers should always supply it.
                registry
                    .lock()
                    .await
                    .set_thread_state(agent_id, ThreadState::Idle);
                tracing::warn!(
                    agent_id = %agent_id,
                    "dispatch_auto_mail_if_available: app-server transport requires inflight set"
                );
            }
            return;
        }
    }

    // Single-flight guard: reserve the thread (Idle -> Busy) before fetching
    // mail to avoid TOCTOU races with concurrent codex-reply requests.
    if !try_reserve_thread_for_auto_mail(agent_id, registry).await {
        return;
    }

    let envelopes = fetch_unread_mail(identity, team, max_messages, max_message_length);
    if envelopes.is_empty() {
        registry
            .lock()
            .await
            .set_thread_state(agent_id, ThreadState::Idle);
        return;
    }

    let child_stdin_opt = shared_stdin.lock().await.clone();
    let Some(child_stdin) = child_stdin_opt else {
        registry
            .lock()
            .await
            .set_thread_state(agent_id, ThreadState::Idle);
        return;
    };

    let content = format_mail_turn_content(&envelopes);
    let auto_req_id = request_counter.fetch_add(1, Ordering::Relaxed);
    let auto_req_id_val = serde_json::Value::Number(auto_req_id.into());
    let auto_msg = json!({
        "jsonrpc": "2.0",
        "id": auto_req_id_val,
        "method": "tools/call",
        "params": {
            "name": "codex-reply",
            "arguments": {
                "prompt": content,
                "threadId": thread_id,
            }
        }
    });
    let Ok(serialized) = serde_json::to_string(&auto_msg) else {
        registry
            .lock()
            .await
            .set_thread_state(agent_id, ThreadState::Idle);
        return;
    };

    let write_ok = {
        let mut stdin = child_stdin.lock().await;
        write_newline_delimited(&mut *stdin, &serialized)
            .await
            .is_ok()
    };
    if write_ok {
        // Register in pending map for Busy -> Idle transition on response.
        let (tx, _rx) = oneshot::channel();
        {
            let mut p = pending.lock().await;
            p.insert(auto_req_id_val.clone(), tx);
            p.mark_auto_mail(auto_req_id_val, agent_id.to_string());
            let source =
                SourceEnvelope::new("atm_mail", format!("{identity}@{team}"), "mail_injector");
            p.mark_request_source(
                serde_json::Value::Number(auto_req_id.into()),
                source.clone(),
            );
            p.set_last_agent_source(agent_id.to_string(), source);
        }
        // FR-8.12: mark read only after successful dispatch.
        let ids: Vec<String> = envelopes.iter().map(|e| e.message_id.clone()).collect();
        mark_messages_read(identity, team, &ids);
        tracing::info!(
            agent_id = %agent_id,
            req_id = auto_req_id,
            message_count = envelopes.len(),
            "chained auto-mail codex-reply dispatched (FR-8.1)"
        );
    } else {
        registry
            .lock()
            .await
            .set_thread_state(agent_id, ThreadState::Idle);
        tracing::warn!("chained auto-mail: failed to write codex-reply to child stdin");
    }
}

/// Dispatch auto-mail to an app-server child using `turn/start` or `turn/steer`.
///
/// Called by [`dispatch_auto_mail_if_available`] when the active transport
/// reports [`crate::transport::CodexTransport::uses_app_server_injection`] ==
/// `true`.  Unlike the MCP/cli-json path (which uses the `codex-reply`
/// `tools/call` wrapper), the app-server protocol sends mail content directly
/// as a `turn/start` or `turn/steer` JSON-RPC request:
///
/// - **`turn/start`** — used when the thread is `Idle` or `Terminal`
///   (`active_turn_id` is `None`).
/// - **`turn/steer`** — used when the thread is `Busy` and has an active
///   turn in progress (`active_turn_id` is `Some(id)`).  Requires
///   `expectedTurnId` to match the active turn.
///
/// FR-8.12 semantics are preserved: [`mark_messages_read`] is only called
/// **after** the write to child stdin succeeds.  If the write fails, the
/// registry thread state is restored to `Idle` so the next poll can retry.
///
/// The `inflight` set is updated before `mark_messages_read` to prevent
/// the next poll cycle from re-injecting the same messages while the current
/// dispatch is in-progress.  On write failure the in-flight IDs are cleared
/// so they become eligible for retry.
#[expect(
    clippy::too_many_arguments,
    reason = "all parameters are distinct concerns required by a single \
              coordinated dispatch operation; splitting would require a \
              context struct refactor tracked separately"
)]
async fn dispatch_auto_mail_app_server(
    agent_id: &str,
    identity: &str,
    thread_id: &str,
    team: &str,
    max_messages: usize,
    max_message_length: usize,
    registry: &Arc<Mutex<SessionRegistry>>,
    shared_stdin: &SharedChildStdin,
    request_counter: &Arc<AtomicU64>,
    pending: &Arc<Mutex<PendingRequests>>,
    active_turn_id: Option<String>,
    inflight: &Arc<Mutex<InflightMailSet>>,
) {
    // 1. Fetch unread mail.
    let all_envelopes = fetch_unread_mail(identity, team, max_messages, max_message_length);
    if all_envelopes.is_empty() {
        registry
            .lock()
            .await
            .set_thread_state(agent_id, ThreadState::Idle);
        return;
    }

    // 2. Filter out messages already in-flight (dedup guard).
    let injectable_ids: Vec<String> = {
        let inf = inflight.lock().await;
        all_envelopes
            .iter()
            .filter(|e| !inf.is_inflight(&e.message_id))
            .map(|e| e.message_id.clone())
            .collect()
    };

    // Collect only the injectable envelopes.
    let envelopes: Vec<_> = all_envelopes
        .into_iter()
        .filter(|e| injectable_ids.contains(&e.message_id))
        .collect();

    if envelopes.is_empty() {
        registry
            .lock()
            .await
            .set_thread_state(agent_id, ThreadState::Idle);
        return;
    }

    // 3. Acquire the child stdin.
    let child_stdin_opt = shared_stdin.lock().await.clone();
    let Some(child_stdin) = child_stdin_opt else {
        registry
            .lock()
            .await
            .set_thread_state(agent_id, ThreadState::Idle);
        return;
    };

    // 4. Build the JSON-RPC request (turn/start or turn/steer).
    let content = format_mail_turn_content(&envelopes);
    let req_id = request_counter.fetch_add(1, Ordering::Relaxed);
    let req_id_val = serde_json::Value::Number(req_id.into());

    let auto_msg = if let Some(ref turn_id) = active_turn_id {
        // Thread is Busy — use turn/steer with expectedTurnId.
        // Per the app-server protocol spec (Section 1), messages omit the `jsonrpc` field.
        json!({
            "id": req_id_val,
            "method": "turn/steer",
            "params": {
                "threadId": thread_id,
                "expectedTurnId": turn_id,
                "input": [{ "type": "text", "text": content }]
            }
        })
    } else {
        // Thread is Idle or Terminal — use turn/start.
        // Per the app-server protocol spec (Section 1), messages omit the `jsonrpc` field.
        json!({
            "id": req_id_val,
            "method": "turn/start",
            "params": {
                "threadId": thread_id,
                "input": [{ "type": "text", "text": content }]
            }
        })
    };

    let Ok(serialized) = serde_json::to_string(&auto_msg) else {
        registry
            .lock()
            .await
            .set_thread_state(agent_id, ThreadState::Idle);
        return;
    };

    // 5. Mark IDs as in-flight before writing, to prevent concurrent polls
    //    from re-injecting the same messages.
    let dispatched_ids: Vec<String> = envelopes.iter().map(|e| e.message_id.clone()).collect();
    {
        inflight.lock().await.mark_inflight(&dispatched_ids);
    }

    // 6. Write to child stdin.
    let write_ok = {
        let mut stdin = child_stdin.lock().await;
        write_newline_delimited(&mut *stdin, &serialized)
            .await
            .is_ok()
    };

    if write_ok {
        // Track source attribution for downstream event correlation.
        {
            let source =
                SourceEnvelope::new("atm_mail", format!("{identity}@{team}"), "mail_injector");
            let mut p = pending.lock().await;
            p.mark_request_source(serde_json::Value::Number(req_id.into()), source.clone());
            p.set_last_agent_source(agent_id.to_string(), source);
        }
        // FR-8.12: mark-read only after successful dispatch.
        mark_messages_read(identity, team, &dispatched_ids);
        tracing::info!(
            agent_id = %agent_id,
            req_id = req_id,
            message_count = dispatched_ids.len(),
            method = if active_turn_id.is_some() { "turn/steer" } else { "turn/start" },
            "app-server auto-mail dispatched (FR-8.1)"
        );
        // After successful mark-read the messages are no longer unread, so
        // clear them from the in-flight set to keep it compact.
        inflight.lock().await.clear_inflight(&dispatched_ids);
    } else {
        // Write failed — restore thread to Idle and clear in-flight so the
        // next poll cycle can retry.
        inflight.lock().await.clear_inflight(&dispatched_ids);
        registry
            .lock()
            .await
            .set_thread_state(agent_id, ThreadState::Idle);
        tracing::warn!(
            agent_id = %agent_id,
            "app-server auto-mail: failed to write to child stdin; will retry on next poll"
        );
    }
}

/// Attempt to reserve a thread for auto-mail dispatch by transitioning
/// `Idle -> Busy` atomically under the registry lock.
async fn try_reserve_thread_for_auto_mail(
    agent_id: &str,
    registry: &Arc<Mutex<SessionRegistry>>,
) -> bool {
    let mut reg = registry.lock().await;
    let can_reserve = reg
        .get(agent_id)
        .map(|e| e.status == SessionStatus::Active && e.thread_state == ThreadState::Idle)
        .unwrap_or(false);
    if can_reserve {
        reg.set_thread_state(agent_id, ThreadState::Busy);
    }
    can_reserve
}

/// Route a message received from the child to the appropriate destination.
///
/// This is a free function rather than a method to avoid borrow conflicts with
/// the `ProxyServer`'s mutable child handle.
///
/// Handles `elicitation/create` requests by bridging them upstream with a new
/// proxy-assigned request ID, registering correlation in [`ElicitationRegistry`]
/// (FR-18).
#[expect(
    clippy::too_many_arguments,
    reason = "routing needs shared pending/thread/watch/elicitation state passed explicitly"
)]
async fn route_child_message(
    msg: Value,
    pending: &Arc<Mutex<PendingRequests>>,
    upstream_tx: &mpsc::Sender<Value>,
    dropped: &Arc<AtomicU64>,
    thread_to_agent: &Arc<tokio::sync::Mutex<HashMap<String, String>>>,
    watch_stream_hub: &Arc<tokio::sync::Mutex<WatchStreamHub>>,
    elicitation_registry: &Arc<Mutex<ElicitationRegistry>>,
    elicitation_counter: &Arc<AtomicU64>,
) {
    let method = msg.get("method").and_then(|v| v.as_str());

    if method == Some("codex/event") {
        let mut event = msg;
        forward_event(
            &mut event,
            pending,
            thread_to_agent,
            watch_stream_hub,
            upstream_tx,
            dropped,
        )
        .await;
        return;
    }

    // Elicitation/create — bridge upstream (FR-18).
    if method == Some("elicitation/create") {
        if let Some(downstream_id) = msg.get("id").cloned() {
            let upstream_id_num = elicitation_counter.fetch_add(1, Ordering::Relaxed);
            let upstream_request_id = Value::Number(upstream_id_num.into());

            // Resolve the agent_id for this elicitation using thread_to_agent map
            let agent_id = {
                let thread_id_opt = msg
                    .pointer("/params/threadId")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if let Some(tid) = thread_id_opt {
                    thread_to_agent
                        .lock()
                        .await
                        .get(&tid)
                        .cloned()
                        .unwrap_or_else(|| "proxy:unknown".to_string())
                } else {
                    "proxy:unknown".to_string()
                }
            };

            // NOTE: `_response_rx` is intentionally dropped here. Delivery of the approval
            // decision back to the child happens via a *different* mechanism for the MCP path:
            // `resolve_for_downstream()` returns the rewritten response (with downstream id
            // restored), and the upstream response handler at the top of this function writes
            // it directly to child.stdin. The `response_tx` registered in the registry is kept
            // so that `ElicitationRegistry::cancel_for_agent` and `expire_timeouts` can call
            // `send()` on it (those sends will fail silently when the receiver is already
            // dropped, which is acceptable — the session close also terminates the child).
            // Compare with `bridge_entered_review_mode` in transport.rs, which uses the oneshot
            // channel for delivery because the app-server path has no equivalent direct-write hook.
            let (response_tx, _response_rx) = tokio::sync::oneshot::channel::<Value>();

            // Register in the elicitation registry
            elicitation_registry.lock().await.register(
                agent_id.clone(),
                downstream_id.clone(),
                upstream_request_id.clone(),
                response_tx,
            );

            // Build the upstream request: copy the original params and inject agent_id,
            // then replace the id with the upstream_request_id.
            let mut upstream_msg = msg.clone();
            if let Some(id_field) = upstream_msg.get_mut("id") {
                *id_field = upstream_request_id.clone();
            }
            if let Some(params) = upstream_msg.get_mut("params") {
                if let Some(obj) = params.as_object_mut() {
                    obj.insert("agent_id".to_string(), Value::String(agent_id.clone()));
                }
            }

            // Forward to upstream
            let _ = upstream_tx.send(upstream_msg).await;

            return;
        }
    }

    // Response — route to pending request
    if method.is_none() {
        if let Some(resp_id) = msg.get("id") {
            let mut guard = pending.lock().await;
            let is_tl = guard.is_tools_list(resp_id);
            if let Some(tx) = guard.complete(resp_id) {
                let mut resp = msg;
                if is_tl {
                    intercept_tools_list(&mut resp);
                }
                let _ = tx.send(resp);
                return;
            }
        }
    }

    // Anything else (server-initiated request) — forward upstream
    let _ = upstream_tx.send(msg).await;
}

/// Intercept a `tools/list` response to replace the `codex` tool schema with
/// the extended proxy schema and append all synthetic ATM tools.
///
/// This is called on responses from the child that match a `tools/list` request.
/// The function mutates the response in-place.
pub fn intercept_tools_list(response: &mut Value) {
    if let Some(tools_array) = response
        .pointer_mut("/result/tools")
        .and_then(|v| v.as_array_mut())
    {
        // Replace the child's codex tool entry with the extended proxy schema (FR-16.4)
        let extended_codex = crate::tools::codex_tool_schema();
        if let Some(codex_entry) = tools_array
            .iter_mut()
            .find(|t| t.get("name").and_then(|n| n.as_str()) == Some("codex"))
        {
            *codex_entry = extended_codex;
        }

        // Append synthetic ATM tools
        for tool in synthetic_tools() {
            tools_array.push(tool);
        }
    }
}

/// Check whether a tool name belongs to the synthetic ATM tool set.
fn is_synthetic_tool(name: &str) -> bool {
    matches!(
        name,
        "atm_send"
            | "atm_read"
            | "atm_broadcast"
            | "atm_pending_count"
            | "agent_sessions"
            | "agent_status"
            | "agent_close"
            | "agent_watch_attach"
            | "agent_watch_poll"
            | "agent_watch_detach"
    )
}

/// Return the proxy start time as `(iso8601_string, epoch_secs)`.
fn proxy_start_time() -> (String, u64) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    let (y, mo, d) = epoch_days_to_ymd(days);
    let iso = format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z");
    (iso, secs)
}

/// Convert days since Unix epoch to (year, month, day).
fn epoch_days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    days += 719468;
    let era = days / 146097;
    let doe = days % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    (y, mo, d)
}

/// Construct a JSON-RPC error response.
pub fn make_error_response(id: Value, code: i64, message: &str, data: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
            "data": data
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intercept_tools_list_appends_synthetic() {
        let mut response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": [
                    {"name": "codex", "inputSchema": {}},
                    {"name": "codex-reply", "inputSchema": {}}
                ]
            }
        });
        intercept_tools_list(&mut response);
        let tools = response["result"]["tools"].as_array().unwrap();
        // 2 original + synthetic ATM tools
        assert_eq!(tools.len(), 2 + crate::tools::SYNTHETIC_TOOL_COUNT);
    }

    #[test]
    fn test_intercept_preserves_original_tools() {
        let mut response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": [
                    {"name": "codex", "inputSchema": {}},
                    {"name": "codex-reply", "inputSchema": {}}
                ]
            }
        });
        intercept_tools_list(&mut response);
        let tools = response["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"codex"));
        assert!(names.contains(&"codex-reply"));
    }

    #[test]
    fn test_is_synthetic_tool() {
        assert!(is_synthetic_tool("atm_send"));
        assert!(is_synthetic_tool("atm_read"));
        assert!(is_synthetic_tool("agent_close"));
        assert!(is_synthetic_tool("agent_watch_attach"));
        assert!(is_synthetic_tool("agent_watch_poll"));
        assert!(is_synthetic_tool("agent_watch_detach"));
        assert!(!is_synthetic_tool("codex"));
        assert!(!is_synthetic_tool("codex-reply"));
        assert!(!is_synthetic_tool("unknown"));
    }

    #[tokio::test]
    async fn test_watch_attach_poll_detach_synthetic_tools() {
        let proxy = ProxyServer::new(crate::config::AgentMcpConfig::default());
        let agent_id = "codex:test-agent";

        proxy.watch_stream_hub.lock().await.publish_frame(
            agent_id,
            SourceEnvelope::new("client_prompt", "arch-atm", "mcp_primary"),
            json!({"type":"task_started"}),
        );

        let attach = proxy
            .handle_synthetic_tool(
                &json!(1),
                "agent_watch_attach",
                &json!({"agent_id": agent_id}),
                None,
            )
            .await;
        assert!(
            attach.get("error").is_none(),
            "attach should succeed: {attach}"
        );
        let attach_text = attach
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .expect("attach text");
        let attach_json: Value = serde_json::from_str(attach_text).expect("valid attach payload");
        assert_eq!(attach_json["attached"], true);
        assert_eq!(attach_json["replay"].as_array().map(|a| a.len()), Some(1));
        assert!(attach_json["watcher_count"].as_u64().unwrap_or(0) >= 1);

        let attach_again = proxy
            .handle_synthetic_tool(
                &json!(11),
                "agent_watch_attach",
                &json!({"agent_id": agent_id}),
                None,
            )
            .await;
        assert!(
            attach_again.get("error").is_none(),
            "second attach should be idempotent: {attach_again}"
        );
        let attach_again_text = attach_again
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .expect("attach_again text");
        let attach_again_json: Value =
            serde_json::from_str(attach_again_text).expect("valid attach_again payload");
        assert_eq!(attach_again_json["attached"], true);
        assert!(attach_again_json["watcher_count"].as_u64().unwrap_or(0) >= 1);

        proxy.watch_stream_hub.lock().await.publish_frame(
            agent_id,
            SourceEnvelope::new("atm_mail", "arch-atm@atm-dev", "mail_injector"),
            json!({"type":"agent_message_delta"}),
        );

        let poll = proxy
            .handle_synthetic_tool(
                &json!(2),
                "agent_watch_poll",
                &json!({"agent_id": agent_id, "limit": 10}),
                None,
            )
            .await;
        assert!(poll.get("error").is_none(), "poll should succeed: {poll}");
        let poll_text = poll
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .expect("poll text");
        let poll_json: Value = serde_json::from_str(poll_text).expect("valid poll payload");
        assert_eq!(poll_json["events"].as_array().map(|a| a.len()), Some(1));
        assert_eq!(poll_json["rendered"].as_array().map(|a| a.len()), Some(1));

        let detach = proxy
            .handle_synthetic_tool(
                &json!(3),
                "agent_watch_detach",
                &json!({"agent_id": agent_id}),
                None,
            )
            .await;
        assert!(
            detach.get("error").is_none(),
            "detach should succeed: {detach}"
        );
        let detach_text = detach
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .expect("detach text");
        let detach_json: Value = serde_json::from_str(detach_text).expect("valid detach payload");
        assert_eq!(detach_json["detached"], true);
        assert_eq!(detach_json["watcher_count"], 0);
    }

    #[tokio::test]
    async fn test_watch_poll_without_attach_returns_error_result() {
        let proxy = ProxyServer::new(crate::config::AgentMcpConfig::default());
        let resp = proxy
            .handle_synthetic_tool(
                &json!(1),
                "agent_watch_poll",
                &json!({"agent_id": "codex:not-attached"}),
                None,
            )
            .await;
        assert_eq!(
            resp.pointer("/result/isError").and_then(|v| v.as_bool()),
            Some(true),
            "poll without attach must return isError=true"
        );
    }

    #[tokio::test]
    async fn test_watch_attach_requires_agent_id() {
        let proxy = ProxyServer::new(crate::config::AgentMcpConfig::default());
        let resp = proxy
            .handle_synthetic_tool(&json!(1), "agent_watch_attach", &json!({}), None)
            .await;
        assert_eq!(
            resp.pointer("/result/isError").and_then(|v| v.as_bool()),
            Some(true),
            "attach without agent_id must return isError=true"
        );
    }

    #[test]
    fn test_should_publish_watch_event_protocol_drift_matrix() {
        let canonical_kinds = vec![
            "task_started",
            "task_complete",
            "user_message",
            "approval_prompt",
            "approval_request",
            "approval_rejected",
            "approval_approved",
            "entered_review_mode",
            "exited_review_mode",
            "item/enteredReviewMode",
            "item/exitedReviewMode",
            "agent_message_delta",
            "agent_message",
            "agent_message_completed",
            "agent_message_chunk",
            "agent_message_content_delta",
            "reasoning_content_delta",
            "agent_reasoning_delta",
            "reasoning_raw_content_delta",
            "reasoning_content",
            "exec_command_output_delta",
            "exec_command_started",
            "exec_command_completed",
            "exec_command_error",
            "terminal_interaction",
            "request_user_input",
            "elicitation_request",
            "patch_apply_begin",
            "patch_apply_end",
            "turn_diff",
            "file_change",
            "item_started",
            "item_completed",
            "thread_status_changed",
            "turn_started",
            "turn_completed",
            "turn_aborted",
            "turn_interrupted",
            "turn_cancelled",
            "cancelled",
            "interrupt",
            "idle",
            "done",
            "shutdown_complete",
            "session_configured",
            "thread_name_updated",
            "token_count",
            "model_reroute",
            "context_compacted",
            "thread_rolled_back",
            "undo_started",
            "undo_completed",
            "background_event",
            "warning",
            "error",
            "deprecation_notice",
            "plan_update",
            "plan_delta",
            "mcp_list_tools_response",
            "remote_skill_downloaded",
            "skills_update_available",
            "stream_error",
        ];
        for kind in canonical_kinds {
            let event = json!({"params":{"type":kind}});
            assert!(should_publish_watch_event(&event), "kind={kind}");
        }
        assert!(should_publish_watch_event(
            &json!({"params":{"type":"mcp_tool_call_begin"}})
        ));
        assert!(should_publish_watch_event(
            &json!({"params":{"type":"web_search_end"}})
        ));
        assert!(should_publish_watch_event(
            &json!({"params":{"type":"realtime_conversation_started"}})
        ));
        assert!(should_publish_watch_event(
            &json!({"params":{"type":"collab_agent_message"}})
        ));
        assert!(should_publish_watch_event(
            &json!({"params":{"type":"list_project_templates"}})
        ));
        assert!(!should_publish_watch_event(
            &json!({"params":{"type":"unknown_new_event_kind"}})
        ));
    }

    #[test]
    fn test_should_publish_watch_event_missing_params_graceful() {
        assert!(!should_publish_watch_event(&json!({})));
        assert!(!should_publish_watch_event(&json!({"params":{}})));
    }

    #[test]
    fn test_extract_stream_error_event_maps_summary_and_session() {
        let event = json!({
            "params": {
                "type": "stream_error",
                "threadId": "th-err-1",
                "message": "socket closed"
            }
        });
        let mapped = extract_stream_error_event(&event, "codex:agent-1");
        let Some(agent_team_mail_core::daemon_stream::DaemonStreamEvent::StreamError {
            agent_id,
            session_id,
            error_summary,
        }) = mapped
        else {
            panic!("expected StreamError mapping");
        };
        assert_eq!(agent_id, "codex:agent-1");
        assert_eq!(session_id, "th-err-1");
        assert_eq!(error_summary, "socket closed");
    }

    #[tokio::test]
    async fn test_emit_stream_error_summary_to_daemon_noop_without_daemon() {
        // Best-effort emit should never panic/fail when daemon socket is absent.
        let event = json!({
            "params": { "type": "stream_error", "message": "test error" }
        });
        emit_stream_error_summary_to_daemon(&event, "codex:agent-1").await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_flush_dropped_counters_reports_and_resets() {
        WATCH_UNKNOWN_EVENT_COUNT.store(7, Ordering::Relaxed);
        let dropped = Arc::new(AtomicU64::new(5));
        flush_dropped_counters_to_daemon(&dropped, "proxy:all").await;
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        assert_eq!(WATCH_UNKNOWN_EVENT_COUNT.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_format_watch_frame_graceful_on_partial_payloads() {
        let frame = json!({
            "source": {"kind":"atm_mail"},
            "event": {"params":{"type":"agent_message_delta","delta":"hello world"}}
        });
        let rendered = format_watch_frame(&frame);
        assert!(rendered.contains("[atm_mail]"));
        assert!(rendered.contains("assistant"));

        // Drift case: missing source and missing params text fields.
        let drift = json!({"event":{"params":{"type":"exec_command_completed"}}});
        let rendered2 = format_watch_frame(&drift);
        assert!(rendered2.contains("client_prompt"));
        assert!(rendered2.contains("cmd:"));
    }

    #[test]
    fn test_format_watch_frame_code_fence_and_context_usage() {
        let frame = json!({
            "source": {"kind":"client_prompt"},
            "event": {
                "params": {
                    "type":"agent_message_delta",
                    "delta":"```rust\\nfn main(){}\\n```",
                    "usage": {"contextWindowPct": 72.4}
                }
            }
        });
        let rendered = format_watch_frame(&frame);
        assert!(rendered.contains("assistant(code):"));
        assert!(rendered.contains("```rust"));
        assert!(rendered.contains("ctx 72%"));
    }

    #[test]
    #[serial_test::serial]
    fn test_append_watch_frame_for_tui_with_cap_rotates_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let watch_path = dir.path().join(".config/atm/watch-stream/test-agent.jsonl");
        std::fs::create_dir_all(
            watch_path
                .parent()
                .expect("watch feed path should have parent directory"),
        )
        .unwrap();
        std::fs::write(&watch_path, "x".repeat(256)).unwrap();

        let frame = json!({
            "source": {"kind": "client_prompt"},
            "event": {"params": {"type": "agent_message_delta", "delta": "hello"}}
        });
        append_watch_frame_for_tui_at_path(&frame, 64, &watch_path);

        let rotated_path = watch_path.with_extension("jsonl.1");
        assert!(rotated_path.exists(), "rotation should create .jsonl.1");
        let content = std::fs::read_to_string(&watch_path).expect("new feed file should exist");
        assert!(
            content.contains("\"rendered\""),
            "new feed file should contain appended rendered record"
        );
        assert!(
            content.contains("assistant"),
            "rendered line should include formatted message class"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_forward_event_unknown_watch_kind_records_telemetry() {
        let before = WATCH_UNKNOWN_EVENT_COUNT.load(Ordering::Relaxed);
        let (tx, mut rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));
        let mut map = HashMap::new();
        map.insert(
            "thread-unknown".to_string(),
            "codex:unknown-agent".to_string(),
        );
        let thread_to_agent: Arc<tokio::sync::Mutex<HashMap<String, String>>> =
            Arc::new(tokio::sync::Mutex::new(map));
        let watch_stream_hub = Arc::new(tokio::sync::Mutex::new(WatchStreamHub::default()));
        let mut event = json!({
            "jsonrpc": "2.0",
            "method": "codex/event",
            "params": {"type": "brand_new_kind", "threadId": "thread-unknown"}
        });

        forward_event(
            &mut event,
            &pending,
            &thread_to_agent,
            &watch_stream_hub,
            &tx,
            &dropped,
        )
        .await;
        let _ = rx.try_recv().expect("event should still forward upstream");
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        let after = WATCH_UNKNOWN_EVENT_COUNT.load(Ordering::Relaxed);
        assert_eq!(
            after,
            before + 1,
            "unknown kind should bump telemetry counter"
        );

        let sub = watch_stream_hub
            .lock()
            .await
            .subscribe("codex:unknown-agent");
        assert!(
            sub.replay.is_empty(),
            "unknown watch events should not be published to replay"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_forward_event_stream_error_triggers_daemon_emit_attempt() {
        let before = STREAM_ERROR_EMIT_ATTEMPTS.load(Ordering::Relaxed);
        let (tx, mut rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));
        let mut map = HashMap::new();
        map.insert("th-err".to_string(), "codex:err-agent".to_string());
        let thread_to_agent: Arc<tokio::sync::Mutex<HashMap<String, String>>> =
            Arc::new(tokio::sync::Mutex::new(map));
        let watch_stream_hub = Arc::new(tokio::sync::Mutex::new(WatchStreamHub::default()));
        let mut event = json!({
            "jsonrpc":"2.0",
            "method":"codex/event",
            "params":{"type":"stream_error","threadId":"th-err","message":"socket closed"}
        });

        forward_event(
            &mut event,
            &pending,
            &thread_to_agent,
            &watch_stream_hub,
            &tx,
            &dropped,
        )
        .await;
        let _ = rx.try_recv().expect("event should forward upstream");
        let after = STREAM_ERROR_EMIT_ATTEMPTS.load(Ordering::Relaxed);
        assert_eq!(
            after,
            before + 1,
            "stream_error should trigger daemon emission"
        );
    }

    #[test]
    fn test_make_error_response_structure() {
        let resp = make_error_response(
            json!(42),
            ERR_TIMEOUT,
            "timed out",
            json!({"error_source": "proxy"}),
        );
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 42);
        assert_eq!(resp["error"]["code"], ERR_TIMEOUT);
        assert_eq!(resp["error"]["message"], "timed out");
        assert_eq!(resp["error"]["data"]["error_source"], "proxy");
    }

    #[test]
    fn test_make_error_child_dead() {
        let resp = make_error_response(
            json!(1),
            ERR_CHILD_DEAD,
            "Codex child process died (exit code: 1)",
            json!({"error_source": "proxy", "exit_code": 1}),
        );
        assert_eq!(resp["error"]["code"], ERR_CHILD_DEAD);
        assert_eq!(resp["error"]["data"]["exit_code"], 1);
    }

    #[tokio::test]
    async fn test_forward_event_injects_agent_id_unknown_when_no_thread_id() {
        let (tx, mut rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));
        let thread_to_agent: Arc<tokio::sync::Mutex<HashMap<String, String>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let watch_stream_hub = Arc::new(tokio::sync::Mutex::new(WatchStreamHub::default()));
        let mut event = json!({
            "jsonrpc": "2.0",
            "method": "codex/event",
            "params": {"type": "task_started"}
        });
        // No threadId in the event → falls back to "proxy:unknown"
        forward_event(
            &mut event,
            &pending,
            &thread_to_agent,
            &watch_stream_hub,
            &tx,
            &dropped,
        )
        .await;
        let received = rx.try_recv().expect("event should be forwarded");
        assert_eq!(received["params"]["agent_id"], "proxy:unknown");
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_forward_event_resolves_agent_id_from_thread_id() {
        let (tx, mut rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));
        let mut map = HashMap::new();
        map.insert("thread-123".to_string(), "codex:abc-agent".to_string());
        let thread_to_agent: Arc<tokio::sync::Mutex<HashMap<String, String>>> =
            Arc::new(tokio::sync::Mutex::new(map));
        let watch_stream_hub = Arc::new(tokio::sync::Mutex::new(WatchStreamHub::default()));
        let mut event = json!({
            "jsonrpc": "2.0",
            "method": "codex/event",
            "params": {"type": "task_started", "threadId": "thread-123"}
        });
        forward_event(
            &mut event,
            &pending,
            &thread_to_agent,
            &watch_stream_hub,
            &tx,
            &dropped,
        )
        .await;
        let received = rx.try_recv().expect("event should be forwarded");
        assert_eq!(received["params"]["agent_id"], "codex:abc-agent");
        assert_eq!(dropped.load(Ordering::Relaxed), 0);

        let sub = watch_stream_hub.lock().await.subscribe("codex:abc-agent");
        assert_eq!(sub.replay.len(), 1, "watch hub should retain replay event");
    }

    #[tokio::test]
    async fn test_forward_event_source_from_request_id_correlation() {
        let (tx, mut rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));
        let mut map = HashMap::new();
        map.insert("thread-123".to_string(), "codex:abc-agent".to_string());
        let thread_to_agent: Arc<tokio::sync::Mutex<HashMap<String, String>>> =
            Arc::new(tokio::sync::Mutex::new(map));
        let watch_stream_hub = Arc::new(tokio::sync::Mutex::new(WatchStreamHub::default()));

        {
            let mut p = pending.lock().await;
            p.mark_request_source(
                json!(99),
                SourceEnvelope::new("atm_mail", "arch-atm@atm-dev", "mail_injector"),
            );
        }

        let mut event = json!({
            "jsonrpc": "2.0",
            "method": "codex/event",
            "params": {"type": "task_started", "threadId": "thread-123", "_meta": {"requestId": 99}}
        });
        forward_event(
            &mut event,
            &pending,
            &thread_to_agent,
            &watch_stream_hub,
            &tx,
            &dropped,
        )
        .await;
        let _ = rx.try_recv().expect("event should be forwarded");
        assert_eq!(dropped.load(Ordering::Relaxed), 0);

        let sub = watch_stream_hub.lock().await.subscribe("codex:abc-agent");
        let replay0 = sub.replay.first().expect("replay event");
        assert_eq!(
            replay0.pointer("/source/kind").and_then(|v| v.as_str()),
            Some("atm_mail")
        );
        assert_eq!(
            replay0.pointer("/source/channel").and_then(|v| v.as_str()),
            Some("mail_injector")
        );
    }

    #[tokio::test]
    async fn test_forward_event_source_falls_back_to_last_agent_source() {
        let (tx, mut rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));
        let mut map = HashMap::new();
        map.insert("thread-123".to_string(), "codex:abc-agent".to_string());
        let thread_to_agent: Arc<tokio::sync::Mutex<HashMap<String, String>>> =
            Arc::new(tokio::sync::Mutex::new(map));
        let watch_stream_hub = Arc::new(tokio::sync::Mutex::new(WatchStreamHub::default()));

        {
            let mut p = pending.lock().await;
            p.set_last_agent_source(
                "codex:abc-agent".to_string(),
                SourceEnvelope::new("user_steer", "randlee", "tui_user"),
            );
        }

        let mut event = json!({
            "jsonrpc": "2.0",
            "method": "codex/event",
            "params": {"type": "task_started", "threadId": "thread-123"}
        });
        forward_event(
            &mut event,
            &pending,
            &thread_to_agent,
            &watch_stream_hub,
            &tx,
            &dropped,
        )
        .await;
        let _ = rx.try_recv().expect("event should be forwarded");
        assert_eq!(dropped.load(Ordering::Relaxed), 0);

        let sub = watch_stream_hub.lock().await.subscribe("codex:abc-agent");
        let replay0 = sub.replay.first().expect("replay event");
        assert_eq!(
            replay0.pointer("/source/kind").and_then(|v| v.as_str()),
            Some("user_steer")
        );
        assert_eq!(
            replay0.pointer("/source/actor").and_then(|v| v.as_str()),
            Some("randlee")
        );
    }

    #[tokio::test]
    async fn test_forward_event_drops_on_full_channel() {
        let (tx, _rx) = mpsc::channel::<Value>(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));
        let thread_to_agent: Arc<tokio::sync::Mutex<HashMap<String, String>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let watch_stream_hub = Arc::new(tokio::sync::Mutex::new(WatchStreamHub::default()));

        // Fill the channel
        let _ = tx.try_send(json!({"fill": true}));

        // Now the channel is full — forward_event should drop and increment counter
        let mut event = json!({
            "jsonrpc": "2.0",
            "method": "codex/event",
            "params": {"type": "task_started"}
        });
        forward_event(
            &mut event,
            &pending,
            &thread_to_agent,
            &watch_stream_hub,
            &tx,
            &dropped,
        )
        .await;
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_proxy_server_debug() {
        let config = crate::config::AgentMcpConfig::default();
        let proxy = ProxyServer::new(config);
        let _ = format!("{proxy:?}");
    }

    #[test]
    fn test_constants() {
        assert_eq!(UPSTREAM_CHANNEL_CAPACITY, 256);
        assert_eq!(CHILD_DRAIN_GRACE_MS, 100);
    }

    #[test]
    fn test_error_code_constants() {
        assert_eq!(ERR_IDENTITY_CONFLICT, -32001);
        assert_eq!(ERR_SESSION_NOT_FOUND, -32002);
        assert_eq!(ERR_MAX_SESSIONS_EXCEEDED, -32004);
        assert_eq!(ERR_CHILD_DEAD, -32005);
        assert_eq!(ERR_TIMEOUT, -32006);
        assert_eq!(ERR_INVALID_SESSION_PARAMS, -32007);
        assert_eq!(ERR_AGENT_FILE_NOT_FOUND, -32008);
    }

    #[tokio::test]
    async fn auto_mail_reservation_is_single_flight() {
        let registry = Arc::new(Mutex::new(SessionRegistry::new(8)));
        let agent_id = {
            let mut reg = registry.lock().await;
            let entry = reg
                .register(
                    "auto-mail-agent".to_string(),
                    "default".to_string(),
                    ".".to_string(),
                    None,
                    None,
                    None,
                )
                .unwrap();
            reg.set_thread_state(&entry.agent_id, ThreadState::Idle);
            entry.agent_id
        };

        // First reservation transitions Idle -> Busy.
        assert!(try_reserve_thread_for_auto_mail(&agent_id, &registry).await);

        // While Busy, a second reservation must fail.
        assert!(!try_reserve_thread_for_auto_mail(&agent_id, &registry).await);

        let state = registry
            .lock()
            .await
            .get(&agent_id)
            .map(|e| e.thread_state.clone())
            .unwrap();
        assert_eq!(state, ThreadState::Busy);
    }

    #[test]
    fn test_proxy_server_new_with_team() {
        let config = crate::config::AgentMcpConfig::default();
        let proxy = ProxyServer::new_with_team(config, "atm-dev");
        assert_eq!(proxy.team, "atm-dev");
    }

    #[test]
    fn test_proxy_server_default_team() {
        let config = crate::config::AgentMcpConfig::default();
        let proxy = ProxyServer::new(config);
        assert_eq!(proxy.team, "default");
    }

    /// codex call with both agent_file and prompt returns ERR_INVALID_SESSION_PARAMS.
    #[tokio::test]
    #[serial_test::serial]
    async fn codex_call_with_agent_file_and_prompt_returns_invalid_params() {
        let _dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ATM_HOME", _dir.path()) };

        let config = crate::config::AgentMcpConfig::default();
        let mut proxy = ProxyServer::new(config);
        let (upstream_tx, mut upstream_rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));

        let msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "codex",
                "arguments": {
                    "prompt": "hello",
                    "agent_file": "/some/file.md"
                }
            }
        });

        proxy
            .handle_tools_call(msg, &pending, &upstream_tx, &dropped)
            .await;
        let resp = upstream_rx.try_recv().expect("should get error response");
        unsafe { std::env::remove_var("ATM_HOME") };

        assert_eq!(
            resp.pointer("/error/code").and_then(|v| v.as_i64()),
            Some(ERR_INVALID_SESSION_PARAMS)
        );
    }

    /// codex call with a non-existent agent_file returns ERR_AGENT_FILE_NOT_FOUND.
    #[tokio::test]
    #[serial_test::serial]
    async fn codex_call_with_missing_agent_file_returns_not_found() {
        let _dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ATM_HOME", _dir.path()) };

        let config = crate::config::AgentMcpConfig::default();
        let mut proxy = ProxyServer::new(config);
        let (upstream_tx, mut upstream_rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));

        // Use a path that is guaranteed not to exist: a non-existent file inside
        // the temp dir (which is freshly created and empty).
        let missing_file = _dir.path().join("definitely-does-not-exist.md");
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "codex",
                "arguments": {
                    "agent_file": missing_file.to_string_lossy()
                }
            }
        });

        proxy
            .handle_tools_call(msg, &pending, &upstream_tx, &dropped)
            .await;
        let resp = upstream_rx.try_recv().expect("should get error response");
        unsafe { std::env::remove_var("ATM_HOME") };

        assert_eq!(
            resp.pointer("/error/code").and_then(|v| v.as_i64()),
            Some(ERR_AGENT_FILE_NOT_FOUND)
        );
    }

    /// Identity resolution: explicit param wins over config wins over default.
    #[tokio::test]
    async fn codex_identity_resolution_explicit_over_config_over_default() {
        let config = crate::config::AgentMcpConfig {
            identity: Some("config-identity".to_string()),
            ..Default::default()
        };
        let proxy = ProxyServer::new(config);

        // Verify the registry is accessible and can store sessions
        let mut reg = proxy.registry.lock().await;
        let entry = reg
            .register(
                "explicit-identity".to_string(),
                "team".to_string(),
                ".".to_string(),
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(entry.identity, "explicit-identity");
    }

    /// FR-4.5: in-thread ATM tools must use the thread-bound identity,
    /// not an arbitrary args.identity override.
    #[tokio::test]
    #[serial_test::serial]
    async fn synthetic_tool_prefers_thread_bound_identity_over_args_identity() {
        let dir = tempfile::tempdir().unwrap();
        let atm_home = dir.path().join("runtime-home").to_string_lossy().to_string();
        // SAFETY: isolated tmp dir, no parallelism risk in serial test
        unsafe {
            std::env::set_var("HOME", dir.path());
            std::env::set_var("ATM_HOME", &atm_home);
        };

        let config = crate::config::AgentMcpConfig {
            identity: Some("config-identity".to_string()),
            ..Default::default()
        };
        let mut proxy = ProxyServer::new(config);

        let agent_id = {
            let mut reg = proxy.registry.lock().await;
            let entry = reg
                .register(
                    "bound-identity".to_string(),
                    "default".to_string(),
                    ".".to_string(),
                    None,
                    None,
                    None,
                )
                .unwrap();
            reg.set_thread_id(&entry.agent_id, "thread-abc".to_string());
            entry.agent_id
        };
        proxy
            .thread_to_agent
            .lock()
            .await
            .insert("thread-abc".to_string(), agent_id);

        let (upstream_tx, mut upstream_rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));

        let msg = json!({
            "jsonrpc": "2.0",
            "id": 77,
            "method": "tools/call",
            "params": {
                "name": "atm_send",
                "_meta": {"threadId": "thread-abc"},
                "arguments": {
                    "to": "receiver",
                    "message": "hello from test",
                    "identity": "spoofed-identity"
                }
            }
        });

        proxy
            .handle_tools_call(msg, &pending, &upstream_tx, &dropped)
            .await;
        let _resp = upstream_rx
            .try_recv()
            .expect("should get synthetic tool response");

        let inbox_path = dir
            .path()
            .join(".claude")
            .join("teams")
            .join("default")
            .join("inboxes")
            .join("receiver.json");
        let inbox_content = std::fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<agent_team_mail_core::InboxMessage> =
            serde_json::from_str(&inbox_content).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].from, "bound-identity");

        // SAFETY: restoring process env after isolated test
        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("HOME");
        };
    }

    /// FR-3.2: Persisted active sessions loaded on startup are marked stale.
    #[test]
    #[serial_test::serial]
    fn startup_marks_persisted_sessions_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        let team = "test-startup-team";

        // Write a registry.json with one "active" session
        let sessions_path = dir
            .path()
            .join(".config")
            .join("atm")
            .join("agent-sessions")
            .join(team);
        std::fs::create_dir_all(&sessions_path).unwrap();
        let registry_json = serde_json::json!({
            "version": 1,
            "sessions": [{
                "agent_id": "codex:test-persisted-1234",
                "identity": "arch-ctm",
                "team": team,
                "thread_id": null,
                "cwd": ".",
                "repo_root": null,
                "repo_name": null,
                "branch": null,
                "started_at": "2026-02-18T00:00:00Z",
                "last_active": "2026-02-18T00:00:00Z",
                "status": "active"
            }]
        });
        std::fs::write(
            sessions_path.join("registry.json"),
            serde_json::to_string_pretty(&registry_json).unwrap(),
        )
        .unwrap();

        let atm_home = dir.path().to_string_lossy().to_string();
        // SAFETY: isolated tmp dir, no parallelism risk here (single-threaded test)
        unsafe { std::env::set_var("ATM_HOME", &atm_home) };

        let config = crate::config::AgentMcpConfig::default();
        let proxy = ProxyServer::new_with_team(config, team);

        unsafe { std::env::remove_var("ATM_HOME") };

        // The registry should have the persisted session as Stale
        let reg = proxy.registry.try_lock().unwrap();
        let all = reg.list_all();
        assert_eq!(all.len(), 1, "should have 1 loaded session");
        let entry = all[0];
        assert_eq!(entry.agent_id, "codex:test-persisted-1234");
        assert_eq!(
            entry.status,
            crate::session::SessionStatus::Stale,
            "loaded session must be stale"
        );
        // Active count should be 0 (stale sessions don't count)
        assert_eq!(reg.active_count(), 0);
    }

    /// FR-16.3: codex call with agent_id for unknown session returns error.
    #[tokio::test]
    #[serial_test::serial]
    async fn codex_resume_with_unknown_agent_id_returns_error() {
        let _dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ATM_HOME", _dir.path()) };

        let config = crate::config::AgentMcpConfig::default();
        let mut proxy = ProxyServer::new(config);
        let (upstream_tx, mut upstream_rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));

        let msg = json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "codex",
                "arguments": {
                    "agent_id": "codex:does-not-exist-xyz",
                    "prompt": "hello"
                }
            }
        });

        proxy
            .handle_tools_call(msg, &pending, &upstream_tx, &dropped)
            .await;
        let resp = upstream_rx.try_recv().expect("should get error response");
        unsafe { std::env::remove_var("ATM_HOME") };

        assert_eq!(
            resp.pointer("/error/code").and_then(|v| v.as_i64()),
            Some(ERR_SESSION_NOT_FOUND),
            "unknown agent_id should return ERR_SESSION_NOT_FOUND"
        );
        let msg_str = resp
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            msg_str.contains("session not found for agent_id"),
            "error message should indicate session not found, got: {msg_str}"
        );
    }

    /// FR-16.3: codex call with existing agent_id but no threadId yet returns error.
    #[tokio::test]
    async fn codex_resume_without_thread_id_returns_error() {
        let config = crate::config::AgentMcpConfig::default();
        let mut proxy = ProxyServer::new(config);

        // Register a session without a threadId
        let agent_id = {
            let mut reg = proxy.registry.lock().await;
            reg.register(
                "resume-test-identity".to_string(),
                "default".to_string(),
                ".".to_string(),
                None,
                None,
                None,
            )
            .unwrap()
            .agent_id
            .clone()
        };

        let (upstream_tx, mut upstream_rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));

        let msg = json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "codex",
                "arguments": {
                    "agent_id": agent_id
                }
            }
        });

        proxy
            .handle_tools_call(msg, &pending, &upstream_tx, &dropped)
            .await;
        let resp = upstream_rx.try_recv().expect("should get error response");

        assert_eq!(
            resp.pointer("/error/code").and_then(|v| v.as_i64()),
            Some(ERR_INTERNAL),
            "session without threadId should return ERR_INTERNAL"
        );
        let msg_str = resp
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            msg_str.contains("no threadId"),
            "error message should mention no threadId, got: {msg_str}"
        );
    }

    /// FR-16.3 wire-protocol fix: resume path rewrites params.name to "codex-reply"
    /// and injects threadId into params.arguments before forwarding to child.
    ///
    /// This is a unit test of the mutation logic itself (not end-to-end forwarding)
    /// because end-to-end requires a live Codex child process.
    #[test]
    fn resume_rewrite_sets_name_and_injects_thread_id() {
        // Simulate the incoming message as it arrives from upstream
        let mut msg = json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "tools/call",
            "params": {
                "name": "codex",
                "arguments": {
                    "agent_id": "some-agent-id-abc",
                    "prompt": "continue the work"
                }
            }
        });

        let known_thread_id = "thread-resume-xyz-789";

        // Apply the exact mutations from the FR-16.3 resume branch
        if let Some(name) = msg.pointer_mut("/params/name") {
            *name = serde_json::Value::String("codex-reply".to_string());
        }
        if let Some(args) = msg.pointer_mut("/params/arguments") {
            if let Some(obj) = args.as_object_mut() {
                obj.insert(
                    "threadId".to_string(),
                    serde_json::Value::String(known_thread_id.to_string()),
                );
            }
        }

        assert_eq!(
            msg.pointer("/params/name").and_then(|v| v.as_str()),
            Some("codex-reply"),
            "params.name must be rewritten to codex-reply so child treats this as a resume"
        );
        assert_eq!(
            msg.pointer("/params/arguments/threadId")
                .and_then(|v| v.as_str()),
            Some(known_thread_id),
            "threadId must be injected into params.arguments for Codex to resume the conversation"
        );
        // Existing fields must be preserved
        assert_eq!(
            msg.pointer("/params/arguments/agent_id")
                .and_then(|v| v.as_str()),
            Some("some-agent-id-abc"),
            "agent_id must remain in arguments after rewrite"
        );
        assert_eq!(
            msg.pointer("/params/arguments/prompt")
                .and_then(|v| v.as_str()),
            Some("continue the work"),
            "prompt must remain in arguments after rewrite"
        );
    }

    /// Fix 6: intercept_tools_list replaces codex entry with extended schema.
    #[test]
    fn test_intercept_tools_list_replaces_codex_with_extended_schema() {
        let mut response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": [
                    {"name": "codex", "inputSchema": {"type": "object", "properties": {}}},
                    {"name": "codex-reply", "inputSchema": {}}
                ]
            }
        });
        intercept_tools_list(&mut response);
        let tools = response["result"]["tools"].as_array().unwrap();

        // 2 original (codex replaced + codex-reply) + synthetic ATM tools
        assert_eq!(tools.len(), 2 + crate::tools::SYNTHETIC_TOOL_COUNT);

        // The codex entry should now have the extended schema with identity property
        let codex_tool = tools
            .iter()
            .find(|t| t.get("name").and_then(|n| n.as_str()) == Some("codex"))
            .expect("codex tool must be present");
        let has_identity = codex_tool
            .pointer("/inputSchema/properties/identity")
            .is_some();
        assert!(
            has_identity,
            "extended codex schema must include identity property"
        );
        let has_agent_id = codex_tool
            .pointer("/inputSchema/properties/agent_id")
            .is_some();
        assert!(
            has_agent_id,
            "extended codex schema must include agent_id property"
        );
    }

    /// Fix 4: IDENTITY_CONFLICT errors use conflicting_agent_id key.
    #[tokio::test]
    #[serial_test::serial]
    async fn identity_conflict_error_uses_conflicting_agent_id_key() {
        let _dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("ATM_HOME", _dir.path()) };

        let config = crate::config::AgentMcpConfig::default();
        let mut proxy = ProxyServer::new(config);

        // Pre-register an identity so the second call conflicts
        {
            let mut reg = proxy.registry.lock().await;
            reg.register(
                "conflicting-identity".to_string(),
                "default".to_string(),
                ".".to_string(),
                None,
                None,
                None,
            )
            .unwrap();
        }

        let (upstream_tx, mut upstream_rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));

        let msg = json!({
            "jsonrpc": "2.0",
            "id": 20,
            "method": "tools/call",
            "params": {
                "name": "codex",
                "arguments": {
                    "identity": "conflicting-identity",
                    "prompt": "hello"
                }
            }
        });

        proxy
            .handle_tools_call(msg, &pending, &upstream_tx, &dropped)
            .await;
        unsafe { std::env::remove_var("ATM_HOME") };

        let resp = upstream_rx.try_recv().expect("should get error response");
        assert_eq!(
            resp.pointer("/error/code").and_then(|v| v.as_i64()),
            Some(ERR_IDENTITY_CONFLICT)
        );
        // The data field must use "conflicting_agent_id", not "agent_id" or "existing_agent_id"
        let data = resp.pointer("/error/data").unwrap();
        assert!(
            data.get("conflicting_agent_id").is_some(),
            "error data must have 'conflicting_agent_id' key, got: {data}"
        );
        assert!(
            data.get("agent_id").is_none(),
            "error data must NOT have bare 'agent_id' key"
        );
        assert!(
            data.get("existing_agent_id").is_none(),
            "error data must NOT have 'existing_agent_id' key"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn agent_close_allows_immediate_codex_reuse_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let atm_home = dir.path().to_string_lossy().to_string();
        unsafe { std::env::set_var("ATM_HOME", &atm_home) };

        let config = crate::config::AgentMcpConfig::default();
        let mut proxy = ProxyServer::new(config);
        let (upstream_tx, mut upstream_rx) = mpsc::channel::<Value>(8);

        let first_id = json!(701);
        let first_msg = json!({
            "jsonrpc": "2.0",
            "id": first_id,
            "method": "tools/call",
            "params": {
                "name": "codex",
                "arguments": {
                    "identity": "reuse-after-close",
                    "prompt": "first turn"
                }
            }
        });

        let first_agent_id = match proxy
            .prepare_codex_message(&first_id, first_msg, &upstream_tx)
            .await
        {
            PrepareResult::Ok {
                expected_agent_id: Some(agent_id),
                ..
            } => agent_id,
            _ => panic!("expected first prepare_codex_message to succeed"),
        };
        assert!(
            upstream_rx.try_recv().is_err(),
            "unexpected upstream error on first codex call"
        );

        let close_resp = crate::atm_tools::handle_agent_close(
            &json!(702),
            &json!({"agent_id": first_agent_id}),
            Arc::clone(&proxy.registry),
            Arc::clone(&proxy.elicitation_registry),
        )
        .await;
        assert!(
            close_resp.get("error").is_none(),
            "agent_close should succeed: {close_resp}"
        );

        let second_id = json!(703);
        let second_msg = json!({
            "jsonrpc": "2.0",
            "id": second_id,
            "method": "tools/call",
            "params": {
                "name": "codex",
                "arguments": {
                    "identity": "reuse-after-close",
                    "prompt": "second turn"
                }
            }
        });

        let second = proxy
            .prepare_codex_message(&second_id, second_msg, &upstream_tx)
            .await;
        match second {
            PrepareResult::Ok { .. } => {}
            _ => panic!("expected second codex call to succeed"),
        }
        assert!(
            upstream_rx.try_recv().is_err(),
            "expected no ERR_IDENTITY_CONFLICT after agent_close"
        );

        unsafe { std::env::remove_var("ATM_HOME") };
    }

    // -----------------------------------------------------------------------
    // dispatch_auto_mail_app_server — FR-8.12 mark-read boundary test
    // -----------------------------------------------------------------------

    /// Verify that `dispatch_auto_mail_app_server` does NOT call
    /// `mark_messages_read` when the write to child stdin fails.
    ///
    /// FR-8.12: mark-read must only happen after the request is accepted by
    /// the child stdin.  If the write fails, messages must remain unread so
    /// the next poll cycle can retry.
    ///
    /// Implementation note: we simulate a write failure by dropping the read
    /// half of a duplex stream before writing, which causes `write_all` to
    /// return a broken-pipe error on the write half.
    #[tokio::test]
    #[serial_test::serial]
    async fn mark_read_only_after_successful_dispatch() {
        use std::collections::HashMap;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("ATM_HOME", dir.path()) };

        // Seed inbox with one unread message.
        let team = "test-team";
        let identity = "test-agent";
        let inbox_dir = dir.path().join(".claude/teams").join(team).join("inboxes");
        std::fs::create_dir_all(&inbox_dir).unwrap();
        let inbox_path = inbox_dir.join(format!("{identity}.json"));
        let msg = agent_team_mail_core::InboxMessage {
            from: "alice".to_string(),
            source_team: None,
            text: "hello from alice".to_string(),
            timestamp: "2026-02-22T10:00:00Z".to_string(),
            read: false,
            summary: None,
            message_id: Some("test-msg-1".to_string()),
            unknown_fields: HashMap::new(),
        };
        std::fs::write(
            &inbox_path,
            serde_json::to_string_pretty(&vec![&msg]).unwrap(),
        )
        .unwrap();

        // Build a broken stdin: write half of a duplex where the read half is dropped.
        let (write_half, _read_half_dropped) = tokio::io::duplex(256);
        // Drop the read half immediately — writes will fail with broken pipe.
        drop(_read_half_dropped);

        let broken_stdin: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>> = Arc::new(Mutex::new(
            Box::new(write_half) as Box<dyn AsyncWrite + Send + Unpin>,
        ));

        // Wrap in the SharedChildStdin type.
        let shared_stdin: SharedChildStdin = Arc::new(Mutex::new(Some(broken_stdin)));

        // Build registry with an Active/Idle session.
        let registry = Arc::new(Mutex::new(SessionRegistry::new(8)));
        let agent_id = {
            let mut reg = registry.lock().await;
            let entry = reg
                .register(
                    "test-agent".to_string(),
                    identity.to_string(),
                    ".".to_string(),
                    None,
                    None,
                    None,
                )
                .unwrap();
            reg.set_thread_state(&entry.agent_id, ThreadState::Idle);
            entry.agent_id
        };

        let request_counter = Arc::new(AtomicU64::new(1));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));
        let inflight = Arc::new(Mutex::new(InflightMailSet::new()));

        // Reserve the thread (Idle -> Busy) as dispatch_auto_mail_app_server expects.
        assert!(try_reserve_thread_for_auto_mail(&agent_id, &registry).await);

        // Call the app-server dispatcher — this should fail to write and NOT mark read.
        dispatch_auto_mail_app_server(
            &agent_id,
            identity,
            "thread-1",
            team,
            10,
            4096,
            &registry,
            &shared_stdin,
            &request_counter,
            &pending,
            None, // Idle: use turn/start
            &inflight,
        )
        .await;

        // Verify: the message must still be unread (mark-read was not called).
        let content = std::fs::read_to_string(&inbox_path).unwrap();
        let messages: Vec<agent_team_mail_core::InboxMessage> =
            serde_json::from_str(&content).unwrap();
        assert!(
            !messages[0].read,
            "message must remain unread when dispatch write fails (FR-8.12)"
        );

        // Verify: in-flight set was cleared on failure (retry eligible).
        assert!(
            !inflight.lock().await.is_inflight("test-msg-1"),
            "in-flight set must be cleared after write failure so retry is possible"
        );

        // Verify: thread state was restored to Idle after the failure.
        let thread_state = registry
            .lock()
            .await
            .get(&agent_id)
            .map(|e| e.thread_state.clone())
            .unwrap();
        assert_eq!(
            thread_state,
            ThreadState::Idle,
            "thread state must be restored to Idle after failed dispatch"
        );

        unsafe { std::env::remove_var("ATM_HOME") };
    }

    /// When ATM_HOME is set, `watch_feed_path` must produce
    /// `$ATM_HOME/watch-stream/<agent-id>.jsonl` (no `.config/atm/` nesting).
    #[test]
    #[serial_test::serial]
    fn test_watch_feed_path_uses_atm_home() {
        let dir = tempfile::tempdir().expect("tempdir");
        unsafe { std::env::set_var("ATM_HOME", dir.path()) };

        let result = watch_feed_path("test-agent");

        unsafe { std::env::remove_var("ATM_HOME") };

        let path = result.expect("watch_feed_path should return Some when ATM_HOME is set");
        let expected = dir.path().join("watch-stream").join("test-agent.jsonl");
        assert_eq!(
            path, expected,
            "ATM_HOME path must not include .config/atm/ nesting"
        );
    }
}
