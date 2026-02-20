//! ATM Daemon - Background service for agent team mail plugins

use anyhow::{Context, Result};
use agent_team_mail_daemon::daemon;
use agent_team_mail_daemon::daemon::{new_launch_sender, new_pubsub_store, new_state_store, StatusWriter};
use agent_team_mail_daemon::plugin::{MailService, PluginContext, PluginRegistry};
use agent_team_mail_daemon::roster::RosterService;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// ATM Daemon - Background service for agent team mail plugins
#[derive(Parser, Debug)]
#[command(name = "atm-daemon")]
#[command(about = "Background service for agent team mail plugins")]
#[command(version)]
struct Args {
    /// Path to configuration file
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Team name to monitor (default: all teams)
    #[arg(long, value_name = "NAME")]
    team: Option<String>,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Run in background/daemon mode (future: fork/detach)
    #[arg(short, long)]
    daemon: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let log_level = if args.verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };

    tracing_subscriber::fmt()
        .with_max_level(log_level)
        .with_target(false)
        .init();

    info!("ATM Daemon starting...");

    if args.daemon {
        info!("Daemon mode requested (note: fork/detach not yet implemented)");
    }

    // Determine home and current directories for config resolution
    let home_dir = agent_team_mail_core::home::get_home_dir()
        .context("Failed to determine home directory")?;

    let current_dir = std::env::current_dir()
        .context("Failed to get current directory")?;

    // Load configuration
    let config_overrides = agent_team_mail_core::config::ConfigOverrides {
        config_path: args.config.clone(),
        team: args.team.clone(),
        ..Default::default()
    };

    let config = agent_team_mail_core::config::resolve_config(&config_overrides, &current_dir, &home_dir)
        .context("Failed to resolve configuration")?;

    if let Some(config_path) = args.config {
        info!("Loaded config from: {}", config_path.display());
    } else {
        info!("Using resolved configuration");
    }

    // Build system context
    let claude_root = home_dir.join(".claude");

    let system_ctx = agent_team_mail_core::context::SystemContext::new(
        hostname::get()
            .map_err(|e| anyhow::anyhow!("Failed to get hostname: {e}"))?
            .to_string_lossy()
            .to_string(),
        agent_team_mail_core::context::Platform::detect(),
        claude_root.clone(),
        env!("CARGO_PKG_VERSION").to_string(),
        config.core.default_team.clone(),
    );

    let teams_root = claude_root.join("teams");

    info!("Teams root: {}", teams_root.display());

    // Create mail service and roster service
    let mail_service = MailService::new(teams_root.clone());
    let roster_service = RosterService::new(teams_root.clone());

    // Build plugin context
    let plugin_ctx = PluginContext::new(
        Arc::new(system_ctx),
        Arc::new(mail_service),
        Arc::new(config),
        Arc::new(roster_service),
    );

    // Create plugin registry
    let mut registry = PluginRegistry::new();

    // Register CI Monitor plugin if configured
    if let Some(ci_config) = plugin_ctx.plugin_config("ci_monitor")
        && ci_config
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    {
        registry.register(agent_team_mail_daemon::plugins::ci_monitor::CiMonitorPlugin::new());
        info!("Registered CI Monitor plugin");
    }

    // Register Issues plugin if configured
    if let Some(issues_config) = plugin_ctx.plugin_config("issues")
        && issues_config
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    {
        registry.register(agent_team_mail_daemon::plugins::issues::IssuesPlugin::new());
        info!("Registered Issues plugin");
    }

    // Create the shared agent state store.  When the worker adapter plugin is
    // enabled we hand the same Arc to both the plugin and the event loop so
    // that the socket server reads live state.  When the plugin is absent the
    // store stays empty; the socket server still starts but returns
    // AGENT_NOT_FOUND for all agent-state queries.
    let state_store = new_state_store();

    // Create the shared pub/sub store.  When the worker adapter plugin is
    // enabled, the plugin's internal pub/sub Arc is captured before registration
    // and used here so CLI subscribe requests and notification delivery share
    // the same registry.  When the plugin is absent an empty store is used.
    let mut pubsub_store = new_pubsub_store();

    // Create the launch channel.  When the worker adapter plugin is enabled we
    // wire the receiver into the plugin and store the sender in the shared
    // LaunchSender so the socket server can forward launch requests.  When the
    // plugin is absent, the sender stays None and the socket server returns
    // LAUNCH_UNAVAILABLE for any "launch" commands.
    let launch_tx = new_launch_sender();

    // Register Worker Adapter plugin if configured
    if let Some(workers_config) = plugin_ctx.plugin_config("workers")
        && workers_config
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        let (tx, rx) = tokio::sync::mpsc::channel(16);

        // Store the sender in the shared LaunchSender so the socket server
        // can forward launch requests.
        {
            let mut guard = launch_tx.lock().await;
            *guard = Some(tx);
        }

        // Build the plugin with the shared state store, then capture its
        // internal pub/sub Arc before registering (registration moves the plugin).
        let mut worker_plugin =
            agent_team_mail_daemon::plugins::worker_adapter::WorkerAdapterPlugin::with_state_store(
                Arc::clone(&state_store),
            );
        pubsub_store = worker_plugin.pubsub_store();
        worker_plugin.set_launch_receiver(rx);
        registry.register(worker_plugin);
        info!("Registered Worker Adapter plugin with launch channel");
    }

    info!("Registered {} plugin(s)", registry.len());

    // Create status writer
    let status_writer = Arc::new(StatusWriter::new(
        home_dir.clone(),
        env!("CARGO_PKG_VERSION").to_string(),
    ));
    info!("Status writer initialized: {}", status_writer.status_path().display());

    // Create cancellation token for graceful shutdown
    let cancel_token = CancellationToken::new();

    // Set up signal handlers
    let cancel_for_signals = cancel_token.clone();
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();

        #[cfg(unix)]
        {
            let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Failed to create SIGTERM handler");

            tokio::select! {
                _ = ctrl_c => {
                    info!("Received SIGINT (Ctrl+C)");
                }
                _ = sigterm.recv() => {
                    info!("Received SIGTERM");
                }
            }
        }

        #[cfg(not(unix))]
        {
            ctrl_c.await.expect("Failed to listen for Ctrl+C");
            info!("Received Ctrl+C");
        }

        cancel_for_signals.cancel();
    });

    // Run the daemon event loop
    daemon::run(
        &mut registry,
        &plugin_ctx,
        cancel_token,
        status_writer,
        state_store,
        pubsub_store,
        launch_tx,
    )
    .await
    .context("Daemon event loop failed")?;

    info!("ATM Daemon shutdown complete");
    Ok(())
}
