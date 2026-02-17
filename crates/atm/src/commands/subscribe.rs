//! Subscribe / unsubscribe CLI commands for agent state change notifications.
//!
//! Both commands communicate with the ATM daemon via Unix socket. If the daemon
//! is not running, a clear error message is printed and the command exits with
//! a non-zero status.
//!
//! ## Usage
//!
//! ```text
//! # Subscribe to idle events for arch-ctm
//! atm subscribe arch-ctm idle
//!
//! # Subscribe to both idle and killed events
//! atm subscribe arch-ctm idle killed
//!
//! # Unsubscribe
//! atm unsubscribe arch-ctm
//!
//! # JSON output
//! atm subscribe arch-ctm idle --json
//! ```

use anyhow::Result;
use clap::Args;
use agent_team_mail_core::config::{resolve_config, ConfigOverrides};
use crate::util::settings::get_home_dir;

// ── SubscribeArgs ─────────────────────────────────────────────────────────────

/// Subscribe to agent state change notifications.
///
/// When the watched agent transitions to a matching state, the ATM daemon
/// delivers an inbox message to the subscriber's inbox.
///
/// Subscriptions are ephemeral: they are cleared when the daemon restarts.
/// Re-subscribing (with the same agent) refreshes the TTL without creating
/// a duplicate.
#[derive(Args, Debug)]
pub struct SubscribeArgs {
    /// Agent to subscribe to (e.g., "arch-ctm")
    agent: String,

    /// State events to subscribe to (default: "idle").
    ///
    /// Supported values: `idle`, `busy`, `killed`, `launching`.
    /// Pass multiple values to subscribe to more than one event.
    #[arg(default_value = "idle")]
    events: Vec<String>,

    /// Override default team
    #[arg(long)]
    team: Option<String>,

    /// Output result as JSON
    #[arg(long)]
    json: bool,
}

// ── UnsubscribeArgs ───────────────────────────────────────────────────────────

/// Unsubscribe from agent state change notifications.
#[derive(Args, Debug)]
pub struct UnsubscribeArgs {
    /// Agent to unsubscribe from (e.g., "arch-ctm")
    agent: String,

    /// Override default team
    #[arg(long)]
    team: Option<String>,

    /// Output result as JSON
    #[arg(long)]
    json: bool,
}

// ── execute_subscribe ─────────────────────────────────────────────────────────

/// Execute the `atm subscribe` command.
pub fn execute_subscribe(args: SubscribeArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };
    let config = resolve_config(&overrides, &current_dir, &home_dir)?;

    let subscriber = &config.core.identity;
    let agent = &args.agent;
    let team = args.team.as_deref().unwrap_or(&config.core.default_team);

    match agent_team_mail_core::daemon_client::subscribe_to_agent(
        subscriber,
        agent,
        team,
        &args.events,
    )? {
        None => {
            // Daemon not running
            if args.json {
                let output = serde_json::json!({
                    "error": "daemon_not_running",
                    "message": "Daemon not running. Subscriptions require the ATM daemon."
                });
                eprintln!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                eprintln!("Daemon not running. Subscriptions require the ATM daemon.");
                eprintln!("Start the daemon with: atm-daemon");
            }
            std::process::exit(1);
        }
        Some(resp) if resp.is_ok() => {
            if args.json {
                let output = serde_json::json!({
                    "subscribed": true,
                    "subscriber": subscriber,
                    "agent": agent,
                    "events": args.events,
                    "team": team,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!(
                    "Subscribed: {} will notify {} when {} transitions to {:?}",
                    agent, subscriber, agent, args.events
                );
            }
        }
        Some(resp) => {
            // Daemon returned an error response
            let code = resp.error.as_ref().map(|e| e.code.as_str()).unwrap_or("UNKNOWN");
            let message = resp.error.as_ref().map(|e| e.message.as_str()).unwrap_or("Unknown error");
            if args.json {
                let output = serde_json::json!({
                    "error": code,
                    "message": message,
                });
                eprintln!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                eprintln!("Subscribe failed ({code}): {message}");
            }
            std::process::exit(1);
        }
    }

    Ok(())
}

// ── execute_unsubscribe ───────────────────────────────────────────────────────

/// Execute the `atm unsubscribe` command.
pub fn execute_unsubscribe(args: UnsubscribeArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };
    let config = resolve_config(&overrides, &current_dir, &home_dir)?;

    let subscriber = &config.core.identity;
    let agent = &args.agent;
    let team = args.team.as_deref().unwrap_or(&config.core.default_team);

    match agent_team_mail_core::daemon_client::unsubscribe_from_agent(
        subscriber,
        agent,
        team,
    )? {
        None => {
            // Daemon not running
            if args.json {
                let output = serde_json::json!({
                    "error": "daemon_not_running",
                    "message": "Daemon not running. Subscriptions require the ATM daemon."
                });
                eprintln!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                eprintln!("Daemon not running. Subscriptions require the ATM daemon.");
                eprintln!("Start the daemon with: atm-daemon");
            }
            std::process::exit(1);
        }
        Some(resp) if resp.is_ok() => {
            if args.json {
                let output = serde_json::json!({
                    "unsubscribed": true,
                    "subscriber": subscriber,
                    "agent": agent,
                    "team": team,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("Unsubscribed: {} will no longer receive notifications for {}", subscriber, agent);
            }
        }
        Some(resp) => {
            let code = resp.error.as_ref().map(|e| e.code.as_str()).unwrap_or("UNKNOWN");
            let message = resp.error.as_ref().map(|e| e.message.as_str()).unwrap_or("Unknown error");
            if args.json {
                let output = serde_json::json!({
                    "error": code,
                    "message": message,
                });
                eprintln!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                eprintln!("Unsubscribe failed ({code}): {message}");
            }
            std::process::exit(1);
        }
    }

    Ok(())
}
