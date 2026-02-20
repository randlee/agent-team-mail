//! `sessions` subcommand — list and manage agent sessions (stub).
//!
//! Full session management is planned for Sprint A.3+. This stub returns an
//! empty session list in JSON format.

use crate::cli::SessionsArgs;

/// Run the `sessions` subcommand.
///
/// # Errors
///
/// Currently infallible. Returns `Ok(())` after printing an empty session list.
pub async fn run(_args: SessionsArgs) -> anyhow::Result<()> {
    println!("[]");
    Ok(())
}
