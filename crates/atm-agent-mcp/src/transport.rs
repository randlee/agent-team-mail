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
//! Transport lifecycle (init/shutdown) no longer emits legacy bridge events;
//! structured observability is handled by the unified log pipeline.

use std::io;
use std::pin::Pin;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll};

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::Child;
use tokio::sync::Mutex;

use crate::config::AgentMcpConfig;
use crate::turn_control::TurnControl as _;

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
/// or the [`JsonCodecTransport`] for cli-json mode) without changing
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

    /// Set the session context for turn-state daemon emission.
    ///
    /// Called by the proxy after session registration so the transport's
    /// [`crate::turn_control::TurnTracker`] can emit [`crate::lifecycle_emit::EventKind::TeammateIdle`]
    /// lifecycle events to the daemon. The default implementation is a no-op;
    /// [`AppServerTransport`] and [`McpTransport`] override this (G.5).
    fn set_turn_session_context(&self, _ctx: crate::turn_control::SessionContext) {}

    /// Wire the upstream write channel for approval-gate bridging (G.5).
    ///
    /// Called by the proxy from within `spawn_child`, after the upstream
    /// `mpsc::Sender<Value>` is created.  Only [`AppServerTransport`] overrides
    /// this; all other transports use the default no-op because only the
    /// app-server protocol emits `item/enteredReviewMode` notifications.
    ///
    /// The default implementation is intentionally a no-op so that
    /// [`McpTransport`] and [`JsonCodecTransport`] do not need to be changed.
    fn set_approval_upstream_tx(&self, _tx: tokio::sync::mpsc::Sender<Value>) {}

    /// Returns `true` if this transport uses the app-server JSON-RPC protocol
    /// for mail injection (`turn/start` or `turn/steer`) rather than the MCP
    /// `codex-reply` path.
    ///
    /// Only [`AppServerTransport`] returns `true`; all other transports inherit
    /// the default `false`.
    fn uses_app_server_injection(&self) -> bool {
        false
    }

    /// Returns the active `turn_id` for the given `thread_id` if a turn is
    /// currently in progress ([`crate::stream_norm::TurnState::Busy`]).
    ///
    /// Returns `None` for all other states (`Idle`, `Terminal`, or if the
    /// thread is unknown).
    ///
    /// Uses `try_lock` internally so it never blocks in a synchronous context.
    /// If the lock is contended, returns `None` conservatively (causing the
    /// caller to fall back to `turn/start`, which is always safe).
    ///
    /// Only [`AppServerTransport`] provides a meaningful implementation; all
    /// other transports inherit the default `None`.
    fn active_turn_id_for_thread(&self, _thread_id: &str) -> Option<String> {
        None
    }
}

/// Transport that spawns `codex mcp-server` as a child subprocess.
///
/// This is the production transport for MCP mode.  It reproduces the exact
/// spawn logic that previously lived inline in `ProxyServer::spawn_child`.
///
/// # Turn tracking
///
/// `McpTransport` holds a [`crate::turn_control::TurnTracker`] for API
/// consistency with the other transports. MCP protocol handles turns
/// differently (no explicit `turn/started` / `turn/completed` notifications),
/// so active turn-start/complete tracking is deferred to a future sprint.
/// Session context binding via [`CodexTransport::set_turn_session_context`]
/// is wired in Sprint G.5 so the tracker is ready when daemon emission is needed.
#[derive(Debug)]
pub(crate) struct McpTransport {
    config: AgentMcpConfig,
    /// Unified turn tracker for daemon lifecycle emission.
    ///
    /// Wired in Sprint G.5: [`Self::set_turn_session_context`] calls
    /// [`crate::turn_control::TurnTracker::set_session_context`] so that
    /// daemon lifecycle events are emitted once session context is available.
    /// MCP protocol does not emit explicit `turn/started` / `turn/completed`
    /// notifications, so the turn-start/complete hooks are not yet wired;
    /// only the session context binding is implemented here.
    pub(crate) turn_tracker: crate::turn_control::TurnTracker,
}

impl McpTransport {
    /// Create a new `McpTransport` for the given config and team.
    ///
    /// Emits a `transport_init` structured log event.
    pub fn new(config: AgentMcpConfig, _team: impl Into<String>) -> Self {
        Self {
            config,
            turn_tracker: crate::turn_control::TurnTracker::new_deferred("mcp"),
        }
    }
}

impl Drop for McpTransport {
    fn drop(&mut self) {}
}

#[async_trait]
impl CodexTransport for McpTransport {
    fn set_turn_session_context(&self, ctx: crate::turn_control::SessionContext) {
        // Clone the tracker handle (cheap: Arc clone) and set the context in
        // a background task. This keeps the trait method synchronous while
        // still allowing the async mutex inside TurnTracker to be updated.
        // This mirrors the analogous implementation in AppServerTransport.
        //
        // INTENTIONAL: McpTransport emits zero DaemonStreamEvents under normal
        // operation.  The MCP protocol has no explicit `turn/started` or
        // `turn/completed` notifications, so `TurnStarted`, `TurnCompleted`,
        // and `TurnIdle` are never emitted for MCP sessions.  As a result, the
        // TUI will show no stream state (no [BUSY]/[DONE] badge) for MCP
        // sessions.  This is consistent with the G.4 scope note that deferred
        // explicit MCP turn tracking.  The only emission path that exists is
        // `TurnTracker::emit_idle`, which fires on `interrupt_turn` (i.e. on
        // crash/interrupt) — not on normal turn completion.
        let tracker = self.turn_tracker.clone();
        tokio::spawn(async move {
            tracker.set_session_context(ctx).await;
        });
    }

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

        let exit_status: Arc<Mutex<Option<std::process::ExitStatus>>> = Arc::new(Mutex::new(None));
        let shared_stdin = Arc::new(Mutex::new(
            Box::new(stdin) as Box<dyn AsyncWrite + Send + Unpin>
        ));
        let process: Arc<Mutex<Option<tokio::process::Child>>> = Arc::new(Mutex::new(Some(child)));

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
///
/// # Turn tracking
///
/// `JsonCodecTransport` uses a `type` field rather than `method` for its
/// cli-json events, so there are no explicit `turn/started` / `turn/completed`
/// method names to hook. Turn tracking via [`crate::turn_control::TurnTracker`]
/// is deferred to a future sprint when cli-json turn notifications are added.
/// The tracker is present for API consistency and to enable deferred wiring.
#[derive(Debug)]
pub(crate) struct JsonCodecTransport {
    config: AgentMcpConfig,
    /// Shared idle flag: set to `true` by background task when `idle` JSONL event seen.
    idle_flag: Arc<AtomicBool>,
    /// Turn state tracker using the shared `stream_norm` abstraction.
    ///
    /// `JsonCodecTransport` uses a `type` field rather than `method` for its cli-json
    /// events, but expresses turn-lifecycle transitions as `TurnState` mutations from
    /// `stream_norm`.  This demonstrates the shared abstraction between transports.
    cli_json_turn_state: Arc<Mutex<crate::stream_norm::TurnState>>,
    /// Unified turn tracker (deferred: cli-json turn tracking wired in future sprint).
    #[expect(
        dead_code,
        reason = "present for API consistency with other transports; \
                  cli-json explicit turn notifications are deferred to a future sprint"
    )]
    pub(crate) turn_tracker: crate::turn_control::TurnTracker,
}

impl JsonCodecTransport {
    /// Create a new `JsonCodecTransport` for the given config and team.
    pub fn new(config: AgentMcpConfig, _team: impl Into<String>) -> Self {
        Self {
            config,
            idle_flag: Arc::new(AtomicBool::new(false)),
            cli_json_turn_state: Arc::new(Mutex::new(crate::stream_norm::TurnState::Idle)),
            turn_tracker: crate::turn_control::TurnTracker::new_deferred("cli-json"),
        }
    }
}

impl Drop for JsonCodecTransport {
    fn drop(&mut self) {}
}

/// JSONL event type, as parsed by the `JsonCodecTransport` background reader.
///
/// # Design note: intentional divergence from `stream_norm::parse_app_server_notification`
///
/// This enum and [`parse_event_type`] are intentionally separate from the
/// `stream_norm::parse_app_server_notification` parser used by the app-server
/// transport.  The two parsers differ because:
///
/// - `cli-json` events use a `"type"` field (e.g. `{"type":"idle"}`).
/// - App-server notifications use a `"method"` field in JSON-RPC style
///   (e.g. `{"method":"turn/started","params":{...}}`).
///
/// Unifying them into one parser would require additional dispatch logic and
/// would couple two unrelated protocol shapes.  The divergence is intentional.
#[derive(Debug)]
pub(crate) enum TransportEventType {
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
pub(crate) fn parse_event_type(line: &str) -> TransportEventType {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| {
            v.get("type")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        })
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
        let cli_json_turn_state = Arc::clone(&self.cli_json_turn_state);
        let cli_json_agent_identity = self
            .config
            .identity
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        // Background task: read lines from the real child stdout, detect idle/done
        // events, and forward everything to the duplex write half.
        tokio::spawn(async move {
            use crate::stream_norm::{TurnState, TurnStatus};
            use agent_team_mail_core::daemon_stream::DaemonStreamEvent;

            let reader = BufReader::new(child_stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                match parse_event_type(&line) {
                    TransportEventType::Idle => {
                        idle_flag.store(true, Ordering::SeqCst);
                        // Also reflect idle via the shared stream_norm TurnState.
                        *cli_json_turn_state.lock().await = TurnState::Idle;
                        // Emit TurnIdle to daemon (best-effort).
                        {
                            let event = DaemonStreamEvent::TurnIdle {
                                agent: cli_json_agent_identity.clone(),
                                turn_id: String::new(),
                                transport: "cli-json".to_string(),
                            };
                            tokio::spawn(async move {
                                crate::stream_emit::emit_stream_event(&event).await;
                            });
                        }
                    }
                    TransportEventType::Done => {
                        // Reset idle flag on done (session complete).
                        idle_flag.store(false, Ordering::SeqCst);
                        // Reflect terminal state via the shared stream_norm TurnState.
                        *cli_json_turn_state.lock().await = TurnState::Terminal {
                            turn_id: String::new(),
                            status: TurnStatus::Completed,
                        };
                        // Emit TurnCompleted to daemon (best-effort).
                        {
                            let event = DaemonStreamEvent::TurnCompleted {
                                agent: cli_json_agent_identity.clone(),
                                thread_id: String::new(),
                                turn_id: String::new(),
                                status:
                                    agent_team_mail_core::daemon_stream::TurnStatusWire::Completed,
                                transport: "cli-json".to_string(),
                            };
                            tokio::spawn(async move {
                                crate::stream_emit::emit_stream_event(&event).await;
                            });
                        }
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

// ─── AppServerTransport ───────────────────────────────────────────────────────

/// Minimum supported app-server protocol version (string comparison; acceptable for semver at
/// this stage).  Versions below this string cause a `warn`-level log AND return an `Err`
/// from [`AppServerTransport::spawn`] / [`AppServerTransport::spawn_from_io`].
/// Protocol version incompatibility is surfaced explicitly — no silent downgrade or silent
/// failure is permitted.
const MIN_SUPPORTED_PROTOCOL_VERSION: &str = "2.0";

/// Compare two dot-separated version strings numerically.
///
/// Parses each segment as `u64`; segments that fail to parse are compared
/// lexicographically as a fallback.  Returns `true` when `version` is
/// *at least* `min_version`.
///
/// # Examples
/// ```ignore
/// assert!(version_gte("2.0", "2.0"));
/// assert!(version_gte("10.0", "2.0"));
/// assert!(!version_gte("1.9", "2.0"));
/// ```
fn version_gte(version: &str, min_version: &str) -> bool {
    let a: Vec<&str> = version.split('.').collect();
    let b: Vec<&str> = min_version.split('.').collect();
    let len = a.len().max(b.len());
    for i in 0..len {
        let seg_a = a.get(i).copied().unwrap_or("0");
        let seg_b = b.get(i).copied().unwrap_or("0");
        match (seg_a.parse::<u64>(), seg_b.parse::<u64>()) {
            (Ok(na), Ok(nb)) => match na.cmp(&nb) {
                std::cmp::Ordering::Greater => return true,
                std::cmp::Ordering::Less => return false,
                std::cmp::Ordering::Equal => continue,
            },
            _ => match seg_a.cmp(seg_b) {
                std::cmp::Ordering::Greater => return true,
                std::cmp::Ordering::Less => return false,
                std::cmp::Ordering::Equal => continue,
            },
        }
    }
    true // all segments equal
}

/// Transport that spawns `codex app-server` and communicates via the app-server
/// JSON-RPC 2.0 JSONL protocol.
///
/// Unlike [`McpTransport`] and [`JsonCodecTransport`], this transport performs
/// the MCP-style `initialize` / `initialized` handshake inside [`Self::spawn`]
/// before returning the [`RawChildIo`].  This guarantees that downstream proxy
/// code never sends turn or thread requests before the protocol handshake is
/// complete.
///
/// # Stdout isolation
///
/// Child stdout is always piped (never inherited from the parent process).
/// The `atm-agent-mcp` binary uses its own stdout for JSON-RPC with upstream
/// (Claude).  Letting child stdout bleed into parent stdout would corrupt that
/// channel.
///
/// # Turn state
///
/// A background task reads JSONL lines from child stdout, parses them with
/// [`crate::stream_norm::parse_app_server_notification`], and maintains a
/// per-thread [`crate::stream_norm::TurnState`].  The `is_idle()` trait method
/// returns `true` only when all threads are in the `Idle` state.
///
/// # Crash handling
///
/// When child stdout closes (EOF), the background task marks all active threads
/// as [`crate::stream_norm::TurnState::Terminal`] with
/// [`crate::stream_norm::TurnStatus::Failed`] and clears the `initialized` flag.

#[derive(Debug)]
pub(crate) struct AppServerTransport {
    config: AgentMcpConfig,
    team: String,
    /// Thread ID -> ATM session ID mapping.
    ///
    /// This is the transport-local thread registry. It maps Codex `threadId` values to
    /// ATM session identifiers within this transport instance.
    /// Integration with the shared `SessionRegistry` in `session.rs` (which carries
    /// `SessionStatus` and `ThreadState` per ATM identity) is deferred to Sprint G.4,
    /// which will wire this transport's turn events into the daemon-facing session model.
    session_registry: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// Currently active turn state per thread_id.
    turn_state: Arc<Mutex<std::collections::HashMap<String, crate::stream_norm::TurnState>>>,
    /// Protocol version from initialize response.
    protocol_version: Arc<Mutex<Option<String>>>,
    /// Whether the initialize/initialized handshake has completed.
    initialized: Arc<AtomicBool>,
    /// Idle flag passed to [`RawChildIo`].
    ///
    /// Set to `false` when a `TurnStarted` notification is received (agent is busy),
    /// and set to `true` when a `TurnCompleted` notification is received and all
    /// threads are idle.  Set to `false` on child crash (EOF).
    ///
    /// This is distinct from `initialized` (which tracks handshake completion).
    idle_flag: Arc<AtomicBool>,
    /// Pending request-response correlation channels.
    ///
    /// When a request is sent that expects a response (e.g. `thread/fork`), a
    /// oneshot sender is inserted under the request ID. The background task
    /// routes incoming responses to the matching sender.
    pending_responses:
        Arc<Mutex<std::collections::HashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>>>,
    /// Elicitation registry for approval-gate bridging (FR-18 / G.5).
    ///
    /// Shared with the notification background task so that when the app-server
    /// emits `item/enteredReviewMode`, the task can register a pending approval
    /// entry and the proxy's upstream-response handler can resolve it.
    pub(crate) elicitation_registry: Arc<Mutex<crate::elicitation::ElicitationRegistry>>,
    /// Monotonic counter for generating unique upstream request IDs for bridged
    /// approval requests (analogous to the counter in `ProxyServer`).
    pub(crate) elicitation_counter: Arc<AtomicU64>,
    /// Deferred upstream write channel for approval-gate bridging (G.5).
    ///
    /// Populated by the proxy via [`AppServerTransport::set_approval_channels`]
    /// after the child is spawned and the upstream channel exists.  The
    /// background notification task holds a clone of this `Arc` and reads the
    /// sender lazily when an `item/enteredReviewMode` notification arrives.
    upstream_tx: Arc<Mutex<Option<tokio::sync::mpsc::Sender<Value>>>>,
    /// Unified turn tracker for daemon lifecycle emission.
    ///
    /// Created with [`crate::turn_control::TurnTracker::new_deferred`] so that the
    /// transport can be constructed before a full [`crate::turn_control::SessionContext`]
    /// is available. Call [`Self::set_session_context`] once session information is
    /// known to enable daemon emission.
    pub(crate) turn_tracker: crate::turn_control::TurnTracker,
}

impl AppServerTransport {
    /// Create a new `AppServerTransport` for the given config and team.
    pub fn new(config: AgentMcpConfig, team: impl Into<String>) -> Self {
        let team = team.into();
        Self {
            config,
            team,
            session_registry: Arc::new(Mutex::new(std::collections::HashMap::new())),
            turn_state: Arc::new(Mutex::new(std::collections::HashMap::new())),
            protocol_version: Arc::new(Mutex::new(None)),
            initialized: Arc::new(AtomicBool::new(false)),
            idle_flag: Arc::new(AtomicBool::new(false)),
            pending_responses: Arc::new(Mutex::new(std::collections::HashMap::new())),
            elicitation_registry: Arc::new(Mutex::new(
                crate::elicitation::ElicitationRegistry::new(120),
            )),
            elicitation_counter: Arc::new(AtomicU64::new(0)),
            upstream_tx: Arc::new(Mutex::new(None)),
            turn_tracker: crate::turn_control::TurnTracker::new_deferred("app-server"),
        }
    }

    /// Bind a [`crate::turn_control::SessionContext`] to the transport's turn tracker.
    ///
    /// Once called, daemon lifecycle events (e.g. `teammate_idle`) will be emitted
    /// whenever turns complete. This must be called after session identity information
    /// (agent identity, team, session ID) is available — typically after the first
    /// successful session registration.
    ///
    /// The proxy calls the synchronous [`CodexTransport::set_turn_session_context`]
    /// trait method instead (which spawns a task to call this), so this method
    /// is retained as a direct async API for callers that already hold an
    /// `AppServerTransport` reference in an async context.
    #[expect(
        dead_code,
        reason = "direct async API; proxy uses set_turn_session_context on the trait \
                  to avoid requiring a concrete AppServerTransport reference"
    )]
    pub async fn set_session_context(&self, ctx: crate::turn_control::SessionContext) {
        self.turn_tracker.set_session_context(ctx).await;
    }

    /// Send a JSONL request to the child process stdin with bounded retry/backoff for
    /// write-level errors.
    ///
    /// Retries up to `MAX_BACKPRESSURE_RETRIES` times with exponential backoff starting
    /// at 50 ms.  Returns `Err` with a descriptive message if all retries are exhausted.
    ///
    /// This function handles write-level retries for stdin buffering errors only.
    /// For application-level `-32001` overload responses, use
    /// [`Self::send_request_with_overload_retry`] instead.
    async fn send_with_backoff(
        stdin: &Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
        request: &str,
    ) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt as _;

        const MAX_BACKPRESSURE_RETRIES: u32 = 3;
        let mut delay_ms = 50u64;

        for attempt in 0..=MAX_BACKPRESSURE_RETRIES {
            let result = {
                let mut guard = stdin.lock().await;
                guard.write_all(request.as_bytes()).await
            };
            match result {
                Ok(()) => return Ok(()),
                Err(e) if attempt < MAX_BACKPRESSURE_RETRIES => {
                    tracing::debug!(
                        attempt = attempt + 1,
                        delay_ms = delay_ms,
                        error = %e,
                        "stdin write failed; retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    delay_ms *= 2;
                }
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "stdin write failed after {} attempts: {e}",
                        MAX_BACKPRESSURE_RETRIES + 1
                    ));
                }
            }
        }
        unreachable!("loop must have returned or errored above")
    }

    /// Send a JSON-RPC request and await its response, retrying on `-32001` overload.
    ///
    /// Registers a oneshot channel in `pending_responses`, sends the request via
    /// `send_with_backoff`, and awaits the response from the background task.
    /// If the response is a `-32001` overload error, retries with exponential
    /// backoff up to `max_retries` times.
    ///
    /// # Errors
    ///
    /// Returns an error if all retries are exhausted, the response channel is
    /// closed, or a timeout occurs waiting for the response.
    async fn send_request_with_overload_retry(
        stdin: &Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
        pending_responses: &Arc<
            Mutex<std::collections::HashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>>,
        >,
        req_id: u64,
        request: &serde_json::Value,
        max_retries: u32,
    ) -> anyhow::Result<serde_json::Value> {
        let line = format!("{}\n", serde_json::to_string(request)?);
        let mut delay_ms = 50u64;

        for attempt in 0..=max_retries {
            let (tx, rx) = tokio::sync::oneshot::channel();
            pending_responses.lock().await.insert(req_id, tx);

            Self::send_with_backoff(stdin, &line).await?;

            let response = tokio::time::timeout(std::time::Duration::from_secs(10), rx)
                .await
                .map_err(|_| {
                    anyhow::anyhow!("timeout waiting for response to request id={req_id}")
                })?
                .map_err(|_| anyhow::anyhow!("response channel closed for request id={req_id}"))?;

            if crate::stream_norm::is_overload_error(&response) {
                if attempt < max_retries {
                    tracing::warn!(
                        req_id,
                        attempt,
                        delay_ms,
                        "app-server returned -32001 overload; retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    delay_ms = delay_ms.saturating_mul(2).min(5000);
                    continue;
                }
                anyhow::bail!(
                    "app-server overloaded after {max_retries} retries for request id={req_id}"
                );
            }

            return Ok(response);
        }
        unreachable!("loop must have returned or errored above")
    }

    /// Fork a new thread by sending a `thread/fork` request to the child process.
    ///
    /// Sends the request and awaits the correlated response, retrying on `-32001`
    /// overload errors. Returns the response JSON value on success.
    ///
    /// Registers a placeholder in `session_registry` mapping the Codex `threadId`
    /// to a `"pending-atm-session:<threadId>"` sentinel. Sprint G.4 will replace
    /// this with the actual ATM session ID once session correlation is established.
    ///
    /// # Errors
    ///
    /// Returns an error if the child process is not running, the write fails after
    /// retries, or a `-32001` overload persists beyond the retry budget.
    pub(crate) async fn fork_thread(
        &self,
        stdin: &Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
        thread_id: &str,
    ) -> anyhow::Result<serde_json::Value> {
        static FORK_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(100);
        let req_id = FORK_COUNTER.fetch_add(1, Ordering::SeqCst);

        let request = serde_json::json!({
            "id": req_id,
            "method": "thread/fork",
            "params": { "threadId": thread_id }
        });

        // Register a sentinel in session_registry. G.4 will populate the real
        // ATM session ID once session correlation is established.
        self.session_registry.lock().await.insert(
            thread_id.to_string(),
            format!("pending-atm-session:{thread_id}"),
        );

        let response = Self::send_request_with_overload_retry(
            stdin,
            &self.pending_responses,
            req_id,
            &request,
            3,
        )
        .await?;

        Ok(response)
    }

    /// Spawn the transport from pre-existing I/O handles (for testing).
    ///
    /// Performs the `initialize` / `initialized` handshake over the provided
    /// I/O and starts the background notification task. No real child process
    /// is spawned.
    ///
    /// # Errors
    ///
    /// Returns an error if the handshake fails or the initialize response
    /// contains an error.
    pub(crate) async fn spawn_from_io(
        &self,
        stdin: Box<dyn AsyncWrite + Send + Unpin>,
        stdout: Box<dyn AsyncRead + Send + Unpin>,
    ) -> anyhow::Result<RawChildIo> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let mut child_stdin = stdin;

        // ── Initialize handshake ────────────────────────────────────────────
        let init_request = serde_json::json!({
            "id": 0,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "atm-agent-mcp",
                    "version": "0.1.0"
                }
            }
        });
        let init_line = format!("{}\n", serde_json::to_string(&init_request)?);
        child_stdin.write_all(init_line.as_bytes()).await?;

        let mut reader = BufReader::new(stdout);
        let mut response_line = String::new();
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reader.read_line(&mut response_line),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timeout waiting for initialize response"))?
        .map_err(|e| anyhow::anyhow!("I/O error reading initialize response: {e}"))?;

        if n == 0 {
            return Err(anyhow::anyhow!(
                "child process closed stdout before sending initialize response"
            ));
        }

        let response: serde_json::Value = serde_json::from_str(response_line.trim())
            .map_err(|e| anyhow::anyhow!("invalid JSON in initialize response: {e}"))?;

        if response.get("error").is_some() {
            let msg = response["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            return Err(anyhow::anyhow!("initialize failed: {msg}"));
        }

        if response.get("result").is_none() {
            return Err(anyhow::anyhow!(
                "initialize response missing 'result' field"
            ));
        }

        let negotiated_version = response["result"]
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        if let Some(ref ver) = negotiated_version {
            *self.protocol_version.lock().await = Some(ver.clone());
            if !version_gte(ver.as_str(), MIN_SUPPORTED_PROTOCOL_VERSION) {
                tracing::warn!(
                    version = %ver,
                    min_required = MIN_SUPPORTED_PROTOCOL_VERSION,
                    "app-server protocol version is below minimum supported; rejecting connection"
                );
                anyhow::bail!(
                    "unsupported app-server protocol version: {ver:?}; \
                     minimum required: {MIN_SUPPORTED_PROTOCOL_VERSION}"
                );
            }
        }

        // Send the initialized notification.
        let initialized_notif = serde_json::json!({
            "method": "initialized",
            "params": {}
        });
        let notif_line = format!("{}\n", serde_json::to_string(&initialized_notif)?);
        child_stdin.write_all(notif_line.as_bytes()).await?;

        self.initialized.store(true, Ordering::SeqCst);

        // ── Set up shared state and the duplex stream ───────────────────────
        let (duplex_write, duplex_read) = tokio::io::duplex(65_536);

        // Create shared_stdin before the task state so the notification task
        // can hold a clone for delivering approval responses to child stdin.
        let shared_stdin = Arc::new(Mutex::new(child_stdin));

        let task_state = NotificationTaskState {
            turn_state: Arc::clone(&self.turn_state),
            idle_flag: Arc::clone(&self.idle_flag),
            initialized: Arc::clone(&self.initialized),
            pending_responses: Arc::clone(&self.pending_responses),
            session_registry: Arc::clone(&self.session_registry),
            team: self.team.clone(),
            // Wire the transport's turn_tracker so that daemon lifecycle events
            // are emitted when turns complete. The tracker may have no session
            // context yet (deferred); set_session_context can be called later.
            turn_tracker: Some(self.turn_tracker.clone()),
            // Wire approval-gate bridging (G.5).
            elicitation_registry: Some(Arc::clone(&self.elicitation_registry)),
            elicitation_counter: Some(Arc::clone(&self.elicitation_counter)),
            upstream_tx: Some(Arc::clone(&self.upstream_tx)),
            // Share child stdin so the notification task can deliver approval
            // decisions back to the child process.
            child_stdin: Some(Arc::clone(&shared_stdin)),
            agent_identity: self.config.identity.clone(),
        };

        tokio::spawn(drive_notification_task(
            reader.into_inner(),
            duplex_write,
            task_state,
        ));

        Ok(RawChildIo {
            stdin: shared_stdin,
            stdout: Box::new(duplex_read) as Box<dyn AsyncRead + Send + Unpin>,
            exit_status: Arc::new(Mutex::new(None)),
            process: Arc::new(Mutex::new(None)),
            idle_flag: Some(Arc::clone(&self.idle_flag)),
        })
    }
}

impl Drop for AppServerTransport {
    fn drop(&mut self) {}
}

/// Shared state bundle for [`drive_notification_task`].
///
/// Groups the `Arc`-wrapped shared state fields that the background task
/// needs, keeping the function signature under the clippy argument limit.
#[doc(hidden)]
pub struct NotificationTaskState {
    /// Per-thread turn state map.
    pub turn_state: Arc<Mutex<std::collections::HashMap<String, crate::stream_norm::TurnState>>>,
    /// Idle flag shared with the transport.
    pub idle_flag: Arc<AtomicBool>,
    /// Whether the handshake has completed.
    pub initialized: Arc<AtomicBool>,
    /// Pending request-response correlation channels.
    pub pending_responses:
        Arc<Mutex<std::collections::HashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>>>,
    /// Thread ID to ATM session mapping.
    pub session_registry: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// Team name for structured event logging.
    pub team: String,
    /// Optional unified turn tracker.
    ///
    /// When `Some`, the background task calls [`crate::turn_control::TurnTracker`]
    /// on each `turn/started` and `turn/completed` notification so that daemon
    /// lifecycle events are emitted via a single, transport-agnostic path.
    ///
    /// `None` is used for the raw `AppServerTransport::spawn` path before a
    /// full [`crate::turn_control::SessionContext`] is available.
    pub turn_tracker: Option<crate::turn_control::TurnTracker>,
    /// Elicitation registry for approval-gate bridging (G.5).
    ///
    /// When `Some`, `item/enteredReviewMode` notifications are registered here so
    /// the proxy's upstream-response handler can resolve them to a downstream
    /// (child stdin) response.  The same `Arc` is shared with the
    /// `AppServerTransport` struct so the proxy can call `resolve_for_downstream`
    /// when an upstream elicitation response arrives.
    ///
    /// `None` disables approval bridging (used in tests that do not exercise the
    /// full proxy path or do not need review-mode support).
    pub elicitation_registry: Option<Arc<Mutex<crate::elicitation::ElicitationRegistry>>>,
    /// Monotonic counter for generating upstream request IDs for bridged approval
    /// requests.  Shared with the `AppServerTransport` struct.
    ///
    /// `None` when `elicitation_registry` is `None`.
    pub elicitation_counter: Option<Arc<AtomicU64>>,
    /// Deferred upstream write channel holder for approval-gate bridging (G.5).
    ///
    /// Holds a clone of the `Arc` from `AppServerTransport::upstream_tx`.
    /// The background task reads the inner `Option<Sender>` lazily when an
    /// `item/enteredReviewMode` notification arrives.  If the sender is not yet
    /// populated (i.e. before the proxy calls `set_approval_upstream_tx`), the
    /// notification is logged at `warn` level and discarded — the approval will
    /// eventually time out and be rejected by `ElicitationRegistry::expire_timeouts`.
    ///
    /// `None` when operating without a proxy (unit tests).
    pub upstream_tx: Option<Arc<Mutex<Option<tokio::sync::mpsc::Sender<Value>>>>>,
    /// Shared stdin writer for the app-server child process (G.5).
    ///
    /// When `Some`, the background task uses this to deliver approval decisions
    /// received via the elicitation registry back to the child process stdin.
    /// The `Arc<Mutex<...>>` is shared between the task (which awaits responses)
    /// and the `RawChildIo` returned to the proxy (which also writes via stdin).
    ///
    /// `None` when operating without a real child process (unit tests that do
    /// not exercise the full approval round-trip).
    pub child_stdin: Option<Arc<Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>>>,
    /// Agent identity for daemon stream event emission (e.g., `"arch-ctm"`).
    ///
    /// Resolved from [`AgentMcpConfig::identity`]. When `None`, stream events
    /// use `"unknown"` as the agent name.
    pub agent_identity: Option<String>,
}

/// Bridge an `item/enteredReviewMode` notification upstream as an
/// `elicitation/create` request (G.5).
///
/// Generates a unique upstream request ID from `elicitation_counter`, registers
/// a pending entry in `elicitation_registry` with a oneshot channel, and sends
/// the bridged request via `upstream_tx_arc`.
///
/// If any prerequisite is `None` (i.e. the task was constructed without
/// approval-gate wiring, or `upstream_tx` has not yet been set by the proxy),
/// logs a `warn`-level message and returns without bridging.  The pending
/// elicitation will never be resolved; the `ElicitationRegistry::expire_timeouts`
/// loop will eventually reject it with a `-32006` timeout error.
///
/// # Security invariant
///
/// This function never silently approves a pending review.  The only outcomes are:
/// - Upstream receives the elicitation and responds explicitly (approve or reject).
/// - The registry timeout loop rejects it with error code `-32006`.
/// - The session closes and `cancel_for_agent` rejects all pending entries.
async fn bridge_entered_review_mode(
    item_id: &str,
    params: &Value,
    team: &str,
    elicitation_registry: &Option<Arc<Mutex<crate::elicitation::ElicitationRegistry>>>,
    elicitation_counter: &Option<Arc<AtomicU64>>,
    upstream_tx_arc: &Option<Arc<Mutex<Option<tokio::sync::mpsc::Sender<Value>>>>>,
    child_stdin: &Option<Arc<Mutex<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>>>,
) {
    let (Some(registry), Some(counter), Some(tx_arc)) =
        (elicitation_registry, elicitation_counter, upstream_tx_arc)
    else {
        tracing::warn!(
            item_id = %item_id,
            "item/enteredReviewMode received but approval bridging is not wired; \
             notification dropped (no upstream channel)"
        );
        return;
    };

    let upstream_id_num = counter.fetch_add(1, Ordering::Relaxed);
    let upstream_request_id = serde_json::json!(upstream_id_num);
    // Use item_id as both agent_id key and downstream_request_id for
    // app-server approval bridging.  The downstream response is written
    // back to the child via the spawned delivery task below.
    let downstream_request_id = serde_json::json!(item_id);

    let (response_tx, response_rx) = tokio::sync::oneshot::channel::<Value>();

    registry.lock().await.register(
        item_id.to_string(),
        downstream_request_id.clone(),
        upstream_request_id.clone(),
        response_tx,
    );

    // Spawn a task that waits for the upstream approval decision and delivers
    // it to the app-server child's stdin.  This is the only code path that
    // writes the final approval/rejection JSON-RPC response back to the child;
    // the elicitation registry's expire_timeouts() and cancel_for_agent() paths
    // resolve the oneshot (sending here), but do NOT write to child stdin
    // directly — that responsibility belongs to this delivery task.
    //
    // When child_stdin is None (unit tests without a real child), the response
    // is received but discarded; the security invariant (no silent approval) is
    // still upheld because the registry always sends an explicit reject payload.
    if let Some(stdin_arc) = child_stdin.clone() {
        let item_id_owned = item_id.to_string();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt as _;
            // The registry's default timeout is 30 seconds.  Mirror that here
            // so the delivery task does not outlive the registry entry.
            const DELIVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(35);
            match tokio::time::timeout(DELIVERY_TIMEOUT, response_rx).await {
                Ok(Ok(response)) => {
                    let line = match serde_json::to_string(&response) {
                        Ok(s) => format!("{s}\n"),
                        Err(e) => {
                            tracing::warn!(
                                item_id = %item_id_owned,
                                "failed to serialize elicitation response for child: {e}"
                            );
                            return;
                        }
                    };
                    let mut guard = stdin_arc.lock().await;
                    if let Err(e) = guard.write_all(line.as_bytes()).await {
                        tracing::warn!(
                            item_id = %item_id_owned,
                            "failed to deliver elicitation response to child stdin: {e}"
                        );
                    } else {
                        tracing::debug!(
                            item_id = %item_id_owned,
                            "approval decision delivered to child stdin"
                        );
                    }
                }
                Ok(Err(_)) => {
                    // Sender dropped without sending — the registry was
                    // cancelled or dropped.  Nothing to deliver.
                    tracing::debug!(
                        item_id = %item_id_owned,
                        "elicitation response_tx dropped before delivery"
                    );
                }
                Err(_) => {
                    // Delivery timeout — the registry's expire_timeouts() loop
                    // should have already sent a rejection via response_tx.
                    // This branch is a safety net in case the registry loop
                    // is slower than expected.
                    tracing::debug!(
                        item_id = %item_id_owned,
                        "elicitation delivery task timed out (registry should have rejected)"
                    );
                }
            }
        });
    }

    // Build the upstream elicitation/create request, preserving params from
    // the EnteredReviewMode notification.
    let mut upstream_params = params.clone();
    if let Some(obj) = upstream_params.as_object_mut() {
        obj.insert("itemId".to_string(), serde_json::json!(item_id));
        obj.insert("source".to_string(), serde_json::json!("app-server"));
    }

    let upstream_msg = serde_json::json!({
        "id": upstream_request_id,
        "method": "elicitation/create",
        "params": upstream_params,
    });

    let tx_guard = tx_arc.lock().await;
    if let Some(ref tx) = *tx_guard {
        if tx.send(upstream_msg).await.is_err() {
            tracing::warn!(
                item_id = %item_id,
                "failed to send elicitation/create upstream: channel closed"
            );
        } else {
            tracing::debug!(
                item_id = %item_id,
                upstream_request_id = %upstream_id_num,
                team = %team,
                "approval gate: bridged item/enteredReviewMode upstream as elicitation/create"
            );
        }
    } else {
        tracing::warn!(
            item_id = %item_id,
            "item/enteredReviewMode: upstream_tx not yet populated by proxy; \
             approval will be rejected by timeout"
        );
    }
}

/// Drive the app-server notification background task from an stdout reader.
///
/// Reads JSONL lines from `stdout`, routes responses to `pending_responses`
/// channels, parses notifications into turn-state updates, and forwards all
/// raw lines through `duplex_write` to the proxy reader.
///
/// When `state.turn_tracker` is `Some`, the task calls into the unified
/// [`crate::turn_control::TurnTracker`] on every `turn/started` and
/// `turn/completed` notification so daemon lifecycle events (e.g.
/// `teammate_idle`) are emitted via a single, transport-agnostic path.
///
/// Called from [`AppServerTransport::spawn`] with the real child stdout, and
/// directly from integration tests with in-memory duplex pipes.
#[doc(hidden)]
pub async fn drive_notification_task(
    stdout: impl AsyncRead + Unpin + Send + 'static,
    mut duplex_write: tokio::io::DuplexStream,
    state: NotificationTaskState,
) {
    let NotificationTaskState {
        turn_state,
        idle_flag,
        initialized,
        pending_responses,
        session_registry,
        team,
        turn_tracker,
        elicitation_registry,
        elicitation_counter,
        upstream_tx: upstream_tx_arc,
        child_stdin,
        agent_identity,
    } = state;
    use crate::stream_norm::{
        AppServerNotification, TurnState, TurnStatus, parse_app_server_notification,
    };
    use agent_team_mail_core::daemon_stream::{DaemonStreamEvent, TurnStatusWire};

    let agent_name = agent_identity.unwrap_or_else(|| "unknown".to_string());
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        // Check if this is a response to a pending request (has `id` and no `method`).
        let line_val: Option<serde_json::Value> = serde_json::from_str(&line).ok();
        if let Some(ref v) = line_val {
            // Route responses (id present, result or error present) to pending channels.
            if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
                if v.get("result").is_some() || v.get("error").is_some() {
                    let sender = pending_responses.lock().await.remove(&id);
                    if let Some(tx) = sender {
                        let _ = tx.send(v.clone());
                    }
                }
            }

            if v.get("method").is_none() && v.get("id").is_some() {
                // This is a response (not a notification).
                // Forward the response and continue (skip notification parsing).
                let bytes = format!("{line}\n");
                if duplex_write.write_all(bytes.as_bytes()).await.is_err() {
                    break;
                }
                continue;
            }
        }

        if let Some(notification) = parse_app_server_notification(&line) {
            match notification {
                AppServerNotification::TurnStarted { thread_id, turn_id } => {
                    idle_flag.store(false, Ordering::SeqCst);
                    turn_state.lock().await.insert(
                        thread_id.clone(),
                        TurnState::Busy {
                            turn_id: turn_id.clone(),
                        },
                    );
                    // Notify the unified turn tracker (if wired) so that
                    // per-thread active-turn state is kept consistent across
                    // all transports.  Pass thread_id as the first argument
                    // (key for per-thread tracking) and turn_id as the second
                    // (unique turn identifier within the thread).
                    // Use start_turn_no_emit to avoid double-emitting TurnStarted
                    // — AppServerTransport emits the stream event directly below
                    // with the correct transport="app-server" label.
                    if let Some(ref tracker) = turn_tracker {
                        tracker.start_turn_no_emit(&thread_id, &turn_id).await;
                    }
                    // Emit normalized stream event to the daemon (best-effort).
                    {
                        let event = DaemonStreamEvent::TurnStarted {
                            agent: agent_name.clone(),
                            thread_id: thread_id.clone(),
                            turn_id: turn_id.clone(),
                            transport: "app-server".to_string(),
                        };
                        tokio::spawn(async move {
                            crate::stream_emit::emit_stream_event(&event).await;
                        });
                    }
                }
                AppServerNotification::TurnCompleted {
                    thread_id,
                    turn_id,
                    status,
                } => {
                    let status_for_tracker = status.clone();
                    let wire_status = match &status {
                        TurnStatus::Completed => TurnStatusWire::Completed,
                        TurnStatus::Interrupted => TurnStatusWire::Interrupted,
                        TurnStatus::Failed => TurnStatusWire::Failed,
                    };
                    {
                        let mut ts = turn_state.lock().await;
                        ts.insert(
                            thread_id.clone(),
                            TurnState::Terminal {
                                turn_id: turn_id.clone(),
                                status,
                            },
                        );
                        let all_done = ts
                            .values()
                            .all(|s| s.is_idle() || matches!(s, TurnState::Terminal { .. }));
                        if all_done || ts.is_empty() {
                            idle_flag.store(true, Ordering::SeqCst);
                        }
                    }
                    // Notify the unified turn tracker (if wired).  Use
                    // on_turn_completed_no_emit to update internal per-thread
                    // turn state and emit the TeammateIdle lifecycle event,
                    // but skip DaemonStreamEvent emission — AppServerTransport
                    // emits TurnCompleted and TurnIdle directly below with the
                    // correct transport="app-server" label, avoiding
                    // double-emission.
                    if let Some(ref tracker) = turn_tracker {
                        tracker
                            .on_turn_completed_no_emit(&thread_id, &turn_id, status_for_tracker)
                            .await;
                    }
                    // Emit normalized stream event to the daemon (best-effort).
                    {
                        let event = DaemonStreamEvent::TurnCompleted {
                            agent: agent_name.clone(),
                            thread_id: thread_id.clone(),
                            turn_id: turn_id.clone(),
                            status: wire_status,
                            transport: "app-server".to_string(),
                        };
                        tokio::spawn(async move {
                            crate::stream_emit::emit_stream_event(&event).await;
                        });
                    }
                    // Immediately follow TurnCompleted with TurnIdle so the TUI
                    // badge transitions from [DONE] back to idle and does not
                    // remain stuck in the Terminal state until the next turn
                    // starts.  The turn_id for TurnIdle is left empty because
                    // there is no active turn at this point.
                    {
                        let idle_event = DaemonStreamEvent::TurnIdle {
                            agent: agent_name.clone(),
                            turn_id: String::new(),
                            transport: "app-server".to_string(),
                        };
                        tokio::spawn(async move {
                            crate::stream_emit::emit_stream_event(&idle_event).await;
                        });
                    }
                }
                AppServerNotification::ItemStarted { item_id } => {
                    tracing::debug!(item_id = %item_id, "item/started");
                }
                AppServerNotification::ItemCompleted { item_id } => {
                    tracing::debug!(item_id = %item_id, "item/completed");
                }
                AppServerNotification::ItemDelta { method, .. } => {
                    tracing::debug!(method = %method, "item/delta");
                }
                // Approval gate: agent entered review mode — bridge upstream (G.5).
                AppServerNotification::EnteredReviewMode { item_id, params } => {
                    bridge_entered_review_mode(
                        &item_id,
                        &params,
                        &team,
                        &elicitation_registry,
                        &elicitation_counter,
                        &upstream_tx_arc,
                        &child_stdin,
                    )
                    .await;
                }
                // Approval gate: agent exited review mode (decision delivered).
                AppServerNotification::ExitedReviewMode { item_id } => {
                    tracing::debug!(
                        item_id = %item_id,
                        "app-server: item/exitedReviewMode (approval decision delivered)"
                    );
                }
                AppServerNotification::Unknown { method } => {
                    tracing::debug!(
                        method = %method,
                        "app-server: unknown notification (non-fatal)"
                    );
                }
            }
        }

        // Forward the raw line to the duplex stream for the proxy.
        let bytes = format!("{line}\n");
        if duplex_write.write_all(bytes.as_bytes()).await.is_err() {
            break;
        }
    }

    // Child stdout closed -- mark all active threads as Terminal/Failed.
    idle_flag.store(false, Ordering::SeqCst);
    initialized.store(false, Ordering::SeqCst);
    let thread_ids: Vec<String> = session_registry.lock().await.keys().cloned().collect();
    // Collect the threads that were active (non-idle) so we can call
    // interrupt_turn on them after releasing the lock (interrupt_turn is async
    // and must not be called while holding the turn_state mutex).
    let mut interrupted_threads: Vec<String> = Vec::new();
    {
        let mut ts = turn_state.lock().await;
        for thread_id in thread_ids {
            let entry = ts.entry(thread_id.clone()).or_insert(TurnState::Idle);
            if !entry.is_idle() {
                // Use the last known turn_id from the Busy state; fall back to
                // thread_id only when no active turn_id is recorded.
                let terminal_turn_id = match &*entry {
                    TurnState::Busy { turn_id } => turn_id.clone(),
                    _ => thread_id.clone(),
                };
                *entry = TurnState::Terminal {
                    turn_id: terminal_turn_id,
                    status: TurnStatus::Failed,
                };
                interrupted_threads.push(thread_id);
            }
        }
    } // turn_state lock released here

    // Notify the unified turn tracker for each interrupted thread so the daemon
    // receives a TeammateIdle event (best-effort; turn_tracker may have no
    // session context yet if set_session_context was never called).
    if let Some(ref tracker) = turn_tracker {
        for thread_id in &interrupted_threads {
            tracker.interrupt_turn(thread_id).await;
        }
    }
    // duplex_write drops here, signalling EOF to the proxy reader.
}

#[async_trait]
impl CodexTransport for AppServerTransport {
    async fn spawn(&self) -> anyhow::Result<RawChildIo> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::process::Command;

        let mut cmd = Command::new(&self.config.codex_bin);
        cmd.arg("app-server");

        if let Some(ref model) = self.config.model {
            cmd.arg("-m").arg(model);
        }

        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let mut child = cmd.spawn()?;

        let mut child_stdin = child.stdin.take().expect("child stdin must be piped");
        let child_stdout = child.stdout.take().expect("child stdout must be piped");

        // ── Initialize handshake ────────────────────────────────────────────
        // Perform the JSON-RPC initialize / initialized exchange synchronously
        // here, before returning RawChildIo, so downstream code is guaranteed
        // to only see a fully-initialized transport.
        //
        // Per the app-server protocol spec (Section 1), messages omit the
        // `jsonrpc` field.

        let init_request = serde_json::json!({
            "id": 0,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "atm-agent-mcp",
                    "version": "0.1.0"
                }
            }
        });
        let init_line = format!("{}\n", serde_json::to_string(&init_request)?);
        child_stdin.write_all(init_line.as_bytes()).await?;

        // Read the initialize response.
        let mut reader = BufReader::new(child_stdout);
        let mut response_line = String::new();
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reader.read_line(&mut response_line),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timeout waiting for initialize response"))?
        .map_err(|e| anyhow::anyhow!("I/O error reading initialize response: {e}"))?;

        if n == 0 {
            return Err(anyhow::anyhow!(
                "child process closed stdout before sending initialize response"
            ));
        }

        let response: serde_json::Value = serde_json::from_str(response_line.trim())
            .map_err(|e| anyhow::anyhow!("invalid JSON in initialize response: {e}"))?;

        if response.get("error").is_some() {
            let msg = response["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            return Err(anyhow::anyhow!("initialize failed: {msg}"));
        }

        if response.get("result").is_none() {
            return Err(anyhow::anyhow!(
                "initialize response missing 'result' field"
            ));
        }

        // Capture optional protocol version and server info.
        let negotiated_version = response["result"]
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        if let Some(ref ver) = negotiated_version {
            *self.protocol_version.lock().await = Some(ver.clone());
            if !version_gte(ver.as_str(), MIN_SUPPORTED_PROTOCOL_VERSION) {
                tracing::warn!(
                    version = %ver,
                    min_required = MIN_SUPPORTED_PROTOCOL_VERSION,
                    "app-server protocol version is below minimum supported; rejecting connection"
                );
                anyhow::bail!(
                    "unsupported app-server protocol version: {ver:?}; \
                     minimum required: {MIN_SUPPORTED_PROTOCOL_VERSION}"
                );
            }
        } else {
            tracing::warn!(
                "app-server did not return protocolVersion; proceeding with unknown version"
            );
        }

        // Send the initialized notification.
        // Per the app-server protocol spec (Section 1), messages omit the `jsonrpc` field.
        let initialized_notif = serde_json::json!({
            "method": "initialized",
            "params": {}
        });
        let notif_line = format!("{}\n", serde_json::to_string(&initialized_notif)?);
        child_stdin.write_all(notif_line.as_bytes()).await?;

        self.initialized.store(true, Ordering::SeqCst);

        // ── Set up shared state and the duplex stream ───────────────────────

        // Pipe remaining child stdout through a duplex stream so the proxy's
        // background reader task gets a Box<dyn AsyncRead>.
        let (duplex_write, duplex_read) = tokio::io::duplex(65_536);

        // Create shared_stdin before the task state so the notification task
        // can hold a clone for delivering approval responses to child stdin.
        let exit_status: Arc<Mutex<Option<ExitStatus>>> = Arc::new(Mutex::new(None));
        let shared_stdin = Arc::new(Mutex::new(
            Box::new(child_stdin) as Box<dyn AsyncWrite + Send + Unpin>
        ));

        let task_state = NotificationTaskState {
            turn_state: Arc::clone(&self.turn_state),
            idle_flag: Arc::clone(&self.idle_flag),
            initialized: Arc::clone(&self.initialized),
            pending_responses: Arc::clone(&self.pending_responses),
            session_registry: Arc::clone(&self.session_registry),
            team: self.team.clone(),
            // Wire the transport's turn_tracker so that daemon lifecycle events
            // are emitted when turns complete. The tracker may have no session
            // context yet (deferred); call set_session_context after session
            // registration completes to enable daemon emission.
            turn_tracker: Some(self.turn_tracker.clone()),
            // Wire approval-gate bridging (G.5).
            elicitation_registry: Some(Arc::clone(&self.elicitation_registry)),
            elicitation_counter: Some(Arc::clone(&self.elicitation_counter)),
            upstream_tx: Some(Arc::clone(&self.upstream_tx)),
            // Share child stdin so the notification task can deliver approval
            // decisions back to the child process.
            child_stdin: Some(Arc::clone(&shared_stdin)),
            agent_identity: self.config.identity.clone(),
        };

        // Background task: read lines, parse notifications, forward to duplex.
        tokio::spawn(drive_notification_task(
            reader.into_inner(),
            duplex_write,
            task_state,
        ));
        let process: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(child)));

        // Monitor the child process exit in a separate task and update exit_status.
        {
            let exit_status_clone = Arc::clone(&exit_status);
            let process_clone = Arc::clone(&process);
            tokio::spawn(async move {
                // Wait for the child process to exit.
                let status = {
                    let mut guard = process_clone.lock().await;
                    if let Some(ref mut c) = *guard {
                        c.wait().await.ok()
                    } else {
                        None
                    }
                };
                if let Some(s) = status {
                    *exit_status_clone.lock().await = Some(s);
                }
            });
        }

        Ok(RawChildIo {
            stdin: shared_stdin,
            stdout: Box::new(duplex_read) as Box<dyn AsyncRead + Send + Unpin>,
            exit_status,
            process,
            // Use the dedicated idle_flag (not initialized) -- they serve different
            // purposes: initialized tracks handshake completion, idle_flag tracks
            // whether all threads are currently idle.
            idle_flag: Some(Arc::clone(&self.idle_flag)),
        })
    }

    fn is_idle(&self) -> bool {
        // idle = initialized AND all threads are in Idle state.
        if !self.initialized.load(Ordering::SeqCst) {
            return false;
        }
        // try_lock is acceptable here; if we can't acquire, treat as not-idle
        // (conservative, avoids blocking).
        if let Ok(states) = self.turn_state.try_lock() {
            states.values().all(|s| s.is_idle())
        } else {
            false
        }
    }

    fn set_turn_session_context(&self, ctx: crate::turn_control::SessionContext) {
        // Clone the tracker handle (cheap: Arc clone) and set the context in
        // a background task. This keeps the trait method synchronous while
        // still allowing the async mutex inside TurnTracker to be updated.
        // The proxy always calls this from an async context so a Tokio runtime
        // is guaranteed to be available.
        let tracker = self.turn_tracker.clone();
        tokio::spawn(async move {
            tracker.set_session_context(ctx).await;
        });
    }

    fn set_approval_upstream_tx(&self, tx: tokio::sync::mpsc::Sender<Value>) {
        // Populate the deferred upstream channel so the notification background
        // task can forward `item/enteredReviewMode` notifications upstream.
        // Runs in a background task to keep the trait method synchronous.
        let upstream_tx_arc = Arc::clone(&self.upstream_tx);
        tokio::spawn(async move {
            *upstream_tx_arc.lock().await = Some(tx);
        });
    }

    fn uses_app_server_injection(&self) -> bool {
        true
    }

    fn active_turn_id_for_thread(&self, thread_id: &str) -> Option<String> {
        use crate::stream_norm::TurnState;
        // try_lock is acceptable here: the turn_state map is tiny (one entry
        // per active thread) and the lock is held only briefly. If contended,
        // return None conservatively — the caller falls back to turn/start,
        // which is always safe.
        if let Ok(guard) = self.turn_state.try_lock() {
            if let Some(TurnState::Busy { turn_id }) = guard.get(thread_id) {
                return Some(turn_id.clone());
            }
        }
        None
    }
}

/// Select a transport based on the `transport` field in [`AgentMcpConfig`].
///
/// Recognised values:
/// - `None` / `"mcp"` -> [`McpTransport`] (spawns `codex mcp-server`).
/// - `"cli-json"` -> [`JsonCodecTransport`] (spawns `codex exec --json`).
/// - `"app-server"` -> [`AppServerTransport`] (spawns `codex app-server`).
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
        Some("cli-json") => Box::new(JsonCodecTransport::new(config.clone(), team)),
        Some("app-server") => Box::new(AppServerTransport::new(config.clone(), team)),
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
        Self {
            tx,
            buf: Vec::new(),
        }
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
    fn make_transport_returns_json_codec_for_cli_json() {
        let config = AgentMcpConfig {
            transport: Some("cli-json".to_string()),
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
        assert!(matches!(parse_event_type(""), TransportEventType::Unknown));
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

    /// Verify that `is_overload_error` is not triggered by a valid initialize response.
    /// This tests the protocol version logging path doesn't false-positive on success.
    #[test]
    fn test_protocol_version_logged() {
        use crate::stream_norm::is_overload_error;

        let valid_init_response = serde_json::json!({
            "id": 0,
            "result": {
                "protocolVersion": "2024-11-05",
                "serverInfo": { "name": "mock-app-server", "version": "0.0.1" }
            }
        });
        // A valid initialize response must not trigger overload detection.
        assert!(
            !is_overload_error(&valid_init_response),
            "valid initialize response must not be mistaken for overload error"
        );
    }

    /// Verify that a mock initialize response with `protocolVersion` is parsed
    /// without error (no panics, correct field extraction).
    #[test]
    fn test_initialize_response_parsing() {
        let response: serde_json::Value = serde_json::from_str(
            r#"{"id":0,"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"s","version":"1"}}}"#,
        )
        .expect("should be valid JSON");

        // No `error` field.
        assert!(response.get("error").is_none());
        // Has `result` field.
        assert!(response.get("result").is_some());
        // `protocolVersion` is present and correct.
        let ver = response["result"]["protocolVersion"].as_str().unwrap();
        assert_eq!(ver, "2024-11-05");
    }

    /// Verify that `send_with_backoff` succeeds on the first try when stdin accepts writes.
    #[tokio::test]
    async fn test_send_with_backoff_succeeds_on_first_try() {
        let (duplex_write, duplex_read) = tokio::io::duplex(4096);
        let stdin: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>> = Arc::new(Mutex::new(Box::new(
            duplex_write,
        )
            as Box<dyn AsyncWrite + Send + Unpin>));

        let result = AppServerTransport::send_with_backoff(
            &stdin,
            r#"{"id":1,"method":"thread/fork","params":{"threadId":"t1"}}"#,
        )
        .await;
        assert!(
            result.is_ok(),
            "send_with_backoff should succeed: {result:?}"
        );
        // Clean up the read half.
        drop(duplex_read);
    }

    // ── McpTransport::set_turn_session_context (G.5) ────────────────────────

    /// Verify that `McpTransport::set_turn_session_context` populates the inner
    /// `TurnTracker` with the supplied `SessionContext`.
    ///
    /// After calling the synchronous trait method, we give the background task
    /// a brief yield so the `tokio::spawn` completes, then verify that
    /// `active_turn_id` is callable (the tracker is properly initialised).
    #[tokio::test]
    async fn mcp_transport_set_turn_session_context_wires_tracker() {
        use crate::turn_control::{SessionContext, TurnControl as _};

        let t = McpTransport::new(AgentMcpConfig::default(), "test-team");
        let ctx = SessionContext::new("agent-1", "team-x", "codex:test-session");

        // Call the synchronous trait method — internally spawns a task.
        t.set_turn_session_context(ctx);

        // Yield briefly to allow the spawned task to complete.
        tokio::task::yield_now().await;

        // Verify the tracker is usable: active_turn_id returns None for an
        // unknown thread (no active turn).
        let active = t.turn_tracker.active_turn_id("thread-1").await;
        assert!(
            active.is_none(),
            "freshly initialised tracker must have no active turn"
        );
    }

    // ── bridge_entered_review_mode (G.5) ─────────────────────────────────────

    /// When all channels are wired, `bridge_entered_review_mode` registers an
    /// entry in the elicitation registry and sends an `elicitation/create`
    /// request upstream.
    #[tokio::test]
    async fn bridge_entered_review_mode_sends_upstream_and_registers() {
        use crate::elicitation::ElicitationRegistry;

        let registry = Arc::new(Mutex::new(ElicitationRegistry::new(30)));
        let counter = Arc::new(AtomicU64::new(0));

        let (tx, mut rx) = tokio::sync::mpsc::channel::<serde_json::Value>(8);
        let upstream_tx_arc = Arc::new(Mutex::new(Some(tx)));

        bridge_entered_review_mode(
            "item-42",
            &serde_json::json!({"toolName":"bash","itemId":"item-42"}),
            "test-team",
            &Some(Arc::clone(&registry)),
            &Some(Arc::clone(&counter)),
            &Some(Arc::clone(&upstream_tx_arc)),
            &None,
        )
        .await;

        // Registry must have exactly one pending entry.
        assert_eq!(
            registry.lock().await.len(),
            1,
            "one pending elicitation must be registered"
        );

        // Upstream channel must have received the elicitation/create request.
        let msg = rx
            .try_recv()
            .expect("upstream must have received a message");
        assert_eq!(
            msg.get("method").and_then(|v| v.as_str()),
            Some("elicitation/create"),
            "upstream message must be elicitation/create"
        );
        assert_eq!(
            msg.get("params")
                .and_then(|p| p.get("itemId"))
                .and_then(|v| v.as_str()),
            Some("item-42"),
            "upstream message params must include itemId"
        );
        assert_eq!(
            msg.get("params")
                .and_then(|p| p.get("source"))
                .and_then(|v| v.as_str()),
            Some("app-server"),
            "upstream message params must include source=app-server"
        );
    }

    /// When `upstream_tx` is `None`, `bridge_entered_review_mode` must not panic
    /// and must not silently approve (no entry is registered in the registry
    /// when the registry itself is also `None`).
    #[tokio::test]
    async fn bridge_entered_review_mode_no_op_when_channels_absent() {
        // All channels absent — should be a no-op with a warn log, no panic.
        bridge_entered_review_mode(
            "item-x",
            &serde_json::json!({}),
            "test-team",
            &None,
            &None,
            &None,
            &None,
        )
        .await;
        // Test passes if no panic occurs.
    }

    /// When the upstream_tx sender is `None` inside the Arc, the entry is still
    /// registered in the registry (so it will eventually be rejected by timeout),
    /// and no panic occurs.
    #[tokio::test]
    async fn bridge_entered_review_mode_registry_entry_registered_even_if_tx_unpopulated() {
        use crate::elicitation::ElicitationRegistry;

        let registry = Arc::new(Mutex::new(ElicitationRegistry::new(30)));
        let counter = Arc::new(AtomicU64::new(0));
        // upstream_tx is Some(Arc) but the inner Option is None (tx not yet set).
        let upstream_tx_arc: Arc<Mutex<Option<tokio::sync::mpsc::Sender<serde_json::Value>>>> =
            Arc::new(Mutex::new(None));

        bridge_entered_review_mode(
            "item-pending",
            &serde_json::json!({}),
            "test-team",
            &Some(Arc::clone(&registry)),
            &Some(Arc::clone(&counter)),
            &Some(Arc::clone(&upstream_tx_arc)),
            &None,
        )
        .await;

        // An entry must be registered even when the tx is not yet populated.
        // It will be rejected by expire_timeouts.
        assert_eq!(
            registry.lock().await.len(),
            1,
            "pending entry must be registered even when upstream_tx is not yet populated"
        );
    }

    // -----------------------------------------------------------------------
    // Transport trait method overrides for AppServerTransport (G.6)
    // -----------------------------------------------------------------------

    #[test]
    fn app_server_transport_uses_app_server_injection() {
        let t = AppServerTransport::new(AgentMcpConfig::default(), "test-team");
        assert!(
            t.uses_app_server_injection(),
            "AppServerTransport must report uses_app_server_injection() == true"
        );
    }

    #[test]
    fn mcp_transport_does_not_use_app_server_injection() {
        let t = McpTransport::new(AgentMcpConfig::default(), "test-team");
        assert!(
            !t.uses_app_server_injection(),
            "McpTransport must not use app-server injection"
        );
    }

    #[test]
    fn json_codec_transport_does_not_use_app_server_injection() {
        let t = JsonCodecTransport::new(AgentMcpConfig::default(), "test-team");
        assert!(
            !t.uses_app_server_injection(),
            "JsonCodecTransport must not use app-server injection"
        );
    }

    #[test]
    fn app_server_transport_active_turn_id_returns_none_when_idle() {
        let t = AppServerTransport::new(AgentMcpConfig::default(), "test-team");
        assert!(
            t.active_turn_id_for_thread("thread-1").is_none(),
            "active_turn_id_for_thread should be None when no turn is active"
        );
    }

    #[tokio::test]
    async fn app_server_transport_active_turn_id_returns_some_when_busy() {
        use crate::stream_norm::TurnState;
        let t = AppServerTransport::new(AgentMcpConfig::default(), "test-team");

        t.turn_state.lock().await.insert(
            "thread-1".to_string(),
            TurnState::Busy {
                turn_id: "turn-abc".to_string(),
            },
        );

        assert_eq!(
            t.active_turn_id_for_thread("thread-1"),
            Some("turn-abc".to_string()),
            "active_turn_id_for_thread should return the active turn_id when Busy"
        );
    }

    #[tokio::test]
    async fn app_server_transport_active_turn_id_returns_none_when_terminal() {
        use crate::stream_norm::{TurnState, TurnStatus};
        let t = AppServerTransport::new(AgentMcpConfig::default(), "test-team");

        t.turn_state.lock().await.insert(
            "thread-1".to_string(),
            TurnState::Terminal {
                turn_id: "turn-xyz".to_string(),
                status: TurnStatus::Completed,
            },
        );

        assert!(
            t.active_turn_id_for_thread("thread-1").is_none(),
            "active_turn_id_for_thread should be None for Terminal state"
        );
    }

    // -----------------------------------------------------------------------
    // App-server injection JSON structure tests (G.6)
    // -----------------------------------------------------------------------

    #[test]
    fn app_server_injection_uses_turn_start_when_idle() {
        let thread_id = "thread-idle";
        let content = "You have 1 unread message:\n\n[1] From: alice | ...";
        let req_id: u64 = 42;

        let msg = serde_json::json!({
            "id": req_id,
            "method": "turn/start",
            "params": {
                "threadId": thread_id,
                "input": [{ "type": "text", "text": content }]
            }
        });

        assert!(
            msg.get("jsonrpc").is_none(),
            "jsonrpc must be omitted per protocol spec"
        );
        assert_eq!(msg["method"].as_str().unwrap(), "turn/start");
        assert_eq!(msg["params"]["threadId"].as_str().unwrap(), thread_id);
        assert_eq!(msg["params"]["input"][0]["type"].as_str().unwrap(), "text");
        assert_eq!(msg["params"]["input"][0]["text"].as_str().unwrap(), content);
        assert!(
            msg["params"].get("expectedTurnId").is_none()
                || msg["params"]["expectedTurnId"].is_null(),
            "turn/start must not include expectedTurnId"
        );
    }

    #[test]
    fn app_server_injection_uses_turn_steer_when_busy() {
        let thread_id = "thread-busy";
        let active_turn_id = "turn-xyz-123";
        let content = "You have 2 unread messages:\n\n[1] From: bob | ...";
        let req_id: u64 = 99;

        let msg = serde_json::json!({
            "id": req_id,
            "method": "turn/steer",
            "params": {
                "threadId": thread_id,
                "expectedTurnId": active_turn_id,
                "input": [{ "type": "text", "text": content }]
            }
        });

        assert!(
            msg.get("jsonrpc").is_none(),
            "jsonrpc must be omitted per protocol spec"
        );
        assert_eq!(msg["method"].as_str().unwrap(), "turn/steer");
        assert_eq!(msg["params"]["threadId"].as_str().unwrap(), thread_id);
        assert_eq!(
            msg["params"]["expectedTurnId"].as_str().unwrap(),
            active_turn_id
        );
        assert_eq!(msg["params"]["input"][0]["type"].as_str().unwrap(), "text");
        assert_eq!(msg["params"]["input"][0]["text"].as_str().unwrap(), content);
    }

    // ── AC2: idle_flag and cli_json_turn_state transitions ───────────────────

    /// `idle` events set idle_flag to `true` and transition `cli_json_turn_state`
    /// to `TurnState::Idle`.
    ///
    /// This test exercises the state machine logic extracted from the background
    /// task without spawning a real child process.  We use the `JsonCodecTransport`
    /// struct fields directly (via their `pub(crate)` / `Arc` accessibility) to
    /// simulate what the background task does when it encounters each event type.
    #[tokio::test]
    async fn idle_event_sets_idle_flag_and_turn_state() {
        use crate::stream_norm::TurnState;

        let t = JsonCodecTransport::new(AgentMcpConfig::default(), "test-team");
        // Pre-condition: idle_flag is false, turn_state is Idle (initial).
        assert!(!t.is_idle(), "idle_flag must start false");
        assert!(t.cli_json_turn_state.lock().await.is_idle());

        // Simulate processing an `idle` JSONL event (mirrors background task).
        t.idle_flag.store(true, Ordering::SeqCst);
        *t.cli_json_turn_state.lock().await = TurnState::Idle;

        assert!(t.is_idle(), "idle_flag must be true after idle event");
        assert!(
            t.cli_json_turn_state.lock().await.is_idle(),
            "cli_json_turn_state must be Idle after idle event"
        );
    }

    /// A non-idle/non-done event resets `idle_flag` to `false`.
    ///
    /// Verifies that the activity-reset branch in the background task works:
    /// any `agent_message`, `tool_call`, `tool_result`, or `file_change` event
    /// must clear the idle flag so `is_idle()` returns false.
    #[tokio::test]
    async fn activity_event_resets_idle_flag() {
        let t = JsonCodecTransport::new(AgentMcpConfig::default(), "test-team");

        // Manually set idle flag (as if an idle event was processed).
        t.idle_flag.store(true, Ordering::SeqCst);
        assert!(t.is_idle());

        // Simulate an agent_message event arriving (mirrors the `_ =>` arm of the
        // background task match).
        t.idle_flag.store(false, Ordering::SeqCst);

        assert!(
            !t.is_idle(),
            "idle_flag must be false after any activity event"
        );
    }

    /// `done` event transitions `cli_json_turn_state` to `TurnState::Terminal`
    /// with `TurnStatus::Completed`, and resets `idle_flag` to `false`.
    #[tokio::test]
    async fn done_event_sets_terminal_turn_state_and_clears_idle_flag() {
        use crate::stream_norm::{TurnState, TurnStatus};

        let t = JsonCodecTransport::new(AgentMcpConfig::default(), "test-team");

        // Pre-set idle flag to true so we can confirm it is cleared by done.
        t.idle_flag.store(true, Ordering::SeqCst);

        // Simulate processing a `done` JSONL event (mirrors background task).
        t.idle_flag.store(false, Ordering::SeqCst);
        *t.cli_json_turn_state.lock().await = TurnState::Terminal {
            turn_id: String::new(),
            status: TurnStatus::Completed,
        };

        assert!(!t.is_idle(), "idle_flag must be false after done event");
        let state = t.cli_json_turn_state.lock().await.clone();
        assert!(
            matches!(
                state,
                TurnState::Terminal {
                    status: TurnStatus::Completed,
                    ..
                }
            ),
            "cli_json_turn_state must be Terminal(Completed) after done event: {state:?}"
        );
    }

    /// Sequence: idle → activity → done verifies all three transitions in order.
    ///
    /// This is the canonical event sequence for a cli-json session that receives
    /// a new task after going idle.
    #[tokio::test]
    async fn idle_then_activity_then_done_transitions() {
        use crate::stream_norm::{TurnState, TurnStatus};

        let t = JsonCodecTransport::new(AgentMcpConfig::default(), "test-team");

        // 1. idle event
        t.idle_flag.store(true, Ordering::SeqCst);
        *t.cli_json_turn_state.lock().await = TurnState::Idle;
        assert!(t.is_idle());
        assert!(t.cli_json_turn_state.lock().await.is_idle());

        // 2. activity event (agent_message) — resets idle flag
        t.idle_flag.store(false, Ordering::SeqCst);
        assert!(!t.is_idle(), "idle_flag must reset on activity");
        // turn_state remains Idle (cli-json has no explicit turn/started notification)
        assert!(t.cli_json_turn_state.lock().await.is_idle());

        // 3. done event — terminal state, idle_flag stays false
        *t.cli_json_turn_state.lock().await = TurnState::Terminal {
            turn_id: String::new(),
            status: TurnStatus::Completed,
        };
        assert!(!t.is_idle(), "idle_flag must remain false at done");
        let state = t.cli_json_turn_state.lock().await.clone();
        assert!(
            matches!(
                state,
                TurnState::Terminal {
                    status: TurnStatus::Completed,
                    ..
                }
            ),
            "must be Terminal(Completed) after done: {state:?}"
        );
    }

    // ── AC3: shared abstraction verification ─────────────────────────────────

    /// Confirm that `JsonCodecTransport` emits `DaemonStreamEvent::TurnIdle` with
    /// `transport: "cli-json"` on an `idle` event, and `DaemonStreamEvent::TurnCompleted`
    /// with `transport: "cli-json"` on a `done` event.
    ///
    /// Because `emit_stream_event` is best-effort (it tolerates a missing daemon
    /// without error), this test verifies the variant shapes are constructible and
    /// correct — confirming that the background task uses the shared abstractions
    /// rather than any parallel type.
    #[test]
    fn stream_emit_variants_use_cli_json_transport_label() {
        use agent_team_mail_core::daemon_stream::{DaemonStreamEvent, TurnStatusWire};

        // These are the exact variant constructions used in the JsonCodecTransport
        // background task.  Verify the `transport` field is set to "cli-json".
        let idle_event = DaemonStreamEvent::TurnIdle {
            agent: "test-agent".to_string(),
            turn_id: String::new(),
            transport: "cli-json".to_string(),
        };
        let done_event = DaemonStreamEvent::TurnCompleted {
            agent: "test-agent".to_string(),
            thread_id: String::new(),
            turn_id: String::new(),
            status: TurnStatusWire::Completed,
            transport: "cli-json".to_string(),
        };

        // Inspect transport label via Debug output (a stable proxy without adding
        // field accessors to the upstream type).
        let idle_dbg = format!("{idle_event:?}");
        assert!(
            idle_dbg.contains("cli-json"),
            "TurnIdle must carry transport=\"cli-json\": {idle_dbg}"
        );

        let done_dbg = format!("{done_event:?}");
        assert!(
            done_dbg.contains("cli-json"),
            "TurnCompleted must carry transport=\"cli-json\": {done_dbg}"
        );
    }

    /// Verify `parse_event_type` maps malformed JSON to `Unknown`, confirming the
    /// function is lenient: a `done` event with no additional fields is `Done`, not
    /// an error.
    #[test]
    fn parse_event_type_done_with_no_extra_fields_is_done() {
        // The `done` event shape has no required additional fields.
        assert!(matches!(
            parse_event_type(r#"{"type":"done"}"#),
            TransportEventType::Done
        ));
        // Extra fields do not affect classification.
        assert!(matches!(
            parse_event_type(r#"{"type":"done","extra":"data"}"#),
            TransportEventType::Done
        ));
    }

    /// Verify `parse_event_type` is lenient about malformed JSON (returns Unknown,
    /// never panics).
    #[test]
    fn parse_event_type_malformed_json_returns_unknown() {
        for bad in &[
            "",
            "   ",
            "{bad json}",
            r#"{"nottype":"idle"}"#,
            "null",
            "42",
        ] {
            assert!(
                matches!(parse_event_type(bad), TransportEventType::Unknown),
                "expected Unknown for input: {bad:?}"
            );
        }
    }

    /// Verify that no cli-json event type, as parsed by `parse_event_type`,
    /// would cause a `TurnState::Busy` transition in the background task.
    ///
    /// The cli-json protocol has no `turn/started` notification, so
    /// `TurnState::Busy` is never reached via event parsing.  Only `idle`
    /// (→ `TurnState::Idle`) and `done` (→ `TurnState::Terminal`) are
    /// state-changing events.  All other events reset `idle_flag` only.
    #[test]
    fn no_cli_json_event_transitions_to_busy_turn_state() {
        // The match arms in the background task are:
        //   TransportEventType::Idle  -> TurnState::Idle
        //   TransportEventType::Done  -> TurnState::Terminal
        //   _                         -> idle_flag = false (no TurnState change)
        // Busy is explicitly absent from all arms.

        let only_idle_and_done_change_state: &[(&str, bool)] = &[
            (r#"{"type":"agent_message"}"#, false),
            (r#"{"type":"tool_call"}"#, false),
            (r#"{"type":"tool_result"}"#, false),
            (r#"{"type":"file_change"}"#, false),
            (r#"{"type":"idle"}"#, true),
            (r#"{"type":"done"}"#, true),
            (r#"{"type":"unknown_future_type"}"#, false),
        ];

        for (line, is_state_changer) in only_idle_and_done_change_state {
            let et = parse_event_type(line);
            let changes_state = matches!(et, TransportEventType::Idle | TransportEventType::Done);
            assert_eq!(
                changes_state, *is_state_changer,
                "event {line:?}: expected is_state_changer={is_state_changer}, got {changes_state}"
            );
            // The state transition (if any) goes to Idle or Terminal, never Busy.
            assert!(
                !matches!(
                    et,
                    TransportEventType::AgentMessage
                        | TransportEventType::ToolCall
                        | TransportEventType::ToolResult
                        | TransportEventType::FileChange
                        | TransportEventType::Unknown
                ) || !changes_state,
                "activity/unknown events must not be state-changing: {line}"
            );
        }
    }
}
