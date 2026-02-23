//! atm - Mail-like messaging for Claude agent teams
//!
//! A thin CLI over the `~/.claude/teams/` file-based API, providing
//! send, read, broadcast, and inbox commands with atomic file I/O.

use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::logging;
use clap::Parser;

mod commands;
mod util;

use commands::Cli;

fn main() {
    let _guards = logging::init_unified(
        "atm",
        logging::UnifiedLogMode::ProducerFanIn {
            daemon_socket: agent_team_mail_core::daemon_client::daemon_socket_path()
                .unwrap_or_else(|_| std::env::temp_dir().join("atm-daemon.sock")),
            fallback_spool_dir: agent_team_mail_core::logging_event::default_spool_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("atm-spool")),
        },
    )
    .unwrap_or_else(|_| logging::init_stderr_only());

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
