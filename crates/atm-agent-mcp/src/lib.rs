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
pub mod summary;
pub mod tools;
pub mod transport;

#[doc(inline)]
pub use transport::{MockTransport, MockTransportHandle, RawChildIo};
