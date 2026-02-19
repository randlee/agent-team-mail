//! atm-agent-mcp library crate.
//!
//! Provides the MCP proxy core, framing, tool schemas, configuration, and CLI
//! types for the `atm-agent-mcp` binary. Exposed as a library for integration
//! testing and potential reuse.

pub mod cli;
pub mod commands;
pub mod config;
pub mod framing;
pub mod proxy;
pub mod tools;
