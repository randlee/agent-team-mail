//! CLI command dispatch and execution

use anyhow::Result;
use clap::{Parser, Subcommand};

mod ack;
mod bridge;
mod broadcast;
mod cleanup;
mod config_cmd;
mod daemon;
mod doctor;
mod gh;
mod inbox;
mod init;
pub mod launch;
mod logging_health;
mod logs;
mod mcp;
mod members;
mod monitor;
mod read;
mod register;
mod request;
mod runtime_adapter;
mod send;
mod spawn;
mod status;
mod subscribe;
mod tail;
mod teams;
mod wait;

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
    /// Acknowledge previously-read ATM messages as actioned
    Ack(ack::AckArgs),

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

    /// Interactive wrapper for spawning a new runtime teammate
    Spawn(spawn::SpawnArgs),

    /// Run daemon/team health diagnostics
    Doctor(doctor::DoctorArgs),

    /// GitHub CI monitor commands (daemon/plugin routed namespace)
    Gh(gh::GhArgs),

    /// Run continuous operational health monitor and send ATM alerts
    Monitor(monitor::MonitorArgs),

    /// Show effective configuration
    Config(config_cmd::ConfigArgs),

    /// Apply retention policies to clean up old messages
    Cleanup(cleanup::CleanupArgs),

    /// Bridge plugin commands (status, sync)
    Bridge(bridge::BridgeArgs),

    /// Daemon management commands (status)
    Daemon(daemon::DaemonArgs),

    /// Subscribe to agent state change notifications
    Subscribe(subscribe::SubscribeArgs),

    /// Unsubscribe from agent state change notifications
    Unsubscribe(subscribe::UnsubscribeArgs),

    /// Tail recent output from a Codex agent's log
    Tail(tail::TailArgs),

    /// Launch a new Codex agent via the daemon
    Launch(launch::LaunchArgs),

    /// View and follow the unified ATM daemon log
    Logs(logs::LogsArgs),

    /// Register this agent session with a team
    Register(register::RegisterArgs),

    /// MCP server setup and management (install for Claude Code, Codex, Gemini)
    Mcp(mcp::McpArgs),

    /// Install Claude Code hook wiring for ATM session coordination
    Init(init::InitArgs),
}

impl Cli {
    /// Execute the CLI command
    pub fn execute(self) -> Result<()> {
        match self.command {
            Commands::Ack(args) => ack::execute(args),
            Commands::Send(args) => send::execute(args),
            Commands::Broadcast(args) => broadcast::execute(args),
            Commands::Read(args) => read::execute(args),
            Commands::Request(args) => request::execute(args),
            Commands::Inbox(args) => inbox::execute(args),
            Commands::Teams(args) => teams::execute(args),
            Commands::Members(args) => members::execute(args),
            Commands::Status(args) => status::execute(args),
            Commands::Spawn(args) => spawn::execute(args),
            Commands::Doctor(args) => doctor::execute(args),
            Commands::Gh(args) => gh::execute(args),
            Commands::Monitor(args) => monitor::execute(args),
            Commands::Config(args) => config_cmd::execute(args),
            Commands::Cleanup(args) => cleanup::execute(args),
            Commands::Bridge(args) => bridge::execute(args),
            Commands::Daemon(args) => daemon::execute(args),
            Commands::Subscribe(args) => subscribe::execute_subscribe(args),
            Commands::Unsubscribe(args) => subscribe::execute_unsubscribe(args),
            Commands::Tail(args) => tail::execute(args),
            Commands::Launch(args) => launch::execute(args),
            Commands::Logs(args) => logs::execute(args),
            Commands::Register(args) => register::execute(args),
            Commands::Mcp(args) => mcp::execute(args),
            Commands::Init(args) => init::execute(args),
        }
    }
}
