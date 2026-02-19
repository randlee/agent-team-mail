//! `summary` subcommand — display saved session summary (stub).
//!
//! Full session summary storage is planned for Sprint A.3+. This stub prints
//! a placeholder message for the requested agent ID.

use crate::cli::SummaryArgs;

/// Run the `summary` subcommand.
///
/// # Errors
///
/// Currently infallible. Returns `Ok(())` after printing the stub message.
pub async fn run(args: SummaryArgs) -> anyhow::Result<()> {
    println!("No summary available for agent {}.", args.agent_id);
    Ok(())
}
