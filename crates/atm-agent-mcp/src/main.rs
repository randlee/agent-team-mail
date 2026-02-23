//! atm-agent-mcp — MCP proxy for Codex agent sessions with ATM team integration.
//!
//! # Subcommands
//!
//! - `serve`    — Start the MCP proxy server (Sprint A.2+)
//! - `config`   — Show resolved configuration
//! - `sessions` — List and manage agent sessions (Sprint A.3+)
//! - `summary`  — Display saved session summary (Sprint A.3+)

use clap::Parser;
use agent_team_mail_core::logging;

use atm_agent_mcp::cli::{Cli, Commands};
use atm_agent_mcp::commands;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _guards = logging::init_unified(
        "atm-agent-mcp",
        logging::UnifiedLogMode::ProducerFanIn {
            daemon_socket: agent_team_mail_core::daemon_client::daemon_socket_path()
                .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/atm-daemon.sock")),
            fallback_spool_dir: agent_team_mail_core::logging_event::default_spool_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/atm-spool")),
        },
    )
    .unwrap_or_else(|_| logging::init_stderr_only());
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve(args) => commands::serve::run(&cli.config, args).await,
        Commands::Config(args) => commands::config_cmd::run(&cli.config, args).await,
        Commands::Sessions(args) => commands::sessions::run(args).await,
        Commands::Summary(args) => commands::summary::run(args).await,
    }
}
