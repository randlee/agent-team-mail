//! CLI command dispatch and execution

use anyhow::Result;
use clap::{Parser, Subcommand};

mod broadcast;
mod error;
mod inbox;
mod read;
mod send;

/// atm - Mail-like messaging for Claude agent teams
#[derive(Parser, Debug)]
#[command(
    name = "atm",
    version,
    about = "Mail-like messaging for Claude agent teams",
    long_about = "A thin CLI over the ~/.claude/teams/ file-based API for agent team messaging"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Send a message to a specific agent
    Send(send::SendArgs),

    /// Broadcast a message to all agents in a team
    Broadcast(broadcast::BroadcastArgs),

    /// Read messages from an inbox
    Read(read::ReadArgs),

    /// Show inbox summary for team members
    Inbox(inbox::InboxArgs),
}

impl Cli {
    /// Execute the CLI command
    pub fn execute(self) -> Result<()> {
        match self.command {
            Commands::Send(args) => send::execute(args),
            Commands::Broadcast(args) => broadcast::execute(args),
            Commands::Read(args) => read::execute(args),
            Commands::Inbox(args) => inbox::execute(args),
        }
    }
}
