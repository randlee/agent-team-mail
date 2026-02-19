//! `serve` subcommand — MCP proxy server (stub).
//!
//! Full implementation is planned for Sprint A.2+. This stub exists so the
//! CLI structure and binary are buildable before the MCP protocol is wired up.

use crate::cli::ServeArgs;

/// Run the `serve` subcommand.
///
/// # Errors
///
/// Currently infallible. Returns `Ok(())` after printing a stub message.
pub async fn run(_args: ServeArgs) -> anyhow::Result<()> {
    println!("atm-agent-mcp serve: MCP proxy server not yet implemented (Sprint A.2+)");
    Ok(())
}
