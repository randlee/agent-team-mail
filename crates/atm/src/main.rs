//! atm - Mail-like messaging for Claude agent teams
//!
//! A thin CLI over the `~/.claude/teams/` file-based API, providing
//! send, read, broadcast, and inbox commands with atomic file I/O.

use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::logging;
use clap::Parser;

mod commands;
mod consts;
mod util;

use commands::Cli;

fn main() {
    // Enable daemon auto-start for daemon-backed ATM commands.
    // Respect explicit caller override (e.g., tests setting "0").
    if std::env::var_os("ATM_DAEMON_AUTOSTART").is_none() {
        // SAFETY: process-local env mutation at startup before command execution.
        unsafe { std::env::set_var("ATM_DAEMON_AUTOSTART", "1") };
    }

    let _guards = logging::init_unified(
        "atm",
        logging::UnifiedLogMode::ProducerFanIn {
            daemon_socket: agent_team_mail_core::daemon_client::daemon_socket_path()
                .unwrap_or_else(|_| std::env::temp_dir().join("atm-daemon.sock")),
            fallback_spool_dir: agent_team_mail_core::home::get_home_dir()
                .map(|home| agent_team_mail_core::logging_event::configured_spool_dir(&home))
                .unwrap_or_else(|_| std::env::temp_dir().join("atm-spool")),
        },
    )
    .unwrap_or_else(|_| logging::init_stderr_only());

    let cli = Cli::parse();

    let exit_code = if let Err(e) = cli.execute() {
        let rendered = e.to_string();
        emit_event_best_effort(EventFields {
            level: "error",
            source: "atm",
            action: "command_error",
            team: std::env::var("ATM_TEAM").ok(),
            session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
            result: Some("error".to_string()),
            error: Some(rendered.clone()),
            ..Default::default()
        });
        if serde_json::from_str::<serde_json::Value>(&rendered).is_ok() {
            eprintln!("{rendered}");
        } else {
            eprintln!("Error: {rendered}");
        }
        1
    } else {
        0
    };

    // Flush the gh observability ledger writer thread before process exit.
    // The writer thread is fire-and-forget; without an explicit flush the OS
    // may kill it before it has written all pending records.  This flush is
    // synchronous and completes quickly (microseconds in practice).
    let _ = agent_team_mail_ci_monitor::flush_gh_observability_records();

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}
