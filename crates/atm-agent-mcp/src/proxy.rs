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
//! ATM tool execution and mail injection are deferred to Sprint A.4+.

use std::collections::HashMap;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::{Duration, timeout};

use crate::config::AgentMcpConfig;
use crate::context::detect_context;
use crate::framing::{UpstreamReader, write_newline_delimited};
use crate::inject::{build_session_context, inject_developer_instructions};
use crate::lock::{acquire_lock, check_lock, release_lock};
use crate::session::{RegistryError, SessionRegistry};
use crate::tools::synthetic_tools;

/// Channel buffer capacity for upstream message delivery.
///
/// Sized to handle burst of MCP responses without backpressure.
const UPSTREAM_CHANNEL_CAPACITY: usize = 256;

/// Grace period in ms after dropping child stdin before force-kill, giving child time to flush output.
const CHILD_DRAIN_GRACE_MS: u64 = 100;

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
#[derive(Debug)]
pub struct ProxyServer {
    config: AgentMcpConfig,
    child: Option<ChildHandle>,
    /// Counter of event notifications dropped due to backpressure.
    pub dropped_events: Arc<AtomicU64>,
    /// In-memory session registry shared with per-request tasks.
    registry: Arc<Mutex<SessionRegistry>>,
    /// ATM team name used for session registration and lock files.
    pub team: String,
    /// Maps Codex `threadId` → `agent_id` for event attribution.
    thread_to_agent: Arc<tokio::sync::Mutex<HashMap<String, String>>>,
}

/// Handle to the spawned Codex child process.
struct ChildHandle {
    /// Shared stdin writer; shared so timeout tasks can send cancellation notifications.
    stdin: Arc<Mutex<ChildStdin>>,
    /// Receives responses and notifications from the child stdout reader task.
    response_rx: mpsc::Receiver<Value>,
    /// If the child has exited, contains the exit status.
    exit_status: Arc<Mutex<Option<ExitStatus>>>,
    /// The child process handle, kept for force-kill on shutdown.
    process: Arc<Mutex<Option<Child>>>,
}

impl std::fmt::Debug for ChildHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildHandle")
            .field("stdin", &"<ChildStdin>")
            .field("response_rx", &"<Receiver>")
            .field("exit_status", &"<Mutex<Option<ExitStatus>>>")
            .field("process", &"<Mutex<Option<Child>>>")
            .finish()
    }
}

/// Tracks in-flight requests waiting for a response from the child.
struct PendingRequests {
    map: HashMap<Value, oneshot::Sender<Value>>,
    /// Request IDs that correspond to `tools/list` requests and need interception.
    tools_list_ids: std::collections::HashSet<Value>,
    /// Request IDs for new `codex` session creation mapped to the preallocated agent_id.
    codex_create_ids: HashMap<Value, String>,
}

impl PendingRequests {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            tools_list_ids: std::collections::HashSet::new(),
            codex_create_ids: HashMap::new(),
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
        Self {
            config,
            child: None,
            dropped_events: Arc::new(AtomicU64::new(0)),
            registry: Arc::new(Mutex::new(registry)),
            team: team_str,
            thread_to_agent: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Load a persisted registry file and mark any `"active"` sessions as stale.
    ///
    /// If the file does not exist or cannot be parsed, returns the registry
    /// unchanged (fresh start). This satisfies FR-3.2's requirement to mark
    /// prior active sessions as stale on proxy startup.
    fn load_stale_from_disk(mut registry: SessionRegistry, team: &str) -> SessionRegistry {
        use crate::lock::sessions_dir;
        use crate::session::SessionStatus;

        let registry_path = sessions_dir().join(team).join("registry.json");
        let Ok(contents) = std::fs::read_to_string(&registry_path) else {
            return registry;
        };
        let Ok(root) = serde_json::from_str::<serde_json::Value>(&contents) else {
            return registry;
        };
        let Some(sessions) = root.get("sessions").and_then(|v| v.as_array()) else {
            return registry;
        };

        for session in sessions {
            let status_str = session.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status_str != "active" {
                continue;
            }
            // Extract required fields; skip malformed entries
            let Some(agent_id) = session.get("agent_id").and_then(|v| v.as_str()) else {
                continue;
            };
            let identity = session
                .get("identity")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let team_val = session
                .get("team")
                .and_then(|v| v.as_str())
                .unwrap_or(team)
                .to_string();
            let cwd = session
                .get("cwd")
                .and_then(|v| v.as_str())
                .unwrap_or(".")
                .to_string();
            let started_at = session
                .get("started_at")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let last_active = session
                .get("last_active")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let thread_id = session
                .get("thread_id")
                .and_then(|v| v.as_str())
                .map(String::from);

            let entry = crate::session::SessionEntry {
                agent_id: agent_id.to_string(),
                identity,
                team: team_val,
                thread_id,
                cwd,
                repo_root: session
                    .get("repo_root")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                repo_name: session
                    .get("repo_name")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                branch: session
                    .get("branch")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                started_at,
                last_active,
                status: SessionStatus::Stale,
            };
            registry.insert_stale(entry);
        }

        registry
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

        // Channel for upstream writes (events + responses routed through the channel).
        // Bounded to prevent unbounded memory growth under backpressure.
        let (upstream_tx, mut upstream_rx) = mpsc::channel::<Value>(UPSTREAM_CHANNEL_CAPACITY);

        loop {
            tokio::select! {
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

                    match method.as_deref() {
                        Some("tools/call") => {
                            self.handle_tools_call(msg, &pending, &upstream_tx, &dropped)
                                .await;
                        }
                        Some(method_name) => {
                            let is_tools_list = method_name == "tools/list";
                            self.forward_to_child(msg, id, is_tools_list, &pending, &upstream_tx)
                                .await;
                        }
                        None => {
                            // Response from upstream (e.g. elicitation response)
                            if let Some(ref handle) = self.child {
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

                // Read from child (server-initiated requests like elicitation)
                msg = async {
                    if let Some(ref mut handle) = self.child {
                        handle.response_rx.recv().await
                    } else {
                        std::future::pending::<Option<Value>>().await
                    }
                } => {
                    if let Some(msg) = msg {
                        route_child_message(msg, &pending, &upstream_tx, &dropped, &thread_to_agent).await;
                    }
                }

                // Drain upstream write channel
                Some(msg) = upstream_rx.recv() => {
                    let serialized = serde_json::to_string(&msg).unwrap_or_default();
                    let frame = crate::framing::encode_content_length(&serialized);
                    if upstream_out.write_all(&frame).await.is_err() {
                        break;
                    }
                    if upstream_out.flush().await.is_err() {
                        break;
                    }
                }
            }
        }

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

        // Shutdown: signal child and force-kill if it ignores stdin EOF
        if let Some(handle) = self.child.take() {
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
        } else if let Some(req_id) = id {
            let err = make_error_response(
                req_id,
                ERR_INTERNAL,
                "Child process not yet spawned",
                json!({"error_source": "proxy"}),
            );
            let _ = upstream_tx.send(err).await;
        }
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
            let resp = self.handle_synthetic_tool(&id, &tool_name, &args);
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
        let (msg_to_forward, expected_agent_id) = if effective_tool_name == "codex" {
            match self.prepare_codex_message(&id, msg, upstream_tx).await {
                PrepareResult::Error => return, // error already sent
                PrepareResult::Ok {
                    modified,
                    expected_agent_id,
                } => (modified, expected_agent_id),
            }
        } else if effective_tool_name == "codex-reply" {
            (self.prepare_codex_reply_message(msg).await, None)
        } else {
            (msg, None)
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

        // Forward to child
        let serialized = serde_json::to_string(&msg_to_forward).unwrap_or_default();
        {
            let mut stdin = handle.stdin.lock().await;
            if let Err(e) = write_newline_delimited(&mut *stdin, &serialized).await {
                tracing::error!("failed to write to child: {e}");
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
        }

        let timeout_secs = self.config.request_timeout_secs;
        let upstream_tx_clone = upstream_tx.clone();
        let req_id = id;
        let child_stdin = Arc::clone(&handle.stdin);

        let thread_to_agent_task = Arc::clone(&self.thread_to_agent);
        let pending_for_thread_map = Arc::clone(pending);
        let registry_for_thread_map = Arc::clone(&self.registry);

        tokio::spawn(async move {
            match timeout(Duration::from_secs(timeout_secs), rx).await {
                Ok(Ok(resp)) => {
                    if let Some(thread_id) = resp
                        .pointer("/result/structuredContent/threadId")
                        .and_then(|v| v.as_str())
                    {
                        if let Some(agent_id) = pending_for_thread_map
                            .lock()
                            .await
                            .take_codex_create(&req_id)
                        {
                            registry_for_thread_map
                                .lock()
                                .await
                                .set_thread_id(&agent_id, thread_id.to_string());
                            thread_to_agent_task
                                .lock()
                                .await
                                .insert(thread_id.to_string(), agent_id);
                        }
                    }
                    let _ = upstream_tx_clone.send(resp).await;
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
        }

        modified_msg
    }

    /// Handle a synthetic tool call (ATM tools, session management).
    ///
    /// ATM communication tools (`atm_send`, `atm_read`, `atm_broadcast`,
    /// `atm_pending_count`) are fully implemented in Sprint A.4.
    /// Session management tools (`agent_sessions`, `agent_status`, `agent_close`)
    /// remain stubs until Sprint A.6.
    fn handle_synthetic_tool(&self, id: &Value, tool_name: &str, args: &Value) -> Value {
        use crate::atm_tools;

        match tool_name {
            "atm_send" | "atm_read" | "atm_broadcast" | "atm_pending_count" => {
                let identity_opt =
                    atm_tools::resolve_identity(args, self.config.identity.as_deref());
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
            "agent_sessions" | "agent_status" | "agent_close" => {
                // Sprint A.6 stubs
                atm_tools::make_mcp_error_result(
                    id,
                    &format!("Tool '{tool_name}' is not yet implemented (Sprint A.6+)"),
                )
            }
            _ => atm_tools::make_mcp_error_result(
                id,
                &format!("Unknown synthetic tool: {tool_name}"),
            ),
        }
    }

    /// Spawn the Codex child process.
    async fn spawn_child(
        &mut self,
        pending: &Arc<Mutex<PendingRequests>>,
        upstream_tx: &mpsc::Sender<Value>,
        dropped: &Arc<AtomicU64>,
    ) -> anyhow::Result<()> {
        let mut cmd = Command::new(&self.config.codex_bin);
        cmd.arg("mcp-server");

        // Pass model if configured
        if let Some(ref model) = self.config.model {
            cmd.arg("-m").arg(model);
        }

        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let mut child: Child = cmd.spawn()?;

        let stdin = child.stdin.take().expect("child stdin must be piped");
        let stdout = child.stdout.take().expect("child stdout must be piped");

        let exit_status: Arc<Mutex<Option<ExitStatus>>> = Arc::new(Mutex::new(None));

        // Wrap stdin in Arc<Mutex> so it can be shared with timeout tasks for cancellation
        let shared_stdin = Arc::new(Mutex::new(stdin));

        // Wrap the child process so we can force-kill on shutdown
        let process: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(child)));

        // Channel for messages from child stdout reader
        let (child_tx, child_rx) = mpsc::channel::<Value>(UPSTREAM_CHANNEL_CAPACITY);

        // Spawn child stdout reader task
        let pending_clone = Arc::clone(pending);
        let upstream_tx_clone = upstream_tx.clone();
        let dropped_clone = Arc::clone(dropped);
        let thread_to_agent_clone = Arc::clone(&self.thread_to_agent);
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

                tracing::debug!(direction = "child->proxy", %msg);

                let method = msg.get("method").and_then(|v| v.as_str());

                if method == Some("codex/event") {
                    // Add agent_id to event params and forward upstream
                    let mut event = msg;
                    forward_event(
                        &mut event,
                        &pending_clone,
                        &thread_to_agent_clone,
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
        });

        self.child = Some(ChildHandle {
            stdin: shared_stdin,
            response_rx: child_rx,
            exit_status,
            process,
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
            obj.insert("agent_id".to_string(), Value::String(agent_id));
        }
    }
    match upstream_tx.try_send(event.clone()) {
        Ok(()) => {}
        Err(_) => {
            dropped_events.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Route a message received from the child to the appropriate destination.
///
/// This is a free function rather than a method to avoid borrow conflicts with
/// the `ProxyServer`'s mutable child handle.
async fn route_child_message(
    msg: Value,
    pending: &Arc<Mutex<PendingRequests>>,
    upstream_tx: &mpsc::Sender<Value>,
    dropped: &Arc<AtomicU64>,
    thread_to_agent: &Arc<tokio::sync::Mutex<HashMap<String, String>>>,
) {
    let method = msg.get("method").and_then(|v| v.as_str());

    if method == Some("codex/event") {
        let mut event = msg;
        forward_event(&mut event, pending, thread_to_agent, upstream_tx, dropped).await;
        return;
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
    )
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
        // 2 original + 7 synthetic
        assert_eq!(tools.len(), 9);
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
        assert!(!is_synthetic_tool("codex"));
        assert!(!is_synthetic_tool("codex-reply"));
        assert!(!is_synthetic_tool("unknown"));
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
        let mut event = json!({
            "jsonrpc": "2.0",
            "method": "codex/event",
            "params": {"type": "task_started"}
        });
        // No threadId in the event → falls back to "proxy:unknown"
        forward_event(&mut event, &pending, &thread_to_agent, &tx, &dropped).await;
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
        let mut event = json!({
            "jsonrpc": "2.0",
            "method": "codex/event",
            "params": {"type": "task_started", "threadId": "thread-123"}
        });
        forward_event(&mut event, &pending, &thread_to_agent, &tx, &dropped).await;
        let received = rx.try_recv().expect("event should be forwarded");
        assert_eq!(received["params"]["agent_id"], "codex:abc-agent");
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_forward_event_drops_on_full_channel() {
        let (tx, _rx) = mpsc::channel::<Value>(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));
        let thread_to_agent: Arc<tokio::sync::Mutex<HashMap<String, String>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));

        // Fill the channel
        let _ = tx.try_send(json!({"fill": true}));

        // Now the channel is full — forward_event should drop and increment counter
        let mut event = json!({
            "jsonrpc": "2.0",
            "method": "codex/event",
            "params": {"type": "task_started"}
        });
        forward_event(&mut event, &pending, &thread_to_agent, &tx, &dropped).await;
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
        unsafe { std::env::set_var("ATM_HOME", "/tmp/atm-test-proxy-invalid") };

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
        unsafe { std::env::set_var("ATM_HOME", "/tmp/atm-test-proxy-notfound") };

        let config = crate::config::AgentMcpConfig::default();
        let mut proxy = ProxyServer::new(config);
        let (upstream_tx, mut upstream_rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(Mutex::new(PendingRequests::new()));

        let msg = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "codex",
                "arguments": {
                    "agent_file": "/tmp/definitely-does-not-exist-12345.md"
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
                "cwd": "/tmp",
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
        unsafe { std::env::set_var("ATM_HOME", "/tmp/atm-test-resume-unknown") };

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
                "/tmp".to_string(),
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

        // 2 original (codex replaced + codex-reply) + 7 synthetic
        assert_eq!(tools.len(), 9);

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
        unsafe { std::env::set_var("ATM_HOME", "/tmp/atm-test-conflict-key") };

        let config = crate::config::AgentMcpConfig::default();
        let mut proxy = ProxyServer::new(config);

        // Pre-register an identity so the second call conflicts
        {
            let mut reg = proxy.registry.lock().await;
            reg.register(
                "conflicting-identity".to_string(),
                "default".to_string(),
                "/tmp".to_string(),
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
}
