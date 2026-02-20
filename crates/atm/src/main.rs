//! atm - Mail-like messaging for Claude agent teams
//!
//! A thin CLI over the `~/.claude/teams/` file-based API, providing
//! send, read, broadcast, and inbox commands with atomic file I/O.

use clap::Parser;
use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};

mod commands;
mod util;

use commands::Cli;

fn main() {
    let cli = Cli::parse();

    if let Err(e) = cli.execute() {
        emit_event_best_effort(EventFields {
            level: "error",
            source: "atm",
            action: "command_error",
            team: std::env::var("ATM_TEAM").ok(),
            session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
            result: Some("error".to_string()),
            error: Some(e.to_string()),
            ..Default::default()
        });
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
