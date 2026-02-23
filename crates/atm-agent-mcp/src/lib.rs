//! atm-agent-mcp library crate.
//!
//! Provides the MCP proxy core, framing, tool schemas, configuration, and CLI
//! types for the `atm-agent-mcp` binary. Exposed as a library for integration
//! testing and potential reuse.

pub mod atm_tools;
pub mod audit;
pub mod cli;
pub mod commands;
pub mod config;
pub mod context;
pub mod elicitation;
pub mod framing;
pub mod inject;
pub mod lifecycle;
pub mod lifecycle_emit;
pub mod lock;
pub mod mail_inject;
pub mod proxy;
pub mod session;
pub mod stdin_queue;
pub mod stream_emit;
pub mod stream_norm;
pub mod summary;
pub mod tools;
pub mod transport;
pub mod turn_control;
pub mod watch_stream;

#[doc(inline)]
pub use transport::{MockTransport, MockTransportHandle, RawChildIo};

/// Test-only helpers for exercising transport factory logic from integration tests.
///
/// [`transport::make_transport`] is `pub(crate)`, so integration tests (which
/// live outside the crate) cannot call it directly.  This module re-exports a
/// thin wrapper that returns an opaque `Box<dyn Send + Sync>`.
///
/// Do NOT use in production code paths.
#[doc(hidden)]
pub mod transport_factory_test {
    use crate::config::AgentMcpConfig;

    /// Opaque transport handle returned by [`make_transport_for_test`].
    ///
    /// The only supported operation is `drop`.  Dropping exercises the
    /// `transport_shutdown` event emission path.
    pub struct OpaqueTransport(
        #[expect(
            dead_code,
            reason = "opaque keepalive: holds the transport alive until dropped; \
                      only operation supported is Drop"
        )]
        Box<dyn Send + Sync>,
    );

    /// Thin wrapper around [`crate::transport::make_transport`] for integration tests.
    ///
    /// Returns an [`OpaqueTransport`] that can be dropped to trigger the
    /// `transport_shutdown` event.
    pub fn make_transport_for_test(config: &AgentMcpConfig, team: &str) -> OpaqueTransport {
        OpaqueTransport(Box::new(crate::transport::make_transport(config, team)))
    }
}

/// Test-only helpers for exercising `AppServerTransport` from integration tests.
///
/// `AppServerTransport` is `pub(crate)`, so integration tests cannot construct
/// it directly. This module provides thin wrappers for test-only operations.
///
/// Do NOT use in production code paths.
#[doc(hidden)]
pub mod app_server_test {
    use std::sync::Arc;
    use tokio::io::AsyncWrite;
    use tokio::sync::Mutex;

    use crate::config::AgentMcpConfig;
    use crate::transport::AppServerTransport;

    /// Wrapper around `AppServerTransport` for integration tests.
    pub struct TestAppServerTransport {
        inner: AppServerTransport,
    }

    impl TestAppServerTransport {
        /// Create a new test transport for the given team name.
        pub fn new(team: &str) -> Self {
            Self {
                inner: AppServerTransport::new(AgentMcpConfig::default(), team),
            }
        }

        /// Perform the initialize/initialized handshake and start the background task.
        ///
        /// # Errors
        ///
        /// Returns an error if the handshake fails.
        pub async fn spawn_from_io(
            &self,
            stdin: Box<dyn AsyncWrite + Send + Unpin>,
            stdout: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        ) -> anyhow::Result<crate::transport::RawChildIo> {
            self.inner.spawn_from_io(stdin, stdout).await
        }

        /// Fork a new thread via the transport.
        ///
        /// # Errors
        ///
        /// Returns an error if the fork request fails.
        pub async fn fork_thread(
            &self,
            stdin: &Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
            thread_id: &str,
        ) -> anyhow::Result<serde_json::Value> {
            self.inner.fork_thread(stdin, thread_id).await
        }
    }
}

/// Test-only helpers for exercising `JsonCodecTransport` cli-json state machine
/// from integration tests.
///
/// `parse_event_type` and `TransportEventType` are `pub(crate)`, so integration
/// tests cannot call them directly.  This module re-exports thin wrappers so
/// the integration test can drive the state machine without coupling to internal
/// type names.
///
/// Do NOT use in production code paths.
#[doc(hidden)]
pub mod cli_json_test {
    /// Re-export of the cli-json event classification result.
    ///
    /// The variants mirror [`crate::transport::TransportEventType`] exactly.
    #[derive(Debug, PartialEq, Eq)]
    pub enum CliJsonEventKind {
        /// `{"type":"agent_message"}`
        AgentMessage,
        /// `{"type":"tool_call"}`
        ToolCall,
        /// `{"type":"tool_result"}`
        ToolResult,
        /// `{"type":"file_change"}`
        FileChange,
        /// `{"type":"idle"}`
        Idle,
        /// `{"type":"done"}`
        Done,
        /// Any unrecognised or malformed event.
        Unknown,
    }

    /// Classify a JSONL line from a `codex exec --json` child process.
    ///
    /// This is a thin public wrapper around `crate::transport::parse_event_type`
    /// intended exclusively for integration-test use.  Returns the classification
    /// as a [`CliJsonEventKind`] so tests do not depend on the internal enum.
    pub fn classify_event(line: &str) -> CliJsonEventKind {
        use crate::transport::TransportEventType;
        match crate::transport::parse_event_type(line) {
            TransportEventType::AgentMessage => CliJsonEventKind::AgentMessage,
            TransportEventType::ToolCall => CliJsonEventKind::ToolCall,
            TransportEventType::ToolResult => CliJsonEventKind::ToolResult,
            TransportEventType::FileChange => CliJsonEventKind::FileChange,
            TransportEventType::Idle => CliJsonEventKind::Idle,
            TransportEventType::Done => CliJsonEventKind::Done,
            TransportEventType::Unknown => CliJsonEventKind::Unknown,
        }
    }
}
