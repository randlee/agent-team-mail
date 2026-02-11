//! atm - Mail-like messaging for Claude agent teams
//!
//! A thin CLI over the `~/.claude/teams/` file-based API, providing
//! send, read, broadcast, and inbox commands with atomic file I/O.

use clap::Parser;

mod commands;
mod util;

use commands::Cli;

fn main() {
    let cli = Cli::parse();

    if let Err(e) = cli.execute() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
