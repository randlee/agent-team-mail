//! Transport abstraction for the Codex child process.
//!
//! [`CodexTransport`] is the trait seam between [`crate::proxy::ProxyServer`]
//! and the underlying child-process implementation.  The only shipping
//! implementation is [`McpTransport`], which spawns `codex mcp-server` as a
//! subprocess exactly as the inline code in `spawn_child` did before this
//! refactor.
//!
//! Sprint C.2b will add `JsonTransport` (newline-delimited JSON over stdin/
//! stdout without a child process) by implementing this same trait.
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

use std::process::ExitStatus;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};

use crate::config::AgentMcpConfig;

/// Raw I/O handles produced by a successful [`CodexTransport::spawn`] call.
///
/// The proxy converts this into its internal `ChildHandle` by wiring up the
/// background reader and wait tasks.  Keeping the two types distinct preserves
/// the existing proxy internals without change.
pub(crate) struct RawChildIo {
    /// Shared stdin writer.  The proxy shares this with timeout tasks so they
    /// can send `notifications/cancelled` to the child.
    pub stdin: Arc<Mutex<ChildStdin>>,
    /// Raw stdout reader, consumed by the proxy's background reader task.
    pub stdout: ChildStdout,
    /// Updated to `Some(status)` when the child process terminates.
    pub exit_status: Arc<Mutex<Option<ExitStatus>>>,
    /// The child process handle, retained for force-kill on proxy shutdown.
    pub process: Arc<Mutex<Option<Child>>>,
}

/// Abstracts the mechanism by which the proxy communicates with a Codex agent.
///
/// Implement this trait to swap in alternative transports (e.g. a test double
/// or the `JsonTransport` added in Sprint C.2b) without changing
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
}

/// Transport that spawns `codex mcp-server` as a child subprocess.
///
/// This is the only production transport.  It reproduces the exact spawn
/// logic that previously lived inline in `ProxyServer::spawn_child`.
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
        let shared_stdin = Arc::new(Mutex::new(stdin));
        let process: Arc<Mutex<Option<tokio::process::Child>>> =
            Arc::new(Mutex::new(Some(child)));

        Ok(RawChildIo {
            stdin: shared_stdin,
            stdout,
            exit_status,
            process,
        })
    }
}

/// Select a transport based on the `transport` field in [`AgentMcpConfig`].
///
/// Currently the only recognised value is `"mcp"` (the default).  Unknown
/// values fall back to `McpTransport` with a `tracing::warn`.
///
/// Returns a `Box<dyn CodexTransport>` so callers can store the transport
/// without knowing the concrete type.  Sprint C.2b will add `JsonTransport`
/// as a second branch here.
pub(crate) fn make_transport(config: &AgentMcpConfig, team: &str) -> Box<dyn CodexTransport> {
    // The `transport` field is `None` by default (not present in .atm.toml).
    // A value of `None` or `Some("mcp")` selects `McpTransport`.
    match config.transport.as_deref() {
        None | Some("mcp") => Box::new(McpTransport::new(config.clone(), team)),
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
    fn make_transport_falls_back_for_unknown() {
        // Unknown transport falls back to McpTransport without panic.
        let config = AgentMcpConfig {
            transport: Some("json".to_string()),
            ..Default::default()
        };
        let _t = make_transport(&config, "test-team");
    }
}
