//! Transport abstraction for the Codex child process.
//!
//! [`CodexTransport`] is the trait seam between [`crate::proxy::ProxyServer`]
//! and the underlying child-process implementation.  Production code uses
//! [`McpTransport`] (spawns `codex mcp-server`) or [`JsonCodecTransport`]
//! (spawns `codex exec --json`).  [`MockTransport`] is an in-memory test
//! double for integration tests.
//!
//! # Design notes
//!
//! The trait splits the child lifecycle into two concerns:
//!
//! 1. **I/O creation** (`spawn`): starts the child process (or equivalent) and
//!    returns raw I/O handles.  This is what `CodexTransport` abstracts.
//! 2. **Background task wiring**: the proxy continues to own the reader loop
//!    and wait loop because they are deeply coupled with `PendingRequests` and
//!    the shared state.
//!
//! C.1 structured events are emitted on `transport_init` and
//! `transport_shutdown` via
//! [`agent_team_mail_core::event_log::emit_event_best_effort`].

use std::io;
use std::pin::Pin;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::Child;
use tokio::sync::Mutex;

use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};

use crate::config::AgentMcpConfig;

/// Raw I/O handles produced by a successful [`CodexTransport::spawn`] call.
///
/// The proxy converts this into its internal `ChildHandle` by wiring up the
/// background reader and wait tasks.  Keeping the two types distinct preserves
/// the existing proxy internals without change.
///
/// Both `stdin` and `stdout` use boxed trait objects so that non-process
/// transports (e.g. [`MockTransport`]) can provide in-memory implementations
/// without requiring a real child process.
pub struct RawChildIo {
    /// Shared stdin writer.  The proxy shares this with timeout tasks so they
    /// can send `notifications/cancelled` to the child.
    pub stdin: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    /// Raw stdout reader, consumed by the proxy's background reader task.
    pub stdout: Box<dyn AsyncRead + Send + Unpin>,
    /// Updated to `Some(status)` when the child process terminates.
    pub exit_status: Arc<Mutex<Option<ExitStatus>>>,
    /// The child process handle, retained for force-kill on proxy shutdown.
    /// `None` for transports that do not spawn a real child process.
    pub process: Arc<Mutex<Option<Child>>>,
    /// Idle flag shared with the transport.  Set to `true` when an `idle` JSONL
    /// event is detected on the child stdout.  Always `None` for [`McpTransport`]
    /// and [`MockTransport`].
    pub idle_flag: Option<Arc<AtomicBool>>,
}

/// Abstracts the mechanism by which the proxy communicates with a Codex agent.
///
/// Implement this trait to swap in alternative transports (e.g. a test double
/// or the [`JsonCodecTransport`] for JSON mode) without changing
/// [`crate::proxy::ProxyServer`].
///
/// The trait is object-safe via [`async_trait`], allowing `Box<dyn
/// CodexTransport>` to be stored in [`crate::proxy::ProxyServer`].
///
/// # Errors
///
/// `spawn` returns an error if the underlying child process (or equivalent)
/// cannot be started.
#[async_trait]
pub(crate) trait CodexTransport: Send + Sync + std::fmt::Debug {
    /// Spawn (or connect to) the Codex agent and return raw I/O handles.
    ///
    /// The returned [`RawChildIo`] is consumed by the proxy to wire up the
    /// background reader and wait tasks.
    async fn spawn(&self) -> anyhow::Result<RawChildIo>;

    /// Returns true if the transport's child process is currently in an idle state.
    ///
    /// For [`McpTransport`], always returns false (MCP protocol has no idle concept).
    /// For [`JsonCodecTransport`], returns true when the last JSONL event was `idle`.
    ///
    /// The proxy calls this to decide when to drain the stdin queue.
    /// [`JsonCodecTransport`] overrides this; all other transports inherit the
    /// default `false`.
    fn is_idle(&self) -> bool {
        false
    }
}

/// Transport that spawns `codex mcp-server` as a child subprocess.
///
/// This is the production transport for MCP mode.  It reproduces the exact
/// spawn logic that previously lived inline in `ProxyServer::spawn_child`.
#[derive(Debug)]
pub(crate) struct McpTransport {
    config: AgentMcpConfig,
    team: String,
}

impl McpTransport {
    /// Create a new `McpTransport` for the given config and team.
    ///
    /// Emits a `transport_init` structured log event.
    pub fn new(config: AgentMcpConfig, team: impl Into<String>) -> Self {
        let team = team.into();
        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm-agent-mcp",
            action: "transport_init",
            team: Some(team.clone()),
            result: Some("mcp".to_string()),
            ..Default::default()
        });
        Self { config, team }
    }
}

impl Drop for McpTransport {
    /// Emits a `transport_shutdown` structured log event when the transport
    /// is dropped (i.e. when the proxy shuts down).
    fn drop(&mut self) {
        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm-agent-mcp",
            action: "transport_shutdown",
            team: Some(self.team.clone()),
            result: Some("mcp".to_string()),
            ..Default::default()
        });
    }
}

#[async_trait]
impl CodexTransport for McpTransport {
    async fn spawn(&self) -> anyhow::Result<RawChildIo> {
        use tokio::process::Command;

        let mut cmd = Command::new(&self.config.codex_bin);
        cmd.arg("mcp-server");

        // Pass model if configured
        if let Some(ref model) = self.config.model {
            cmd.arg("-m").arg(model);
        }

        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let mut child = cmd.spawn()?;

        let stdin = child.stdin.take().expect("child stdin must be piped");
        let stdout = child.stdout.take().expect("child stdout must be piped");

        let exit_status: Arc<Mutex<Option<std::process::ExitStatus>>> =
            Arc::new(Mutex::new(None));
        let shared_stdin = Arc::new(Mutex::new(
            Box::new(stdin) as Box<dyn AsyncWrite + Send + Unpin>
        ));
        let process: Arc<Mutex<Option<tokio::process::Child>>> =
            Arc::new(Mutex::new(Some(child)));

        Ok(RawChildIo {
            stdin: shared_stdin,
            stdout: Box::new(stdout) as Box<dyn AsyncRead + Send + Unpin>,
            exit_status,
            process,
            idle_flag: None,
        })
    }
}

// ─── JsonCodecTransport ──────────────────────────────────────────────────────

/// Transport that spawns `codex exec --json` and communicates via JSONL event stream.
///
/// Spawns the codex binary with `--json` flag.  The child writes JSONL events
/// to its stdout (one JSON object per line).  The proxy writes tool result JSON
/// to child stdin to inject messages.
///
/// Child stdout is piped (NEVER inherited from parent process).
/// `atm-agent-mcp` uses its own stdout for JSON-RPC -- the codex child stdout
/// must not leak there.
///
/// The `idle_flag` is shared between the transport struct and the [`RawChildIo`]
/// it returns.  A background task monitors child stdout for `idle` JSONL events
/// and sets the flag when one is detected.
#[derive(Debug)]
pub(crate) struct JsonCodecTransport {
    config: AgentMcpConfig,
    team: String,
    /// Shared idle flag: set to `true` by background task when `idle` JSONL event seen.
    idle_flag: Arc<AtomicBool>,
}

impl JsonCodecTransport {
    /// Create a new `JsonCodecTransport` for the given config and team.
    ///
    /// Emits a `transport_init` structured log event.
    pub fn new(config: AgentMcpConfig, team: impl Into<String>) -> Self {
        let team = team.into();
        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm-agent-mcp",
            action: "transport_init",
            team: Some(team.clone()),
            result: Some("json".to_string()),
            ..Default::default()
        });
        Self {
            config,
            team,
            idle_flag: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Drop for JsonCodecTransport {
    fn drop(&mut self) {
        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm-agent-mcp",
            action: "transport_shutdown",
            team: Some(self.team.clone()),
            result: Some("json".to_string()),
            ..Default::default()
        });
    }
}

/// JSONL event type, as parsed by the `JsonCodecTransport` background reader.
#[derive(Debug)]
enum TransportEventType {
    AgentMessage,
    ToolCall,
    ToolResult,
    FileChange,
    Idle,
    Done,
    Unknown,
}

/// Parse the `type` field from a JSONL line into a [`TransportEventType`].
///
/// Returns [`TransportEventType::Unknown`] for unrecognised types or parse errors.
fn parse_event_type(line: &str) -> TransportEventType {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(|s| s.to_string()))
        .map(|t| match t.as_str() {
            "agent_message" => TransportEventType::AgentMessage,
            "tool_call" => TransportEventType::ToolCall,
            "tool_result" => TransportEventType::ToolResult,
            "file_change" => TransportEventType::FileChange,
            "idle" => TransportEventType::Idle,
            "done" => TransportEventType::Done,
            _ => TransportEventType::Unknown,
        })
        .unwrap_or(TransportEventType::Unknown)
}

#[async_trait]
impl CodexTransport for JsonCodecTransport {
    async fn spawn(&self) -> anyhow::Result<RawChildIo> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::process::Command;

        let mut cmd = Command::new(&self.config.codex_bin);
        cmd.arg("exec").arg("--json");

        // Pass model if configured
        if let Some(ref model) = self.config.model {
            cmd.arg("-m").arg(model);
        }

        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let mut child = cmd.spawn()?;

        let stdin = child.stdin.take().expect("child stdin must be piped");
        let child_stdout = child.stdout.take().expect("child stdout must be piped");

        // Create a duplex stream: the background task writes all child stdout
        // lines to `duplex_write`, and the proxy reads from `duplex_read`.
        let (mut duplex_write, duplex_read) = tokio::io::duplex(65_536);

        let idle_flag = Arc::clone(&self.idle_flag);
        let team_for_task = self.team.clone();

        // Background task: read lines from the real child stdout, detect idle/done
        // events, and forward everything to the duplex write half.
        tokio::spawn(async move {
            let reader = BufReader::new(child_stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                match parse_event_type(&line) {
                    TransportEventType::Idle => {
                        idle_flag.store(true, Ordering::SeqCst);
                        emit_event_best_effort(EventFields {
                            level: "info",
                            source: "atm-agent-mcp",
                            action: "idle_detected",
                            team: Some(team_for_task.clone()),
                            result: Some("json".to_string()),
                            ..Default::default()
                        });
                    }
                    TransportEventType::Done => {
                        emit_event_best_effort(EventFields {
                            level: "info",
                            source: "atm-agent-mcp",
                            action: "codex_done",
                            team: Some(team_for_task.clone()),
                            result: Some("json".to_string()),
                            ..Default::default()
                        });
                        // Reset idle flag on done (session complete)
                        idle_flag.store(false, Ordering::SeqCst);
                    }
                    _ => {
                        // Reset idle flag on any other event (agent is active)
                        idle_flag.store(false, Ordering::SeqCst);
                    }
                }

                // Forward the line (including idle/done lines) to the duplex stream
                let bytes = format!("{line}\n");
                if duplex_write.write_all(bytes.as_bytes()).await.is_err() {
                    break;
                }
            }
            // Child stdout closed — let the duplex half drop naturally.
        });

        let exit_status: Arc<Mutex<Option<ExitStatus>>> = Arc::new(Mutex::new(None));
        let shared_stdin = Arc::new(Mutex::new(
            Box::new(stdin) as Box<dyn AsyncWrite + Send + Unpin>
        ));
        let process: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(child)));

        Ok(RawChildIo {
            stdin: shared_stdin,
            stdout: Box::new(duplex_read) as Box<dyn AsyncRead + Send + Unpin>,
            exit_status,
            process,
            idle_flag: Some(Arc::clone(&self.idle_flag)),
        })
    }

    fn is_idle(&self) -> bool {
        self.idle_flag.load(Ordering::SeqCst)
    }
}

/// Select a transport based on the `transport` field in [`AgentMcpConfig`].
///
/// Recognised values:
/// - `None` / `"mcp"` -> [`McpTransport`] (spawns `codex mcp-server`).
/// - `"json"` -> [`JsonCodecTransport`] (spawns `codex exec --json`).
/// - `"mock"` -> [`MockTransport`] (in-memory channels; no child process).
///
/// Unknown values fall back to `McpTransport` with a `tracing::warn`.
///
/// Returns a `Box<dyn CodexTransport>` so callers can store the transport
/// without knowing the concrete type.
///
/// # Note on `MockTransport` handle
///
/// When `"mock"` is selected via this function, the [`MockTransportHandle`]
/// is discarded.  Tests and callers that need to inject responses or inspect
/// requests must construct [`MockTransport`] directly via
/// [`MockTransport::new_with_handle`].
pub(crate) fn make_transport(config: &AgentMcpConfig, team: &str) -> Box<dyn CodexTransport> {
    match config.transport.as_deref() {
        None | Some("mcp") => Box::new(McpTransport::new(config.clone(), team)),
        Some("json") => Box::new(JsonCodecTransport::new(config.clone(), team)),
        Some("mock") => {
            // MockTransport for testing/inspection. The handle is discarded here
            // because make_transport doesn't have a way to return it. Tests that
            // need the handle should construct MockTransport directly.
            let (transport, _handle) = MockTransport::new_with_handle();
            Box::new(transport)
        }
        Some(other) => {
            tracing::warn!(
                transport = %other,
                "unknown transport '{}'; falling back to McpTransport",
                other
            );
            Box::new(McpTransport::new(config.clone(), team))
        }
    }
}

// ─── MockTransport ───────────────────────────────────────────────────────────

/// A channel-based sender/receiver handle for [`MockTransport`].
///
/// Obtained by calling [`MockTransport::new_with_handle`].  Allows the test
/// harness (or any caller that does not go through [`make_transport`]) to:
///
/// - inject pre-scripted JSON-RPC lines that appear as "child stdout" by
///   sending on [`Self::response_tx`], and
/// - observe the JSON-RPC messages the proxy wrote to "child stdin" by
///   receiving on [`Self::request_rx`].
pub struct MockTransportHandle {
    /// Send pre-scripted JSON-RPC lines as "child stdout".
    pub response_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Receive the JSON-RPC messages the proxy sent to "child stdin".
    pub request_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
}

/// In-memory test double transport -- no child process is spawned.
///
/// # Stdout isolation
///
/// A `MockTransport` NEVER attaches to the parent process's stdout. The
/// "child stdout" is a tokio duplex read half, fully isolated from
/// `std::io::stdout()`. This is critical because `atm-agent-mcp` uses its
/// own stdout for JSON-RPC communication with upstream (Claude).
///
/// # Usage
///
/// Construct via [`MockTransport::new_with_handle`], which returns both the
/// transport and a [`MockTransportHandle`] for injecting/observing messages.
/// Pass the transport to [`crate::proxy::ProxyServer`] (or any
/// [`CodexTransport`] consumer); keep the handle to drive test assertions.
///
/// When `"mock"` transport is configured via [`make_transport`], the handle
/// is discarded -- construct directly when the handle is needed.
#[derive(Debug)]
pub struct MockTransport {
    /// Sender for pre-scripted responses (test harness injects via this).
    ///
    /// Retained to keep the channel alive even after the
    /// [`MockTransportHandle`]'s sender is dropped.  The background task in
    /// [`Self::spawn`] only receives EOF when all senders are gone.
    #[expect(
        dead_code,
        reason = "keepalive: prevents the response channel from closing if the \
                  MockTransportHandle is dropped before spawn's background task exits"
    )]
    response_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Receiver of pre-scripted responses; consumed by `spawn`'s background task.
    ///
    /// Wrapped in `Option` so `spawn` can `take` it exactly once.
    response_rx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>>,
    /// Sender for capturing proxy->child messages (stdin snoop).
    request_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// Receiver for capturing proxy->child messages; given to the caller handle.
    ///
    /// Wrapped in `Option` so the constructor can `take` it into the handle.
    request_rx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>>,
}

impl MockTransport {
    /// Create a new `MockTransport` and its associated [`MockTransportHandle`].
    ///
    /// The handle is the only way to inject responses or observe requests when
    /// not going through [`make_transport`].
    pub fn new_with_handle() -> (Self, MockTransportHandle) {
        let (response_tx, response_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let transport = Self {
            response_tx: response_tx.clone(),
            response_rx: Arc::new(Mutex::new(Some(response_rx))),
            request_tx,
            request_rx: Arc::new(Mutex::new(Some(request_rx))),
        };

        // Take the request_rx out of the Arc so it can be given to the handle.
        // This is the only place where request_rx is constructed, and we have
        // not yet shared the Arc with anyone, so try_lock succeeds.
        let req_rx = transport
            .request_rx
            .try_lock()
            .expect("freshly created Mutex cannot be contended")
            .take()
            .expect("Option is Some on construction");

        let handle = MockTransportHandle {
            response_tx,
            request_rx: req_rx,
        };

        (transport, handle)
    }

    /// Spawn the in-memory transport and return the raw I/O handles.
    ///
    /// This is a public convenience method that delegates to the
    /// [`CodexTransport`] trait implementation.  It exists so that integration
    /// tests (which are external to the crate and cannot call the
    /// `pub(crate)` trait method directly) can exercise the transport.
    ///
    /// # Errors
    ///
    /// Returns an error if `spawn` has already been called on this transport
    /// (the internal response receiver can only be taken once).
    pub async fn spawn(&self) -> anyhow::Result<RawChildIo> {
        <Self as CodexTransport>::spawn(self).await
    }
}

#[async_trait]
impl CodexTransport for MockTransport {
    async fn spawn(&self) -> anyhow::Result<RawChildIo> {
        use tokio::io::AsyncWriteExt as _;

        // Create a duplex stream for "child stdout".
        // The write half is driven by our response channel; the read half is
        // returned to the proxy's background reader task.
        let (mut stdout_write, stdout_read) = tokio::io::duplex(65_536);

        // Take the response receiver -- only valid to call spawn() once.
        let mut response_rx = self
            .response_rx
            .lock()
            .await
            .take()
            .ok_or_else(|| anyhow::anyhow!("MockTransport::spawn called more than once"))?;

        // Spawn background task: drain response channel -> write to stdout_write.
        tokio::spawn(async move {
            while let Some(line) = response_rx.recv().await {
                let bytes = format!("{line}\n");
                if stdout_write.write_all(bytes.as_bytes()).await.is_err() {
                    break;
                }
            }
            // EOF when channel closes -- let the duplex half drop naturally.
        });

        // Create a SniffWriter that captures everything the proxy writes to
        // "child stdin" and forwards it to the request channel.
        let stdin_capturer = SniffWriter::new(self.request_tx.clone());

        Ok(RawChildIo {
            stdin: Arc::new(Mutex::new(
                Box::new(stdin_capturer) as Box<dyn AsyncWrite + Send + Unpin>
            )),
            stdout: Box::new(stdout_read) as Box<dyn AsyncRead + Send + Unpin>,
            exit_status: Arc::new(Mutex::new(None)),
            process: Arc::new(Mutex::new(None)),
            idle_flag: None,
        })
    }
}

// ─── SniffWriter ──────────────────────────────────────────────────────────────

/// An [`AsyncWrite`] implementation that accumulates bytes, splits on `\n`,
/// and sends each complete JSON line to an unbounded channel.
///
/// Used internally by [`MockTransport`] to capture the JSON-RPC messages that
/// the proxy writes to "child stdin".
struct SniffWriter {
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    buf: Vec<u8>,
}

impl SniffWriter {
    fn new(tx: tokio::sync::mpsc::UnboundedSender<String>) -> Self {
        Self { tx, buf: Vec::new() }
    }
}

impl AsyncWrite for SniffWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.buf.extend_from_slice(buf);
        // Extract and forward complete newline-terminated lines.
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let s = String::from_utf8_lossy(&line).trim().to_string();
            if !s.is_empty() {
                // Best-effort: ignore send errors (receiver may have dropped).
                let _ = self.tx.send(s);
            }
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_transport_returns_mcp_for_none() {
        let config = AgentMcpConfig::default();
        // Should not panic; transport field is None by default.
        let _t = make_transport(&config, "default");
    }

    #[test]
    fn make_transport_returns_mcp_for_explicit_mcp() {
        let config = AgentMcpConfig {
            transport: Some("mcp".to_string()),
            ..Default::default()
        };
        let _t = make_transport(&config, "test-team");
    }

    #[test]
    fn make_transport_returns_json_codec_for_json() {
        let config = AgentMcpConfig {
            transport: Some("json".to_string()),
            ..Default::default()
        };
        let _t = make_transport(&config, "test-team");
    }

    #[test]
    fn make_transport_returns_mock_transport() {
        let config = AgentMcpConfig {
            transport: Some("mock".to_string()),
            ..Default::default()
        };
        let _t = make_transport(&config, "test-team");
    }

    #[test]
    fn make_transport_falls_back_for_unknown() {
        // Unknown transport values fall back to McpTransport without panic.
        let config = AgentMcpConfig {
            transport: Some("unknown-transport".to_string()),
            ..Default::default()
        };
        let _t = make_transport(&config, "test-team");
    }

    #[test]
    fn mock_transport_new_with_handle_does_not_panic() {
        let (_transport, _handle) = MockTransport::new_with_handle();
    }

    #[test]
    fn parse_event_type_detects_idle() {
        assert!(matches!(
            parse_event_type(r#"{"type":"idle"}"#),
            TransportEventType::Idle
        ));
        assert!(matches!(
            parse_event_type(r#"{"type": "idle", "extra": 42}"#),
            TransportEventType::Idle
        ));
    }

    #[test]
    fn parse_event_type_detects_done() {
        assert!(matches!(
            parse_event_type(r#"{"type":"done"}"#),
            TransportEventType::Done
        ));
    }

    #[test]
    fn parse_event_type_handles_all_known_types() {
        assert!(matches!(
            parse_event_type(r#"{"type":"agent_message"}"#),
            TransportEventType::AgentMessage
        ));
        assert!(matches!(
            parse_event_type(r#"{"type":"tool_call"}"#),
            TransportEventType::ToolCall
        ));
        assert!(matches!(
            parse_event_type(r#"{"type":"tool_result"}"#),
            TransportEventType::ToolResult
        ));
        assert!(matches!(
            parse_event_type(r#"{"type":"file_change"}"#),
            TransportEventType::FileChange
        ));
    }

    #[test]
    fn parse_event_type_returns_unknown_for_unrecognised() {
        assert!(matches!(
            parse_event_type(r#"{"type":"unknown_type"}"#),
            TransportEventType::Unknown
        ));
        assert!(matches!(
            parse_event_type(r#"{"foo":"bar"}"#),
            TransportEventType::Unknown
        ));
        assert!(matches!(
            parse_event_type("not json"),
            TransportEventType::Unknown
        ));
        assert!(matches!(
            parse_event_type(""),
            TransportEventType::Unknown
        ));
    }

    #[test]
    fn json_codec_transport_is_idle_default_false() {
        let t = JsonCodecTransport::new(AgentMcpConfig::default(), "test-team");
        assert!(!t.is_idle());
    }

    #[test]
    fn json_codec_transport_idle_flag_round_trip() {
        let t = JsonCodecTransport::new(AgentMcpConfig::default(), "test-team");
        assert!(!t.is_idle());
        t.idle_flag.store(true, Ordering::SeqCst);
        assert!(t.is_idle());
        t.idle_flag.store(false, Ordering::SeqCst);
        assert!(!t.is_idle());
    }
}
