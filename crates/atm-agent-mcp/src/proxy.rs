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
//!
//! This module implements the Sprint A.2 proxy core. Identity binding, session
//! registry, and ATM tool execution are deferred to later sprints.

use std::collections::HashMap;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::{Duration, timeout};

use crate::config::AgentMcpConfig;
use crate::framing::{UpstreamReader, write_newline_delimited};
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

/// Manages the MCP proxy lifecycle: upstream I/O, child process, and message routing.
#[derive(Debug)]
pub struct ProxyServer {
    config: AgentMcpConfig,
    child: Option<ChildHandle>,
    /// Counter of event notifications dropped due to backpressure.
    pub dropped_events: Arc<AtomicU64>,
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
}

impl PendingRequests {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            tools_list_ids: std::collections::HashSet::new(),
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
}

impl ProxyServer {
    /// Create a new proxy server with the given configuration.
    pub fn new(config: AgentMcpConfig) -> Self {
        Self {
            config,
            child: None,
            dropped_events: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Run the proxy loop, reading from `upstream_in` and writing to `upstream_out`.
    ///
    /// This is the main entry point. It blocks until upstream EOF or a fatal error.
    ///
    /// # Errors
    ///
    /// Returns an error on unrecoverable I/O failures. Transient errors (child crash,
    /// timeout) are reported as JSON-RPC error responses to the upstream client.
    pub async fn run<R, W>(
        &mut self,
        upstream_in: R,
        mut upstream_out: W,
    ) -> anyhow::Result<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let mut reader = UpstreamReader::new(upstream_in);
        let pending = Arc::new(Mutex::new(PendingRequests::new()));
        let dropped = Arc::clone(&self.dropped_events);

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
                        route_child_message(msg, &pending, &upstream_tx, &dropped).await;
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
        msg: Value,
        pending: &Arc<Mutex<PendingRequests>>,
        upstream_tx: &mpsc::Sender<Value>,
        dropped: &Arc<AtomicU64>,
    ) {
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let tool_name = msg
            .pointer("/params/name")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Check if this is a synthetic ATM tool call
        if is_synthetic_tool(tool_name) {
            let resp = self.handle_synthetic_tool(&id, tool_name);
            let _ = upstream_tx.send(resp).await;
            return;
        }

        let is_codex_tool =
            tool_name == "codex" || tool_name == "codex-reply";

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

        let Some(ref handle) = self.child else {
            let err = make_error_response(
                id.clone(),
                ERR_INTERNAL,
                "Child process not available",
                json!({"error_source": "proxy"}),
            );
            let _ = upstream_tx.send(err).await;
            return;
        };

        // Forward to child
        let serialized = serde_json::to_string(&msg).unwrap_or_default();
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
        pending.lock().await.insert(id.clone(), tx);

        let timeout_secs = self.config.request_timeout_secs;
        let upstream_tx_clone = upstream_tx.clone();
        let req_id = id;
        // Clone the shared stdin so the timeout task can send a cancellation notification
        let child_stdin = Arc::clone(&handle.stdin);

        tokio::spawn(async move {
            match timeout(Duration::from_secs(timeout_secs), rx).await {
                Ok(Ok(resp)) => {
                    let _ = upstream_tx_clone.send(resp).await;
                }
                Ok(Err(_)) => {
                    // Sender dropped (child died)
                    tracing::debug!("pending request canceled (child died)");
                }
                Err(_elapsed) => {
                    tracing::warn!("request timed out after {timeout_secs}s");
                    // Best-effort: send notifications/cancelled to child (FR-14.2).
                    // Ignore any error — child may already be dead.
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

    /// Handle a synthetic tool call (ATM tools, session management).
    ///
    /// For Sprint A.2 these return stub "not implemented" errors.
    fn handle_synthetic_tool(&self, id: &Value, tool_name: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{
                    "type": "text",
                    "text": format!("Tool '{tool_name}' is not yet implemented (Sprint A.4+)")
                }],
                "isError": true
            }
        })
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
                    forward_event(&mut event, &upstream_tx_clone, &dropped_clone);
                    continue;
                }

                // Check if this is a response (has id, has result or error, no method)
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

                // For server-initiated requests from child (e.g. elicitation/create),
                // forward to upstream
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
            // Take the Child out of the Arc so we can call .wait() on it
            let child_opt = process_clone.lock().await.take();
            if let Some(mut child) = child_opt {
                match child.wait().await {
                    Ok(s) => {
                        tracing::info!("child process exited: {s}");
                        *exit_clone.lock().await = Some(s);
                    }
                    Err(e) => {
                        tracing::error!("error waiting for child: {e}");
                    }
                }
            }
            // Cancel all pending requests
            let mut guard = pending_crash.lock().await;
            // Drop all senders to notify waiters
            guard.map.clear();
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

/// Forward a `codex/event` notification upstream, injecting `agent_id` into params.
///
/// This is a best-effort send: if the upstream channel is full the event is dropped
/// and the `dropped_events` counter is incremented.
fn forward_event(
    event: &mut Value,
    upstream_tx: &mpsc::Sender<Value>,
    dropped_events: &Arc<AtomicU64>,
) {
    if let Some(params) = event.get_mut("params") {
        if let Some(obj) = params.as_object_mut() {
            obj.insert(
                "agent_id".to_string(),
                Value::String("proxy:default".to_string()),
            );
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
) {
    let method = msg.get("method").and_then(|v| v.as_str());

    if method == Some("codex/event") {
        let mut event = msg;
        forward_event(&mut event, upstream_tx, dropped);
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

/// Intercept a `tools/list` response to append synthetic ATM tools.
///
/// This is called on responses from the child that match a `tools/list` request.
/// The function mutates the response in-place, appending synthetic tools to
/// `result.tools`.
pub fn intercept_tools_list(response: &mut Value) {
    if let Some(tools_array) = response
        .pointer_mut("/result/tools")
        .and_then(|v| v.as_array_mut())
    {
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

    #[test]
    fn test_forward_event_injects_agent_id() {
        let (tx, mut rx) = mpsc::channel::<Value>(8);
        let dropped = Arc::new(AtomicU64::new(0));
        let mut event = json!({
            "jsonrpc": "2.0",
            "method": "codex/event",
            "params": {"type": "task_started"}
        });
        forward_event(&mut event, &tx, &dropped);
        let received = rx.try_recv().expect("event should be forwarded");
        assert_eq!(received["params"]["agent_id"], "proxy:default");
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_forward_event_drops_on_full_channel() {
        // Channel capacity of 0 is not valid; use capacity 1 and fill it first
        let (tx, _rx) = mpsc::channel::<Value>(1);
        let dropped = Arc::new(AtomicU64::new(0));

        // Fill the channel
        let filler = json!({"fill": true});
        let _ = tx.try_send(filler);

        // Now the channel is full — forward_event should drop and increment counter
        let mut event = json!({
            "jsonrpc": "2.0",
            "method": "codex/event",
            "params": {"type": "task_started"}
        });
        forward_event(&mut event, &tx, &dropped);
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_proxy_server_debug() {
        let config = crate::config::AgentMcpConfig::default();
        let proxy = ProxyServer::new(config);
        // Verify Debug is implemented (compile-time check; format output is not asserted)
        let _ = format!("{proxy:?}");
    }

    #[test]
    fn test_constants() {
        assert_eq!(UPSTREAM_CHANNEL_CAPACITY, 256);
        assert_eq!(CHILD_DRAIN_GRACE_MS, 100);
    }
}
