//! Configuration resolution for atm-agent-mcp.
//!
//! The entry point is [`resolve_config`], which loads ATM base config and
//! extracts the `[plugins.atm-agent-mcp]` section into an [`AgentMcpConfig`].
//!
//! See [`resolve`] for the full priority chain and [`types`] for all config types.

mod resolve;
mod types;

pub use resolve::{resolve_config, ResolvedConfig};
// Re-exported for use by command modules and future library consumers.
pub use types::{AgentMcpConfig, RolePreset};
