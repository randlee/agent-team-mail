//! atm-agent-mcp library crate.
//!
//! Provides the MCP proxy core, framing, tool schemas, configuration, and CLI
//! types for the `atm-agent-mcp` binary. Exposed as a library for integration
//! testing and potential reuse.

pub mod cli;
pub mod commands;
pub mod config;
pub mod context;
pub mod framing;
pub mod inject;
pub mod lock;
pub mod proxy;
pub mod session;
pub mod tools;
