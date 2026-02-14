//! CLI command dispatch and execution

use anyhow::Result;
use clap::{Parser, Subcommand};

mod broadcast;
mod cleanup;
mod config_cmd;
mod error;
mod inbox;
mod members;
mod read;
mod request;
mod send;
mod status;
mod teams;

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

    /// Send a message and wait for a response (polling)
    Request(request::RequestArgs),

    /// Show inbox summary for team members
    Inbox(inbox::InboxArgs),

    /// List all teams on this machine
    Teams(teams::TeamsArgs),

    /// List agents in a team
    Members(members::MembersArgs),

    /// Show team status overview
    Status(status::StatusArgs),

    /// Show effective configuration
    Config(config_cmd::ConfigArgs),

    /// Apply retention policies to clean up old messages
    Cleanup(cleanup::CleanupArgs),
}

impl Cli {
    /// Execute the CLI command
    pub fn execute(self) -> Result<()> {
        match self.command {
            Commands::Send(args) => send::execute(args),
            Commands::Broadcast(args) => broadcast::execute(args),
            Commands::Read(args) => read::execute(args),
            Commands::Request(args) => request::execute(args),
            Commands::Inbox(args) => inbox::execute(args),
            Commands::Teams(args) => teams::execute(args),
            Commands::Members(args) => members::execute(args),
            Commands::Status(args) => status::execute(args),
            Commands::Config(args) => config_cmd::execute(args),
            Commands::Cleanup(args) => cleanup::execute(args),
        }
    }
}
