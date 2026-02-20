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
    logging::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve(args) => commands::serve::run(&cli.config, args).await,
        Commands::Config(args) => commands::config_cmd::run(&cli.config, args).await,
        Commands::Sessions(args) => commands::sessions::run(args).await,
        Commands::Summary(args) => commands::summary::run(args).await,
    }
}
