//! ATM Daemon - Background service for agent team mail plugins

use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::logging;
use agent_team_mail_daemon::daemon;
use agent_team_mail_daemon::daemon::{
    LogWriterConfig, StatusWriter, new_dedup_store, new_launch_sender, new_log_event_queue,
    new_pubsub_store, new_session_registry, new_state_store, new_stream_event_sender,
    new_stream_state_store, run_log_writer_task,
};
use agent_team_mail_daemon::plugin::{MailService, PluginContext, PluginRegistry};
use agent_team_mail_daemon::roster::RosterService;
use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::info;

fn export_otel_from_entrypoint(
    log_path: &std::path::Path,
    event: &agent_team_mail_core::logging_event::LogEventV1,
) {
    sc_observability::export_otel_best_effort_from_path(log_path, event);
}

fn current_otel_health_from_entrypoint(
    log_path: &std::path::Path,
) -> agent_team_mail_daemon::daemon::observability::OtelHealthSnapshot {
    let health = sc_observability::current_otel_health(log_path);
    agent_team_mail_daemon::daemon::observability::OtelHealthSnapshot {
        schema_version: health.schema_version,
        enabled: health.enabled,
        collector_endpoint: health.collector_endpoint,
        protocol: health.protocol,
        collector_state: health.collector_state,
        local_mirror_state: health.local_mirror_state,
        local_mirror_path: health.local_mirror_path,
        debug_local_export: health.debug_local_export,
        debug_local_state: health.debug_local_state,
        last_error: agent_team_mail_daemon::daemon::observability::OtelLastError {
            code: health.last_error.code,
            message: health.last_error.message,
            at: health.last_error.at,
        },
    }
}

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

    // Initialize shared logging. --verbose maps to ATM_LOG=debug.
    if args.verbose {
        // SAFETY: process-local env mutation during startup before worker tasks spawn.
        unsafe { std::env::set_var("ATM_LOG", "debug") };
    }

    // Determine home directory early for lock/log path resolution.
    let home_dir =
        agent_team_mail_core::home::get_home_dir().context("Failed to determine home directory")?;
    let _ = daemon::startup_auth::sweep_stale_isolated_runtimes()
        .context("Failed to sweep stale isolated runtimes")?;
    let launch_token = daemon::startup_auth::validate_startup_token(&home_dir)
        .context("Daemon launch authorization failed")?;
    let runtime_owner =
        agent_team_mail_core::daemon_client::validate_runtime_admission_for_current_process(
            &home_dir,
        )
        .context("Shared runtime admission failed")?;

    // Enforce single-instance daemon ownership with an exclusive process lock.
    let daemon_lock_path = agent_team_mail_core::daemon_client::daemon_lock_path()?;
    std::fs::create_dir_all(
        daemon_lock_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".")),
    )
    .with_context(|| {
        format!(
            "Failed to create daemon lock directory: {}",
            daemon_lock_path.display()
        )
    })?;
    let daemon_lock = agent_team_mail_core::io::lock::acquire_lock(&daemon_lock_path, 0).map_err(
        |e| match e {
            agent_team_mail_core::io::InboxError::LockTimeout { .. } => {
                let detail = if let Some(existing) =
                    agent_team_mail_core::daemon_client::read_daemon_lock_metadata(&home_dir)
                {
                    format!(
                        "atm-daemon already running for this shared runtime (lock held at {}). Existing owner: pid={} version={} {}. Refusing second instance.",
                        daemon_lock_path.display(),
                        existing.pid,
                        existing.version,
                        agent_team_mail_core::daemon_client::format_runtime_owner_summary(&existing.owner)
                    )
                } else {
                    format!(
                        "atm-daemon already running (lock held at {}). Refusing second instance.",
                        daemon_lock_path.display()
                    )
                };
                daemon::startup_auth::log_shared_runtime_rejection(
                    &home_dir,
                    &launch_token,
                    &detail,
                );
                if let Some(existing) =
                    agent_team_mail_core::daemon_client::read_daemon_lock_metadata(&home_dir)
                {
                    anyhow::anyhow!(
                        "atm-daemon already running for this shared runtime (lock held at {}). Existing owner: pid={} version={} {}. Refusing second instance.",
                        daemon_lock_path.display(),
                        existing.pid,
                        existing.version,
                        agent_team_mail_core::daemon_client::format_runtime_owner_summary(&existing.owner)
                    )
                } else {
                    anyhow::anyhow!(
                        "atm-daemon already running (lock held at {}). Refusing second instance.",
                        daemon_lock_path.display()
                    )
                }
            }
            other => anyhow::anyhow!(
                "Failed to acquire daemon lock at {}: {}",
                daemon_lock_path.display(),
                other
            ),
        },
    )?;
    agent_team_mail_core::daemon_client::write_daemon_lock_metadata(
        &home_dir,
        env!("CARGO_PKG_VERSION"),
        &runtime_owner,
    )
    .context("Failed to write daemon lock metadata")?;
    daemon::startup_auth::persist_runtime_metadata_from_token(&home_dir, &launch_token)
        .context("Failed to persist launch lease metadata")?;
    daemon::startup_auth::log_launch_accepted(&home_dir, &launch_token);

    // Resolve canonical log writer config once and reuse for startup merge + writer task.
    let log_writer_config = LogWriterConfig::from_env(&home_dir);
    let log_file = log_writer_config.log_path.clone();

    let _log_guards = logging::init_unified(
        "atm-daemon",
        logging::UnifiedLogMode::DaemonWriter {
            file_path: log_file.clone(),
            rotation: logging::RotationConfig::default(),
        },
    )
    .unwrap_or_else(|_| logging::init_stderr_only());

    info!("ATM Daemon starting...");

    if args.daemon {
        info!("Daemon mode requested (note: fork/detach not yet implemented)");
    }

    // Merge any spool files written by producers while the daemon was offline.
    // This runs before the socket server starts so no new spool files can arrive
    // from this daemon instance during the merge window.
    {
        let spool_dir = agent_team_mail_core::logging_event::configured_spool_dir(&home_dir);
        match agent_team_mail_daemon::daemon::spool_merge::merge_spool_on_startup(
            &spool_dir, &log_file,
        ) {
            Ok(count) if count > 0 => {
                info!(count, "Merged spool events into canonical log");
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("Spool merge failed (non-fatal): {e}");
            }
        }
    }

    let current_dir = std::env::current_dir().context("Failed to get current directory")?;

    // Load configuration
    let config_overrides = agent_team_mail_core::config::ConfigOverrides {
        config_path: args.config.clone(),
        team: args.team.clone(),
        ..Default::default()
    };

    let config =
        agent_team_mail_core::config::resolve_config(&config_overrides, &current_dir, &home_dir)
            .context("Failed to resolve configuration")?;
    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm-daemon",
        action: "daemon_start",
        team: Some(config.core.default_team.clone()),
        session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
        agent_id: std::env::var("ATM_IDENTITY").ok(),
        agent_name: std::env::var("ATM_IDENTITY").ok(),
        result: Some("starting".to_string()),
        ..Default::default()
    });

    if let Some(config_path) = args.config {
        info!("Loaded config from: {}", config_path.display());
    } else {
        info!("Using resolved configuration");
    }

    // Build system context
    let claude_root = agent_team_mail_core::home::claude_root_dir_for(&home_dir);

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

    let teams_root = agent_team_mail_core::home::teams_root_dir_for(&home_dir);

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

    // Register GH Monitor plugin if configured
    if let Some(ci_config) = plugin_ctx.plugin_config("gh_monitor")
        && ci_config
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    {
        registry.register(agent_team_mail_daemon::plugins::ci_monitor::CiMonitorPlugin::new());
        info!("Registered GH Monitor plugin");
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

    // Create the session registry for tracking Claude Code agent session IDs.
    // Created here so it can be shared with the WorkerAdapterPlugin (for hook
    // event population) and with the socket server (for session-query responses).
    let session_registry = new_session_registry();

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
        worker_plugin.set_session_registry(Arc::clone(&session_registry));
        registry.register(worker_plugin);
        info!("Registered Worker Adapter plugin with launch channel and session registry");
    }

    info!("Registered {} plugin(s)", registry.len());

    // Create the durable dedupe store for restart-safe request idempotency.
    let dedup_store = new_dedup_store(&home_dir).with_context(|| {
        let path = agent_team_mail_core::daemon_client::daemon_dedup_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unresolved daemon dedup path>".to_string());
        format!("Failed to initialise durable dedupe store at {path}")
    })?;

    // Create status writer
    let status_writer = Arc::new(StatusWriter::new(
        home_dir.clone(),
        env!("CARGO_PKG_VERSION").to_string(),
        runtime_owner.clone(),
    ));
    info!(
        "Status writer initialized: {}",
        status_writer.status_path().display()
    );

    // Create cancellation token for graceful shutdown
    let cancel_token = CancellationToken::new();
    let lease_violation = daemon::startup_auth::new_shared_lease_violation();
    let lease_monitor_task = daemon::startup_auth::spawn_isolated_test_lease_monitor(
        home_dir.clone(),
        launch_token.clone(),
        cancel_token.clone(),
        lease_violation.clone(),
    );

    // Set up signal handlers
    let cancel_for_signals = cancel_token.clone();
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();

        #[cfg(unix)]
        {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
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

    // Create the per-agent stream state store for normalised turn events.
    let stream_state_store = new_stream_state_store();

    // Create the broadcast sender for push-based stream event fanout.
    let stream_event_sender = new_stream_event_sender();

    // Create the bounded log event queue and async writer task.
    let log_event_queue = new_log_event_queue();
    let log_cancel = cancel_token.clone();
    daemon::observability::install_otel_export_hook(Arc::new(export_otel_from_entrypoint));
    daemon::observability::install_otel_health_hook(Arc::new(current_otel_health_from_entrypoint));
    tokio::spawn(run_log_writer_task(
        log_event_queue.clone(),
        log_writer_config,
        log_cancel,
    ));

    // Run the daemon event loop
    let run_result = daemon::run(
        &mut registry,
        &plugin_ctx,
        daemon_lock,
        cancel_token,
        status_writer,
        state_store,
        pubsub_store,
        launch_tx,
        session_registry,
        dedup_store,
        stream_state_store,
        stream_event_sender,
        log_event_queue,
    )
    .await;
    if let Some(task) = lease_monitor_task {
        let _ = task.await;
    }
    let lease_violation = lease_violation.lock().unwrap().clone();
    match (&run_result, &lease_violation) {
        (Ok(_), None) => {
            daemon::startup_auth::log_clean_owner_shutdown(&home_dir, &launch_token);
            emit_event_best_effort(EventFields {
                level: "info",
                source: "atm-daemon",
                action: "daemon_stop",
                team: Some(plugin_ctx.config.core.default_team.clone()),
                session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
                agent_id: std::env::var("ATM_IDENTITY").ok(),
                agent_name: std::env::var("ATM_IDENTITY").ok(),
                result: Some("ok".to_string()),
                ..Default::default()
            })
        }
        (Ok(_), Some(_)) => emit_event_best_effort(EventFields {
            level: "info",
            source: "atm-daemon",
            action: "daemon_stop",
            team: Some(plugin_ctx.config.core.default_team.clone()),
            session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
            agent_id: std::env::var("ATM_IDENTITY").ok(),
            agent_name: std::env::var("ATM_IDENTITY").ok(),
            result: Some("ok".to_string()),
            ..Default::default()
        }),
        (Err(e), _) => emit_event_best_effort(EventFields {
            level: "error",
            source: "atm-daemon",
            action: "daemon_stop",
            team: Some(plugin_ctx.config.core.default_team.clone()),
            session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
            agent_id: std::env::var("ATM_IDENTITY").ok(),
            agent_name: std::env::var("ATM_IDENTITY").ok(),
            result: Some("error".to_string()),
            error: Some(e.to_string()),
            ..Default::default()
        }),
    }
    if let Some(violation) = lease_violation {
        anyhow::bail!(
            "daemon lease violation: {} ({})",
            violation.event_name,
            violation.detail
        );
    }
    run_result.context("Daemon event loop failed")?;

    info!("ATM Daemon shutdown complete");
    Ok(())
}
