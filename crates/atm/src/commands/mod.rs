//! CLI command dispatch and execution

use anyhow::Result;
use clap::{Parser, Subcommand};

mod error;
mod send;
mod teams;
mod members;
mod status;
mod config_cmd;

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
    /// List all teams on this machine
    Teams(teams::TeamsArgs),
    /// List agents in a team
    Members(members::MembersArgs),
    /// Show team status overview
    Status(status::StatusArgs),
    /// Show effective configuration
    Config(config_cmd::ConfigArgs),
}

impl Cli {
    /// Execute the CLI command
    pub fn execute(self) -> Result<()> {
        match self.command {
            Commands::Send(args) => send::execute(args),
            Commands::Teams(args) => teams::execute(args),
            Commands::Members(args) => members::execute(args),
            Commands::Status(args) => status::execute(args),
            Commands::Config(args) => config_cmd::execute(args),
        }
    }
}
