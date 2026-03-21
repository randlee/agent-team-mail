//! Daemon management commands

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::consts::ISOLATED_RUNTIME_DEFAULT_TTL_SECS;
use agent_team_mail_core::daemon_client::RuntimeOwnerMetadata;
use agent_team_mail_core::io::inbox::inbox_append;
use agent_team_mail_core::schema::InboxMessage;
use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Args, Subcommand};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};
use sysinfo::{Pid, ProcessStatus, System};
use uuid::Uuid;

use crate::commands::logging_health::{
    LoggingHealthSnapshot, OtelHealthSnapshot, build_logging_health_contract,
    build_otel_health_contract,
};
use crate::util::settings::get_home_dir;
use agent_team_mail_core::daemon_client::{
    create_isolated_runtime_root, daemon_lock_metadata_path_for, daemon_runtime_metadata_path_for,
    daemon_status_path_for, read_daemon_lock_metadata, reap_expired_isolated_runtime_roots,
};
use std::path::{Path, PathBuf};

const DEFAULT_DAEMON_STOP_TIMEOUT_SECS: u64 = 60;

/// Daemon management commands
#[derive(Args, Debug)]
pub struct DaemonArgs {
    /// Kill the named agent process via daemon session tracking
    #[arg(long, value_name = "AGENT")]
    kill: Option<String>,

    /// Team override for --kill
    #[arg(long, requires = "kill")]
    team: Option<String>,

    /// Wait timeout in seconds for graceful shutdown before forced termination
    #[arg(long, default_value_t = 10, requires = "kill")]
    timeout: u64,

    #[command(subcommand)]
    command: Option<DaemonCommands>,
}

#[derive(Subcommand, Debug)]
enum DaemonCommands {
    /// Show daemon status
    Status(StatusArgs),
    /// Stop the running daemon gracefully
    Stop(StopArgs),
    /// Restart the daemon (stop then autostart)
    Restart(RestartArgs),
    /// Create an explicit isolated ATM runtime root for smoke/debug/test work
    Isolated(IsolatedArgs),
}

/// Stop the running daemon
#[derive(Args, Debug)]
pub struct StopArgs {
    /// Wait timeout in seconds for graceful shutdown (default 60)
    #[arg(long, default_value_t = DEFAULT_DAEMON_STOP_TIMEOUT_SECS)]
    timeout: u64,
}

/// Restart the daemon (stop then autostart)
#[derive(Args, Debug)]
pub struct RestartArgs {
    /// Wait timeout in seconds for graceful shutdown before restart (default 60)
    #[arg(long, default_value_t = DEFAULT_DAEMON_STOP_TIMEOUT_SECS)]
    timeout: u64,
}

/// Create an isolated ATM runtime root.
#[derive(Args, Debug)]
pub struct IsolatedArgs {
    /// Optional label to include in the isolated runtime directory name
    #[arg(long)]
    name: Option<String>,

    /// TTL in minutes before the isolated runtime becomes cleanup-eligible
    #[arg(long, default_value_t = ISOLATED_RUNTIME_DEFAULT_TTL_SECS / 60)]
    ttl_minutes: u64,

    /// Allow live GitHub polling in this isolated runtime
    #[arg(long)]
    allow_live_github: bool,

    /// Output as JSON
    #[arg(long)]
    json: bool,
}

/// Show daemon status
#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Output as JSON
    #[arg(long)]
    json: bool,
}

/// Execute daemon command
pub fn execute(args: DaemonArgs) -> Result<()> {
    if let Some(agent) = args.kill.as_deref() {
        return execute_kill(agent, args.team.as_deref(), args.timeout.max(1));
    }

    match args
        .command
        .unwrap_or(DaemonCommands::Status(StatusArgs { json: false }))
    {
        DaemonCommands::Status(status_args) => execute_status(status_args),
        DaemonCommands::Stop(stop_args) => execute_stop(stop_args.timeout.max(1)),
        DaemonCommands::Restart(restart_args) => execute_restart(restart_args.timeout.max(1)),
        DaemonCommands::Isolated(isolated_args) => execute_isolated(isolated_args),
    }
}

fn execute_isolated(args: IsolatedArgs) -> Result<()> {
    let reaped = reap_expired_isolated_runtime_roots()?;
    let created = create_isolated_runtime_root(
        args.name.as_deref(),
        Duration::from_secs(args.ttl_minutes.max(1) * 60),
        args.allow_live_github,
    )?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "atm_home": created.home,
                "runtime_dir": created.runtime_dir,
                "socket_path": created.socket_path,
                "lock_path": created.lock_path,
                "status_path": created.status_path,
                "runtime_kind": created.metadata.runtime_kind.as_str(),
                "created_at": created.metadata.created_at,
                "expires_at": created.metadata.expires_at,
                "allow_live_github_polling": created.metadata.allow_live_github_polling,
                "reaped_count": reaped.len(),
            }))?
        );
        return Ok(());
    }

    println!("Created isolated ATM runtime:");
    println!("ATM_HOME:           {}", created.home.display());
    println!("Runtime Dir:        {}", created.runtime_dir.display());
    println!("Socket:             {}", created.socket_path.display());
    println!("Lock:               {}", created.lock_path.display());
    println!("Status:             {}", created.status_path.display());
    println!("Created At:         {}", created.metadata.created_at);
    println!(
        "Expires At:         {}",
        created.metadata.expires_at.as_deref().unwrap_or("none")
    );
    println!(
        "Live GH Polling:    {}",
        if created.metadata.allow_live_github_polling {
            "enabled"
        } else {
            "disabled"
        }
    );
    if !reaped.is_empty() {
        println!("Reaped Expired:     {}", reaped.len());
    }
    Ok(())
}

fn execute_kill(agent: &str, team_override: Option<&str>, timeout_secs: u64) -> Result<()> {
    if !agent_team_mail_core::daemon_client::daemon_is_running() {
        ensure_daemon_running().context("failed to auto-start daemon for --kill")?;
    }
    if !agent_team_mail_core::daemon_client::daemon_is_running() {
        anyhow::bail!("daemon is not running");
    }

    let _home_dir = get_home_dir()?;
    let config_home = agent_team_mail_core::home::get_os_home_dir()?;
    let current_dir = std::env::current_dir()?;
    let config = resolve_config(
        &ConfigOverrides {
            team: team_override.map(ToString::to_string),
            ..Default::default()
        },
        &current_dir,
        &config_home,
    )?;
    let team_name = team_override.unwrap_or(&config.core.default_team);

    let Some(info) = agent_team_mail_core::daemon_client::query_session_for_team(team_name, agent)?
    else {
        anyhow::bail!("no daemon session record found for {agent}@{team_name}");
    };

    if !info.alive {
        crate::commands::teams::cleanup_single_agent(
            team_name.to_string(),
            agent.to_string(),
            true,
        )?;
        println!("Agent {agent}@{team_name} already not alive; teardown cleanup completed");
        return Ok(());
    }

    let pid = validated_signal_pid(info.process_id).ok_or_else(|| {
        anyhow::anyhow!(
            "refusing to signal invalid pid {} for {}@{}",
            info.process_id,
            agent,
            team_name
        )
    })?;

    send_shutdown_request(&config_home, team_name, agent)?;
    if wait_for_session_dead(team_name, agent, timeout_secs) {
        crate::commands::teams::cleanup_single_agent(
            team_name.to_string(),
            agent.to_string(),
            true,
        )?;
        println!("Graceful shutdown + teardown cleanup complete for {agent}@{team_name}");
        return Ok(());
    }

    #[cfg(unix)]
    {
        // SAFETY: SIGINT requests graceful interrupt of the target process.
        let _ = unsafe { libc::kill(pid, libc::SIGINT) };
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("forced termination not supported on this platform");
    }

    if wait_for_session_dead(team_name, agent, 10) {
        crate::commands::teams::cleanup_single_agent(
            team_name.to_string(),
            agent.to_string(),
            true,
        )?;
        println!("SIGINT termination + teardown cleanup complete for {agent}@{team_name}");
        Ok(())
    } else {
        #[cfg(unix)]
        {
            // SAFETY: SIGTERM requests cooperative shutdown after SIGINT timeout.
            let _ = unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        if wait_for_session_dead(team_name, agent, 10) {
            crate::commands::teams::cleanup_single_agent(
                team_name.to_string(),
                agent.to_string(),
                true,
            )?;
            println!("SIGTERM termination + teardown cleanup complete for {agent}@{team_name}");
            Ok(())
        } else {
            #[cfg(unix)]
            {
                // SAFETY: SIGKILL force-terminates process that ignored prior shutdown attempts.
                let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            if wait_for_session_dead(team_name, agent, 3) {
                crate::commands::teams::cleanup_single_agent(
                    team_name.to_string(),
                    agent.to_string(),
                    true,
                )?;
                println!("SIGKILL termination + teardown cleanup complete for {agent}@{team_name}");
                Ok(())
            } else {
                anyhow::bail!("failed to terminate {agent}@{team_name} within timeout")
            }
        }
    }
}

fn validated_signal_pid(pid: u32) -> Option<i32> {
    if pid > 1 && pid <= i32::MAX as u32 {
        Some(pid as i32)
    } else {
        None
    }
}

fn ensure_daemon_running() -> Result<()> {
    // Trigger daemon autostart through the daemon query path used by published
    // atm-core APIs, then verify liveness.
    let _ = agent_team_mail_core::daemon_client::query_list_agents();
    if agent_team_mail_core::daemon_client::daemon_is_running() {
        Ok(())
    } else {
        anyhow::bail!("daemon is not running")
    }
}

fn send_shutdown_request(
    home_dir: &std::path::Path,
    team_name: &str,
    agent_name: &str,
) -> Result<()> {
    let payload = serde_json::json!({
        "type": "shutdown_request",
        "requestId": Uuid::new_v4().to_string(),
        "from": "atm",
        "reason": "daemon --kill",
        "timestamp": Utc::now().to_rfc3339(),
    });
    let msg = InboxMessage {
        from: "atm".to_string(),
        source_team: None,
        text: payload.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        read: false,
        summary: Some("shutdown_request".to_string()),
        message_id: Some(Uuid::new_v4().to_string()),
        unknown_fields: HashMap::new(),
    };
    let inbox_path = agent_team_mail_core::home::config_team_dir_for(home_dir, team_name)
        .join("inboxes")
        .join(format!("{agent_name}.json"));
    inbox_append(&inbox_path, &msg, team_name, "atm")?;
    Ok(())
}

/// Stop the running daemon by sending SIGTERM to the PID reported in daemon metadata.
///
/// On non-Unix platforms this is a no-op (returns `Ok(())`).
fn execute_stop(timeout_secs: u64) -> Result<()> {
    #[cfg(unix)]
    {
        let runtime = daemon_runtime_paths()?;

        if !agent_team_mail_core::daemon_client::daemon_is_running() {
            println!(
                "Daemon is not running (no live daemon socket at {}).",
                runtime.socket_path.display()
            );
            return Ok(());
        }

        match stop_daemon_with(
            &runtime,
            timeout_secs,
            signal_pid,
            is_pid_alive,
            Duration::from_millis(200),
        )? {
            StopOutcome::Stopped { pid, already_dead } => {
                if already_dead {
                    println!("Daemon process {pid} no longer exists; cleaning up runtime files.");
                } else {
                    println!(
                        "Sent SIGTERM to daemon (PID {pid}); waiting up to {timeout_secs}s for exit..."
                    );
                    println!("Daemon (PID {pid}) has stopped.");
                }
                Ok(())
            }
            StopOutcome::TimedOut { pid } => {
                println!(
                    "Sent SIGTERM to daemon (PID {pid}); waiting up to {timeout_secs}s for exit..."
                );
                println!("Daemon (PID {pid}) did not stop within {timeout_secs}s after SIGTERM.");
                println!("You may force-kill it with: kill -9 {pid}");
                std::process::exit(1);
            }
        }
    }

    #[cfg(not(unix))]
    {
        eprintln!("atm daemon stop is not supported on this platform.");
        std::process::exit(1);
    }
}

/// Restart the daemon: stop the running instance then trigger autostart.
fn execute_restart(timeout_secs: u64) -> Result<()> {
    #[cfg(unix)]
    {
        let runtime = daemon_runtime_paths()?;
        if let Some(expected_bin) = expected_daemon_binary_path() {
            stop_matching_daemon_processes(
                &expected_bin,
                &runtime,
                signal_pid,
                is_pid_alive,
                RestartTiming::DEFAULT,
            )?;
        } else {
            eprintln!(
                "warning: unable to resolve expected daemon binary path; skipping pre-restart daemon cleanup"
            );
        }
        restart_daemon_with(
            &runtime,
            timeout_secs,
            signal_pid,
            is_pid_alive,
            agent_team_mail_core::daemon_client::ensure_daemon_running,
            agent_team_mail_core::daemon_client::daemon_is_running,
            RestartTiming::DEFAULT,
        )?;
    }
    Ok(())
}

#[cfg(unix)]
#[derive(Debug, Clone)]
struct DaemonRuntimePaths {
    home_dir: PathBuf,
    socket_path: PathBuf,
    status_path: PathBuf,
    metadata_path: PathBuf,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopOutcome {
    Stopped { pid: i32, already_dead: bool },
    TimedOut { pid: i32 },
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
struct RestartTiming {
    stop_poll_interval: Duration,
    runtime_absent_timeout: Duration,
    runtime_absent_poll_interval: Duration,
}

#[cfg(unix)]
impl RestartTiming {
    const DEFAULT: Self = Self {
        stop_poll_interval: Duration::from_millis(200),
        runtime_absent_timeout: Duration::from_secs(2),
        runtime_absent_poll_interval: Duration::from_millis(50),
    };
}

#[cfg(unix)]
fn daemon_runtime_paths() -> Result<DaemonRuntimePaths> {
    let home_dir = get_home_dir()?;
    Ok(DaemonRuntimePaths {
        home_dir: home_dir.clone(),
        socket_path: agent_team_mail_core::daemon_client::daemon_socket_path()
            .context("failed to resolve daemon socket path")?,
        status_path: agent_team_mail_core::daemon_client::daemon_status_path()
            .context("failed to resolve daemon status path")?,
        metadata_path: agent_team_mail_core::daemon_client::daemon_lock_metadata_path()
            .context("failed to resolve daemon lock metadata path")?,
    })
}

#[cfg(unix)]
fn read_daemon_pid(paths: &DaemonRuntimePaths) -> Result<i32> {
    let pid: i32 = daemon_pid_from_runtime_artifacts(&paths.home_dir)
        .map(|pid| pid as i32)
        .ok_or_else(|| {
            anyhow::anyhow!("failed to determine daemon pid from status/lock metadata")
        })?;
    if pid <= 1 {
        anyhow::bail!(
            "daemon metadata contains invalid PID {}; refusing to send signal",
            pid
        );
    }
    Ok(pid)
}

#[cfg(unix)]
fn signal_pid(pid: i32, signal: i32) -> std::io::Result<()> {
    // SAFETY: signal delivery is delegated to libc; caller controls pid/signal.
    let rc = unsafe { libc::kill(pid, signal) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn is_pid_alive(pid: i32) -> bool {
    if pid <= 1 {
        return false;
    }

    // First ask the kernel if the PID still exists at all.
    // SAFETY: kill(pid, 0) checks liveness without delivering a signal.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc != 0 {
        return false;
    }

    let system = System::new_all();
    system
        .process(Pid::from_u32(pid as u32))
        .map(|process| process.status() != ProcessStatus::Zombie)
        .unwrap_or(true)
}

#[cfg(unix)]
fn expected_daemon_binary_path() -> Option<PathBuf> {
    std::env::var_os("ATM_DAEMON_BIN")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("atm-daemon")))
        })
}

#[cfg(unix)]
fn process_executable_matches_parts(
    exe: Option<&Path>,
    cmd0: Option<&std::ffi::OsString>,
    expected: &Path,
) -> bool {
    exe.is_some_and(|exe| exe == expected)
        || cmd0.map(|arg| Path::new(arg) == expected).unwrap_or(false)
}

#[cfg(unix)]
fn process_executable_matches(process: &sysinfo::Process, expected: &Path) -> bool {
    process_executable_matches_parts(process.exe(), process.cmd().first(), expected)
}

#[cfg(unix)]
fn matching_daemon_process_ids(expected: &Path) -> Vec<i32> {
    let system = System::new_all();
    let mut pids = system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            process_executable_matches(process, expected).then_some(pid.as_u32() as i32)
        })
        .filter(|pid| *pid > 1)
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    pids
}

#[cfg(unix)]
fn stop_matching_daemon_processes<FSignal, FAlive>(
    expected: &Path,
    runtime: &DaemonRuntimePaths,
    send_signal: FSignal,
    is_alive: FAlive,
    timing: RestartTiming,
) -> Result<Vec<i32>>
where
    FSignal: Fn(i32, i32) -> std::io::Result<()>,
    FAlive: Fn(i32) -> bool,
{
    let pids = matching_daemon_process_ids(expected);
    stop_matching_daemon_processes_for_pids(pids, runtime, send_signal, is_alive, timing)
}

#[cfg(unix)]
fn stop_matching_daemon_processes_for_pids<FSignal, FAlive>(
    pids: Vec<i32>,
    runtime: &DaemonRuntimePaths,
    send_signal: FSignal,
    is_alive: FAlive,
    timing: RestartTiming,
) -> Result<Vec<i32>>
where
    FSignal: Fn(i32, i32) -> std::io::Result<()>,
    FAlive: Fn(i32) -> bool,
{
    if pids.is_empty() {
        return Ok(Vec::new());
    }

    for pid in &pids {
        if let Err(err) = send_signal(*pid, libc::SIGTERM)
            && err.raw_os_error() != Some(libc::ESRCH)
        {
            return Err(anyhow::anyhow!(
                "failed to send SIGTERM to daemon process {pid}: {err}"
            ));
        }
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if pids.iter().all(|pid| !is_alive(*pid)) {
            cleanup_runtime_files(runtime);
            return Ok(pids);
        }
        std::thread::sleep(timing.stop_poll_interval);
    }

    for pid in &pids {
        if is_alive(*pid) {
            send_signal(*pid, libc::SIGKILL)
                .with_context(|| format!("failed to send SIGKILL to daemon process {pid}"))?;
        }
    }

    let kill_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < kill_deadline {
        if pids.iter().all(|pid| !is_alive(*pid)) {
            cleanup_runtime_files(runtime);
            return Ok(pids);
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let survivors = pids
        .into_iter()
        .filter(|pid| is_alive(*pid))
        .collect::<Vec<_>>();
    if survivors.is_empty() {
        cleanup_runtime_files(runtime);
        Ok(Vec::new())
    } else {
        anyhow::bail!(
            "daemon process(es) remained alive after cleanup: {}",
            survivors
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

#[cfg(unix)]
fn cleanup_runtime_files(paths: &DaemonRuntimePaths) {
    let _ = std::fs::remove_file(&paths.socket_path);
    let _ = std::fs::remove_file(&paths.status_path);
    let _ = std::fs::remove_file(&paths.metadata_path);
    let _ = std::fs::remove_file(daemon_runtime_metadata_path_for(&paths.home_dir));
    let _ = std::fs::remove_file(daemon_lock_metadata_path_for(&paths.home_dir));
}

#[cfg(unix)]
fn daemon_pid_from_runtime_artifacts(home: &Path) -> Option<u32> {
    read_daemon_lock_metadata(home)
        .map(|metadata| metadata.pid)
        .or_else(|| {
            let status_path = daemon_status_path_for(home);
            std::fs::read_to_string(status_path)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|json| json.get("pid").and_then(serde_json::Value::as_u64))
                .map(|pid| pid as u32)
        })
}

#[cfg(unix)]
fn wait_for_runtime_files_absent(
    paths: &DaemonRuntimePaths,
    timeout: Duration,
    poll_interval: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !paths.socket_path.exists()
            && !paths.status_path.exists()
            && !paths.metadata_path.exists()
        {
            return true;
        }
        std::thread::sleep(poll_interval);
    }
    !paths.socket_path.exists() && !paths.status_path.exists() && !paths.metadata_path.exists()
}

#[cfg(unix)]
fn stop_daemon_with<FSignal, FAlive>(
    paths: &DaemonRuntimePaths,
    timeout_secs: u64,
    send_signal: FSignal,
    is_alive: FAlive,
    poll_interval: Duration,
) -> Result<StopOutcome>
where
    FSignal: Fn(i32, i32) -> std::io::Result<()>,
    FAlive: Fn(i32) -> bool,
{
    let pid = read_daemon_pid(paths)?;
    let post_timeout_grace = Duration::from_secs(timeout_secs.clamp(1, 5));

    if let Err(err) = send_signal(pid, libc::SIGTERM) {
        if err.raw_os_error() == Some(libc::ESRCH) {
            cleanup_runtime_files(paths);
            return Ok(StopOutcome::Stopped {
                pid,
                already_dead: true,
            });
        }
        return Err(anyhow::anyhow!(
            "failed to send SIGTERM to daemon process {pid}: {err}"
        ));
    }

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if !is_alive(pid) {
            cleanup_runtime_files(paths);
            return Ok(StopOutcome::Stopped {
                pid,
                already_dead: false,
            });
        }
        std::thread::sleep(poll_interval);
    }

    let grace_deadline = Instant::now() + post_timeout_grace;
    while Instant::now() < grace_deadline {
        if !is_alive(pid) {
            cleanup_runtime_files(paths);
            return Ok(StopOutcome::Stopped {
                pid,
                already_dead: false,
            });
        }
        std::thread::sleep(poll_interval.min(Duration::from_millis(100)));
    }

    if !is_alive(pid) {
        cleanup_runtime_files(paths);
        return Ok(StopOutcome::Stopped {
            pid,
            already_dead: false,
        });
    }

    Ok(StopOutcome::TimedOut { pid })
}

#[cfg(unix)]
fn restart_daemon_with<FSignal, FAlive, FEnsure, FDaemonRunning>(
    runtime: &DaemonRuntimePaths,
    timeout_secs: u64,
    send_signal: FSignal,
    is_alive: FAlive,
    ensure_running: FEnsure,
    daemon_running: FDaemonRunning,
    timing: RestartTiming,
) -> Result<()>
where
    FSignal: Fn(i32, i32) -> std::io::Result<()>,
    FAlive: Fn(i32) -> bool,
    FEnsure: Fn() -> Result<()>,
    FDaemonRunning: Fn() -> bool,
{
    if daemon_pid_from_runtime_artifacts(&runtime.home_dir).is_some() {
        match stop_daemon_with(
            runtime,
            timeout_secs,
            &send_signal,
            &is_alive,
            timing.stop_poll_interval,
        )? {
            StopOutcome::Stopped { pid, already_dead } => {
                if already_dead {
                    println!("Daemon process {pid} no longer exists; cleaning up runtime files.");
                } else {
                    println!(
                        "Sent SIGTERM to daemon (PID {pid}); waiting up to {timeout_secs}s for exit..."
                    );
                    println!("Daemon (PID {pid}) has stopped.");
                }
            }
            StopOutcome::TimedOut { pid } => {
                println!(
                    "Sent SIGTERM to daemon (PID {pid}); waiting up to {timeout_secs}s for exit..."
                );
                println!(
                    "Daemon (PID {pid}) did not stop within {timeout_secs}s after SIGTERM; escalating to SIGKILL."
                );
                send_signal(pid, libc::SIGKILL)
                    .with_context(|| format!("failed to send SIGKILL to daemon process {pid}"))?;
                let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1));
                while Instant::now() < deadline {
                    if !is_alive(pid) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                if is_alive(pid) {
                    anyhow::bail!("daemon process {pid} remained alive after SIGKILL escalation");
                }
                cleanup_runtime_files(runtime);
            }
        }

        if !wait_for_runtime_files_absent(
            runtime,
            timing.runtime_absent_timeout,
            timing.runtime_absent_poll_interval,
        ) {
            cleanup_runtime_files(runtime);
        }
    }

    ensure_running().context("failed to autostart daemon after stop")?;
    if daemon_running() {
        println!("Daemon restarted successfully.");
    } else {
        anyhow::bail!("daemon did not come back online after restart");
    }
    Ok(())
}

fn wait_for_session_dead(team_name: &str, agent_name: &str, timeout_secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        match agent_team_mail_core::daemon_client::query_session_for_team(team_name, agent_name) {
            Ok(Some(info)) if !info.alive => return true,
            Ok(None) => return true,
            Ok(Some(_)) => {}
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Execute daemon status command
fn execute_status(args: StatusArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let status_path = daemon_status_path_for(&home_dir);

    // Check if status file exists
    if !status_path.exists() {
        if args.json {
            println!("{{\"error\": \"No daemon status found. Is the daemon running?\"}}");
        } else {
            eprintln!("No daemon status found. Is the daemon running?");
            eprintln!("Status file not found: {}", status_path.display());
        }
        std::process::exit(1);
    }

    // Read and parse status file
    let content =
        std::fs::read_to_string(&status_path).context("Failed to read daemon status file")?;

    let status: DaemonStatus =
        serde_json::from_str(&content).context("Failed to parse daemon status file")?;

    // Check if status is stale (timestamp older than 2x poll interval = 60 seconds)
    let stale_threshold_secs = 60;
    let is_stale = is_status_stale(&status.timestamp, stale_threshold_secs);

    let logging_health = build_logging_health_contract(&status.logging, &home_dir);
    let otel_health = build_otel_health_contract(&status.otel);

    if args.json {
        // Output as JSON with stale flag and canonical logging schema.
        let mut output = serde_json::to_value(&status)?;
        if let Some(obj) = output.as_object_mut() {
            obj.remove("logging");
            obj.remove("otel");
            obj.insert(
                "logging_health".to_string(),
                serde_json::to_value(&logging_health)?,
            );
            obj.insert(
                "otel_health".to_string(),
                serde_json::to_value(&otel_health)?,
            );
            obj.insert("stale".to_string(), serde_json::Value::Bool(is_stale));
        }
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        // Human-readable output
        println!("Daemon Status");
        println!("=============");
        println!("PID:         {}", status.pid);
        println!("Version:     {}", status.version);
        println!("Uptime:      {}", format_duration(status.uptime_secs));
        println!("Last update: {}", status.timestamp);
        println!(
            "Runtime:     {} ({})",
            status.owner.runtime_kind.as_str(),
            status.owner.build_profile.as_str()
        );
        println!("Executable:  {}", status.owner.executable_path);
        println!("ATM_HOME:    {}", status.owner.home_scope);

        if is_stale {
            println!();
            println!(
                "WARNING: Daemon status is stale (last update > {}s ago)",
                stale_threshold_secs
            );
            println!("         The daemon may not be running.");
        }

        if !status.teams.is_empty() {
            println!();
            println!("Teams ({}):", status.teams.len());
            for team in &status.teams {
                println!("  - {}", team);
            }
        }

        if !status.plugins.is_empty() {
            println!();
            println!("Plugins ({}):", status.plugins.len());
            for plugin in &status.plugins {
                let status_str = match plugin.status {
                    PluginStatusKind::Running => "running",
                    PluginStatusKind::Error => "error",
                    PluginStatusKind::Disabled => "disabled",
                    PluginStatusKind::DisabledInitError => "disabled_init_error",
                };

                let enabled_str = if plugin.enabled {
                    "enabled"
                } else {
                    "disabled"
                };

                print!("  {} - {} ({})", plugin.name, status_str, enabled_str);

                if let Some(ref error) = plugin.last_error {
                    print!(" - Error: {}", error);
                }

                if let Some(ref last_updated) = plugin.last_updated {
                    print!(" - Last updated: {}", last_updated);
                }

                println!();
            }
        }

        println!();
        println!("Logging:");
        println!("  state:           {}", status.logging.state);
        println!("  dropped_counter: {}", status.logging.dropped_counter);
        println!("  spool_path:      {}", status.logging.spool_path);
        println!(
            "  canonical_log_path: {}",
            status.logging.canonical_log_path
        );
        println!("  spool_count:     {}", status.logging.spool_count);
        if let Some(oldest_spool_age) = status.logging.oldest_spool_age {
            println!("  oldest_spool_age: {oldest_spool_age}s");
        }
        if let Some(last_error) = &status.logging.last_error {
            println!("  last_error:      {last_error}");
        }
        println!("  schema_version:  {}", logging_health.schema_version);
        println!("  log_root:        {}", logging_health.log_root);
        println!(
            "  dropped_events_total: {}",
            logging_health.dropped_events_total
        );
        println!("  spool_file_count: {}", logging_health.spool_file_count);
        if let Some(oldest_spool_age) = logging_health.oldest_spool_age_seconds {
            println!("  oldest_spool_age_seconds: {oldest_spool_age}s");
        }
        if let Some(last_code) = &logging_health.last_error.code {
            println!("  last_error.code: {last_code}");
        }
        if let Some(last_message) = &logging_health.last_error.message {
            println!("  last_error.message: {last_message}");
        }
        if let Some(last_at) = &logging_health.last_error.at {
            println!("  last_error.at:   {last_at}");
        }
        println!();
        println!("OTel:");
        println!("  schema_version:  {}", otel_health.schema_version);
        println!("  enabled:         {}", otel_health.enabled);
        println!("  protocol:        {}", otel_health.protocol);
        println!("  collector_state: {}", otel_health.collector_state);
        println!("  local_mirror_state: {}", otel_health.local_mirror_state);
        println!("  local_mirror_path:  {}", otel_health.local_mirror_path);
        println!("  debug_local_export: {}", otel_health.debug_local_export);
        println!("  debug_local_state:  {}", otel_health.debug_local_state);
        if let Some(endpoint) = &otel_health.collector_endpoint {
            println!("  collector_endpoint: {endpoint}");
        }
        if let Some(last_code) = &otel_health.last_error.code {
            println!("  last_error.code: {last_code}");
        }
        if let Some(last_message) = &otel_health.last_error.message {
            println!("  last_error.message: {last_message}");
        }
        if let Some(last_at) = &otel_health.last_error.at {
            println!("  last_error.at:   {last_at}");
        }
    }

    // Exit with error code if stale
    if is_stale {
        std::process::exit(1);
    }

    Ok(())
}

/// Check if status timestamp is stale
fn is_status_stale(timestamp: &str, threshold_secs: u64) -> bool {
    use chrono::DateTime;
    use std::time::UNIX_EPOCH;

    let parsed = match DateTime::parse_from_rfc3339(timestamp) {
        Ok(dt) => dt,
        Err(_) => return true, // If we can't parse, assume stale
    };

    let status_time = UNIX_EPOCH + Duration::from_secs(parsed.timestamp() as u64);
    let now = SystemTime::now();

    match now.duration_since(status_time) {
        Ok(elapsed) => elapsed.as_secs() > threshold_secs,
        Err(_) => true, // Clock skew or future timestamp
    }
}

/// Format duration as human-readable string
fn format_duration(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if days > 0 {
        format!("{}d {}h {}m {}s", days, hours, minutes, seconds)
    } else if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

// Re-export types from daemon crate for status file parsing
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DaemonStatus {
    timestamp: String,
    pid: u32,
    version: String,
    uptime_secs: u64,
    #[serde(default)]
    owner: RuntimeOwnerMetadata,
    plugins: Vec<PluginStatus>,
    teams: Vec<String>,
    #[serde(default)]
    logging: LoggingHealthSnapshot,
    #[serde(default)]
    otel: OtelHealthSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PluginStatus {
    name: String,
    enabled: bool,
    status: PluginStatusKind,
    last_error: Option<String>,
    last_updated: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PluginStatusKind {
    Running,
    Error,
    Disabled,
    #[serde(rename = "disabled_init_error")]
    DisabledInitError,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(30), "30s");
        assert_eq!(format_duration(90), "1m 30s");
        assert_eq!(format_duration(3661), "1h 1m 1s");
        assert_eq!(format_duration(86400), "1d 0h 0m 0s");
        assert_eq!(format_duration(90061), "1d 1h 1m 1s");
    }

    #[test]
    fn test_is_status_stale_fresh() {
        use chrono::Utc;

        let now = Utc::now();
        let timestamp = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        assert!(!is_status_stale(&timestamp, 60));
    }

    #[test]
    fn test_is_status_stale_old() {
        use chrono::{Duration as ChronoDuration, Utc};

        let old = Utc::now() - ChronoDuration::seconds(120);
        let timestamp = old.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        assert!(is_status_stale(&timestamp, 60));
    }

    #[test]
    fn test_is_status_stale_invalid() {
        assert!(is_status_stale("not-a-timestamp", 60));
    }

    #[cfg(unix)]
    fn temp_runtime_paths(tmp: &TempDir) -> DaemonRuntimePaths {
        let daemon_dir = tmp.path().join(".atm/daemon");
        DaemonRuntimePaths {
            home_dir: tmp.path().to_path_buf(),
            socket_path: daemon_dir.join("atm-daemon.sock"),
            status_path: daemon_dir.join("status.json"),
            metadata_path: daemon_dir.join("daemon.lock.meta.json"),
        }
    }

    #[cfg(unix)]
    fn write_runtime_pid(runtime: &DaemonRuntimePaths, pid: u32) {
        std::fs::create_dir_all(runtime.status_path.parent().unwrap()).expect("create daemon dir");
        let owner = agent_team_mail_core::daemon_client::RuntimeOwnerMetadata {
            runtime_kind: agent_team_mail_core::daemon_client::RuntimeKind::Shared,
            build_profile: agent_team_mail_core::daemon_client::BuildProfile::Debug,
            executable_path: "/tmp/atm-daemon".to_string(),
            home_scope: runtime.home_dir.to_string_lossy().into_owned(),
        };
        let metadata = agent_team_mail_core::daemon_client::DaemonLockMetadata {
            pid,
            owner,
            version: "test-version".to_string(),
            written_at: chrono::Utc::now().to_rfc3339(),
        };
        let status = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "pid": pid,
            "version": "test-version",
            "uptime_secs": 0,
            "owner": metadata.owner,
            "plugins": [],
            "teams": [],
            "logging": {},
            "otel": {}
        });
        std::fs::write(
            &runtime.metadata_path,
            serde_json::to_vec(&metadata).unwrap(),
        )
        .expect("write daemon metadata");
        std::fs::write(&runtime.status_path, serde_json::to_vec(&status).unwrap())
            .expect("write daemon status");
    }

    #[test]
    #[cfg(unix)]
    fn test_stop_daemon_cleans_socket_when_sigterm_reports_esrch() {
        let tmp = TempDir::new().expect("temp dir");
        let runtime = temp_runtime_paths(&tmp);
        write_runtime_pid(&runtime, 4242);
        std::fs::write(&runtime.socket_path, "").expect("write socket");

        let outcome = stop_daemon_with(
            &runtime,
            1,
            |_pid, _signal| Err(std::io::Error::from_raw_os_error(libc::ESRCH)),
            |_pid| true,
            Duration::from_millis(1),
        )
        .expect("stop should succeed");

        assert_eq!(
            outcome,
            StopOutcome::Stopped {
                pid: 4242,
                already_dead: true
            }
        );
        assert!(
            !runtime.metadata_path.exists(),
            "daemon metadata should be removed"
        );
        assert!(
            !runtime.status_path.exists(),
            "status file should be removed"
        );
        assert!(
            !runtime.socket_path.exists(),
            "socket file should be removed on ESRCH cleanup"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_stop_daemon_times_out_when_pid_stays_alive() {
        let tmp = TempDir::new().expect("temp dir");
        let runtime = temp_runtime_paths(&tmp);
        write_runtime_pid(&runtime, 4242);

        let outcome = stop_daemon_with(
            &runtime,
            1,
            |_pid, _signal| Ok(()),
            |_pid| true,
            Duration::from_millis(1),
        )
        .expect("stop should return timeout outcome");

        assert_eq!(outcome, StopOutcome::TimedOut { pid: 4242 });
    }

    #[test]
    #[cfg(unix)]
    fn test_stop_daemon_accepts_exit_detected_after_timeout_loop() {
        let tmp = TempDir::new().expect("temp dir");
        let runtime = temp_runtime_paths(&tmp);
        write_runtime_pid(&runtime, 4242);
        std::fs::write(&runtime.socket_path, "").expect("write socket");

        let outcome = stop_daemon_with(
            &runtime,
            0,
            |_pid, _signal| Ok(()),
            |_pid| false,
            Duration::from_millis(1),
        )
        .expect("stop should succeed");

        assert_eq!(
            outcome,
            StopOutcome::Stopped {
                pid: 4242,
                already_dead: false
            }
        );
        assert!(
            !runtime.metadata_path.exists(),
            "daemon metadata should be removed"
        );
        assert!(
            !runtime.status_path.exists(),
            "status file should be removed"
        );
        assert!(
            !runtime.socket_path.exists(),
            "socket file should be removed"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_stop_daemon_accepts_exit_during_post_timeout_grace() {
        let tmp = TempDir::new().expect("temp dir");
        let runtime = temp_runtime_paths(&tmp);
        write_runtime_pid(&runtime, 4242);
        std::fs::write(&runtime.socket_path, "").expect("write socket");

        let checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let outcome = stop_daemon_with(
            &runtime,
            0,
            |_pid, _signal| Ok(()),
            {
                let checks = std::sync::Arc::clone(&checks);
                move |_pid| checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
            },
            Duration::from_millis(1),
        )
        .expect("stop should succeed");

        assert_eq!(
            outcome,
            StopOutcome::Stopped {
                pid: 4242,
                already_dead: false
            }
        );
        assert!(
            !runtime.metadata_path.exists(),
            "daemon metadata should be removed"
        );
        assert!(
            !runtime.status_path.exists(),
            "status file should be removed"
        );
        assert!(
            !runtime.socket_path.exists(),
            "socket file should be removed"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_stop_daemon_removes_runtime_files() {
        let tmp = TempDir::new().expect("temp dir");
        let runtime = temp_runtime_paths(&tmp);
        write_runtime_pid(&runtime, 4242);
        std::fs::write(&runtime.socket_path, "").expect("write socket");

        let checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let outcome = stop_daemon_with(
            &runtime,
            1,
            |_pid, _signal| Ok(()),
            {
                let checks = std::sync::Arc::clone(&checks);
                move |_pid| checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
            },
            Duration::from_millis(1),
        )
        .expect("stop should succeed");

        assert_eq!(
            outcome,
            StopOutcome::Stopped {
                pid: 4242,
                already_dead: false
            }
        );
        assert!(
            !runtime.metadata_path.exists(),
            "daemon metadata should be removed"
        );
        assert!(
            !runtime.status_path.exists(),
            "status file should be removed"
        );
        assert!(
            !runtime.socket_path.exists(),
            "socket file should be removed"
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_wait_for_runtime_files_absent_returns_true_after_cleanup() {
        let tmp = TempDir::new().expect("temp dir");
        let runtime = temp_runtime_paths(&tmp);
        write_runtime_pid(&runtime, 4242);
        std::fs::write(&runtime.socket_path, "").expect("write socket");

        let metadata_path = runtime.metadata_path.clone();
        let socket_path = runtime.socket_path.clone();
        let status_path = runtime.status_path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            let _ = std::fs::remove_file(metadata_path);
            let _ = std::fs::remove_file(socket_path);
            let _ = std::fs::remove_file(status_path);
        });

        assert!(wait_for_runtime_files_absent(
            &runtime,
            Duration::from_millis(200),
            Duration::from_millis(5),
        ));
    }

    #[test]
    #[cfg(unix)]
    fn test_daemon_restart_produces_new_pid() {
        let tmp = TempDir::new().expect("temp dir");
        let runtime = temp_runtime_paths(&tmp);
        write_runtime_pid(&runtime, 4242);
        std::fs::write(&runtime.socket_path, "").expect("write socket");

        let signals = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let alive_checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let restarted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        restart_daemon_with(
            &runtime,
            1,
            {
                let signals = std::sync::Arc::clone(&signals);
                move |pid, signal| {
                    signals.lock().unwrap().push((pid, signal));
                    Ok(())
                }
            },
            {
                let alive_checks = std::sync::Arc::clone(&alive_checks);
                move |_pid| alive_checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
            },
            {
                let runtime = runtime.clone();
                let restarted = std::sync::Arc::clone(&restarted);
                move || {
                    write_runtime_pid(&runtime, 5252);
                    std::fs::write(&runtime.socket_path, "").expect("write new socket");
                    restarted.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }
            },
            || true,
            RestartTiming {
                stop_poll_interval: Duration::from_millis(1),
                runtime_absent_timeout: Duration::from_millis(50),
                runtime_absent_poll_interval: Duration::from_millis(1),
            },
        )
        .expect("restart should succeed");

        assert_eq!(*signals.lock().unwrap(), vec![(4242, libc::SIGTERM)]);
        assert_eq!(
            daemon_pid_from_runtime_artifacts(&runtime.home_dir),
            Some(5252),
            "restart should leave the new daemon metadata in place"
        );
        assert!(
            runtime.socket_path.exists(),
            "restart should recreate socket"
        );
        assert!(restarted.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn test_expected_daemon_binary_path_honors_atm_daemon_bin_override() {
        let old = std::env::var_os("ATM_DAEMON_BIN");
        let expected = std::env::temp_dir().join("custom-atm-daemon");
        unsafe {
            std::env::set_var("ATM_DAEMON_BIN", &expected);
        }
        assert_eq!(expected_daemon_binary_path(), Some(expected));
        match old {
            Some(value) => unsafe { std::env::set_var("ATM_DAEMON_BIN", value) },
            None => unsafe { std::env::remove_var("ATM_DAEMON_BIN") },
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_process_executable_matches_parts_checks_exe_and_cmd() {
        let expected = std::env::temp_dir().join("atm-daemon");
        let other = std::env::temp_dir().join("other");
        assert!(process_executable_matches_parts(
            Some(&expected),
            None,
            &expected
        ));
        assert!(process_executable_matches_parts(
            None,
            Some(&std::ffi::OsString::from(expected.clone())),
            &expected
        ));
        assert!(!process_executable_matches_parts(
            None,
            Some(&std::ffi::OsString::from(other)),
            &expected
        ));
    }

    #[test]
    #[cfg(unix)]
    fn test_matching_daemon_process_ids_empty_for_missing_path() {
        let missing = PathBuf::from("/definitely/missing/atm-daemon-test");
        assert!(matching_daemon_process_ids(&missing).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn test_stop_matching_daemon_processes_fast_path_when_no_pids() {
        let tmp = TempDir::new().expect("temp dir");
        let runtime = temp_runtime_paths(&tmp);

        let killed = stop_matching_daemon_processes_for_pids(
            Vec::new(),
            &runtime,
            |_pid, _signal| Ok(()),
            |_pid| true,
            RestartTiming {
                stop_poll_interval: Duration::from_millis(1),
                runtime_absent_timeout: Duration::from_millis(1),
                runtime_absent_poll_interval: Duration::from_millis(1),
            },
        )
        .expect("empty pid list should be a no-op");

        assert!(killed.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn test_stop_matching_daemon_processes_all_exit_before_sigkill() {
        let tmp = TempDir::new().expect("temp dir");
        let runtime = temp_runtime_paths(&tmp);
        write_runtime_pid(&runtime, 1111);
        std::fs::write(&runtime.socket_path, "").expect("write socket");

        let signals = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let alive_checks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let pids = vec![1111, 2222];

        let stopped = stop_matching_daemon_processes_for_pids(
            pids.clone(),
            &runtime,
            {
                let signals = std::sync::Arc::clone(&signals);
                move |pid, signal| {
                    signals.lock().unwrap().push((pid, signal));
                    Ok(())
                }
            },
            {
                let alive_checks = std::sync::Arc::clone(&alive_checks);
                move |_pid| alive_checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2
            },
            RestartTiming {
                stop_poll_interval: Duration::from_millis(1),
                runtime_absent_timeout: Duration::from_millis(1),
                runtime_absent_poll_interval: Duration::from_millis(1),
            },
        )
        .expect("all pids should exit after SIGTERM");

        assert_eq!(stopped, pids);
        assert_eq!(
            *signals.lock().unwrap(),
            vec![(1111, libc::SIGTERM), (2222, libc::SIGTERM)]
        );
        assert!(!runtime.metadata_path.exists());
        assert!(!runtime.status_path.exists());
        assert!(!runtime.socket_path.exists());
    }

    #[test]
    #[cfg(unix)]
    fn test_stop_matching_daemon_processes_bails_when_sigkill_survivor_remains() {
        let tmp = TempDir::new().expect("temp dir");
        let runtime = temp_runtime_paths(&tmp);
        let signals = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        let err = stop_matching_daemon_processes_for_pids(
            vec![3333],
            &runtime,
            {
                let signals = std::sync::Arc::clone(&signals);
                move |pid, signal| {
                    signals.lock().unwrap().push((pid, signal));
                    Ok(())
                }
            },
            |_pid| true,
            RestartTiming {
                stop_poll_interval: Duration::from_millis(1),
                runtime_absent_timeout: Duration::from_millis(1),
                runtime_absent_poll_interval: Duration::from_millis(1),
            },
        )
        .expect_err("surviving pid should fail cleanup");

        let msg = format!("{err:#}");
        assert!(msg.contains("3333"));
        assert_eq!(
            *signals.lock().unwrap(),
            vec![(3333, libc::SIGTERM), (3333, libc::SIGKILL)]
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_stop_matching_daemon_processes_errors_on_non_esrch_sigterm_failure() {
        let tmp = TempDir::new().expect("temp dir");
        let runtime = temp_runtime_paths(&tmp);

        let err = stop_matching_daemon_processes_for_pids(
            vec![4444],
            &runtime,
            |_pid, _signal| Err(std::io::Error::from_raw_os_error(libc::EPERM)),
            |_pid| true,
            RestartTiming {
                stop_poll_interval: Duration::from_millis(1),
                runtime_absent_timeout: Duration::from_millis(1),
                runtime_absent_poll_interval: Duration::from_millis(1),
            },
        )
        .expect_err("unexpected SIGTERM error should fail");

        let msg = format!("{err:#}");
        assert!(msg.contains("failed to send SIGTERM to daemon process 4444"));
    }
}
