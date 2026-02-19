//! `serve` subcommand — start the MCP proxy server.
//!
//! Reads MCP JSON-RPC messages from stdin, proxies to a lazily spawned `codex
//! mcp-server` child process, and writes responses to stdout. See [`crate::proxy`]
//! for the core proxy logic and [`crate::framing`] for framing details.

use crate::cli::ServeArgs;
use crate::config::resolve_config;
use crate::proxy::ProxyServer;
use std::path::PathBuf;

/// Run the `serve` subcommand.
///
/// Resolves configuration, applies CLI overrides, then enters the proxy loop
/// which reads from stdin and writes to stdout until EOF.
///
/// # Errors
///
/// Returns an error if configuration resolution fails or the proxy loop
/// encounters an unrecoverable I/O error.
pub async fn run(config_path: &Option<PathBuf>, args: ServeArgs) -> anyhow::Result<()> {
    // Resolve configuration from file/env/defaults
    let resolved = resolve_config(config_path.as_deref())?;
    let mut config = resolved.agent_mcp;

    // Apply CLI argument overrides
    if let Some(ref identity) = args.identity {
        config.identity = Some(identity.clone());
    }
    if let Some(ref model) = args.model {
        config.model = Some(model.clone());
    }
    if args.fast {
        if let Some(ref fast_model) = config.fast_model.clone() {
            config.model = Some(fast_model.clone());
        }
    }
    if let Some(ref sandbox) = args.sandbox {
        config.sandbox = sandbox.clone();
    }
    if let Some(ref policy) = args.approval_policy {
        config.approval_policy = policy.clone();
    }
    if let Some(timeout_secs) = args.timeout {
        config.request_timeout_secs = timeout_secs;
    }

    // Set up upstream I/O (stdin for reading, stdout for writing)
    let upstream_in = tokio::io::stdin();
    let upstream_out = tokio::io::stdout();

    // Use the ATM core team name for session registration and lock files.
    let team = resolved.core.default_team.clone();
    let mut proxy = ProxyServer::new_with_team(config, team);
    proxy.run(upstream_in, upstream_out).await
}
