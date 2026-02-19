//! CLI argument types for atm-agent-mcp.
//!
//! Defines the top-level [`Cli`] struct and all subcommand [`Args`] using
//! clap's derive macros. Each subcommand maps to a module in [`commands`].

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// MCP proxy for managing Codex agent sessions with ATM team integration
#[derive(Parser, Debug)]
#[command(name = "atm-agent-mcp", version, about)]
pub struct Cli {
    /// Path to .atm.toml config file (default: auto-detected)
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

/// Top-level subcommands
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Start MCP proxy server
    Serve(ServeArgs),
    /// Show resolved configuration
    Config(ConfigArgs),
    /// List and manage agent sessions
    Sessions(SessionsArgs),
    /// Display saved session summary
    Summary(SummaryArgs),
}

/// Arguments for the `serve` subcommand
#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Identity override (overrides config/env)
    #[arg(long)]
    pub identity: Option<String>,

    /// Role preset to use
    #[arg(long)]
    pub role: Option<String>,

    /// Model override
    #[arg(long)]
    pub model: Option<String>,

    /// Sandbox mode
    #[arg(long)]
    pub sandbox: Option<String>,

    /// Approval policy
    #[arg(long, name = "approval-policy")]
    pub approval_policy: Option<String>,

    /// Resume most recent session, or a specific agent-id
    #[arg(long)]
    pub resume: Option<Option<String>>,

    /// Use fast model profile
    #[arg(long)]
    pub fast: bool,

    /// Enable subagent mode
    #[arg(long)]
    pub subagents: bool,

    /// Read-only mode
    #[arg(long, conflicts_with = "explore")]
    pub readonly: bool,

    /// Explore mode (same as readonly, for discoverability)
    #[arg(long)]
    pub explore: bool,

    /// Request timeout in seconds
    #[arg(long)]
    pub timeout: Option<u64>,
}

/// Arguments for the `config` subcommand
#[derive(Args, Debug)]
pub struct ConfigArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Arguments for the `sessions` subcommand
#[derive(Args, Debug)]
pub struct SessionsArgs {
    /// Filter by repository name
    #[arg(long)]
    pub repo: Option<String>,

    /// Filter by identity
    #[arg(long)]
    pub identity: Option<String>,

    /// Prune closed/stale sessions
    #[arg(long)]
    pub prune: bool,
}

/// Arguments for the `summary` subcommand
#[derive(Args, Debug)]
pub struct SummaryArgs {
    /// Agent ID to show summary for
    pub agent_id: String,
}
