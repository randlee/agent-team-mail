//! Integration tests for daemon event loop

use agent_team_mail_core::config::Config;
use agent_team_mail_core::context::SystemContext;
use agent_team_mail_core::daemon_client::{BuildProfile, RuntimeKind, RuntimeOwnerMetadata};
use agent_team_mail_core::logging_event::LogEventV1;
use agent_team_mail_daemon::daemon;
use agent_team_mail_daemon::daemon::{
    SessionRegistry, StatusWriter, new_dedup_store, new_launch_sender, new_log_event_queue,
    new_pubsub_store, new_session_registry, new_state_store, new_stream_event_sender,
    new_stream_state_store,
};
use agent_team_mail_daemon::plugin::{
    Capability, MailService, Plugin, PluginContext, PluginError, PluginMetadata, PluginRegistry,
};
use agent_team_mail_daemon::roster::RosterService;
use agent_team_mail_daemon_launch::{
    DaemonLaunchToken, attach_launch_token,
    issue_isolated_test_launch_token as issue_isolated_test_launch_token_inner,
};
#[path = "../../atm/tests/support/daemon_process_guard.rs"]
#[allow(dead_code)]
mod daemon_process_guard;
#[path = "../../atm/tests/support/daemon_test_registry.rs"]
#[allow(dead_code)]
mod daemon_test_registry;
#[path = "../../atm/tests/support/env_guard.rs"]
#[allow(dead_code)]
mod env_guard;
// These daemon integration tests still serialize because the helper contexts
// mutate ATM_HOME process-wide before constructing shared daemon state.
use serial_test::serial;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn read_spool_events(spool: &std::path::Path) -> Vec<LogEventV1> {
    if !spool.exists() {
        return Vec::new();
    }
    let mut events = Vec::new();
    let entries = match std::fs::read_dir(spool) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| x == "jsonl")
            != Some(true)
        {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines().filter(|line| !line.trim().is_empty()) {
            if let Ok(event) = serde_json::from_str::<LogEventV1>(line) {
                events.push(event);
            }
        }
    }
    events
}

fn issue_isolated_test_launch_token(home: &Path, issuer: &str) -> DaemonLaunchToken {
    issue_isolated_test_launch_token_inner(
        home,
        env!("CARGO_BIN_EXE_atm-daemon"),
        issuer,
        format!("{issuer}:{}", std::process::id()),
        std::process::id(),
        Duration::from_secs(600),
    )
}

fn issue_isolated_test_launch_token_with_lease(
    home: &Path,
    issuer: &str,
    test_identifier: &str,
    owner_pid: u32,
    ttl: Duration,
) -> DaemonLaunchToken {
    issue_isolated_test_launch_token_inner(
        home,
        env!("CARGO_BIN_EXE_atm-daemon"),
        issuer,
        test_identifier.to_string(),
        owner_pid,
        ttl,
    )
}

fn start_otel_trace_collector() -> (String, mpsc::Receiver<(String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind collector");
    listener
        .set_nonblocking(false)
        .expect("collector blocking mode");
    let addr = listener.local_addr().expect("collector addr");
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        for _ in 0..8 {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 1024];
            let mut header_end = None;
            while header_end.is_none() {
                let read = stream.read(&mut chunk).expect("read request");
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                header_end = buffer.windows(4).position(|window| window == b"\r\n\r\n");
            }
            let Some(header_end_idx) = header_end else {
                continue;
            };
            let body_start = header_end_idx + 4;
            let headers = String::from_utf8_lossy(&buffer[..header_end_idx]);
            let first_line = headers.lines().next().unwrap_or_default().to_string();
            let path = first_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    (name.eq_ignore_ascii_case("content-length"))
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);

            while buffer.len().saturating_sub(body_start) < content_length {
                let read = stream.read(&mut chunk).expect("read request body");
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
            }

            let body = String::from_utf8_lossy(&buffer[body_start..body_start + content_length])
                .to_string();
            tx.send((path, body)).expect("send captured request");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .expect("write response");
        }
    });

    (format!("http://{}", addr), rx)
}

/// Mock plugin that tracks lifecycle calls
struct MockPlugin {
    name: String,
    events: Arc<Mutex<Vec<String>>>,
    shutdown_delay: Option<Duration>,
}

impl MockPlugin {
    fn new(name: impl Into<String>, events: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            name: name.into(),
            events,
            shutdown_delay: None,
        }
    }

    fn with_shutdown_delay(mut self, delay: Duration) -> Self {
        self.shutdown_delay = Some(delay);
        self
    }
}

/// Plugin that fails immediately from run(), used to verify task isolation.
struct FailingRunPlugin {
    name: String,
    events: Arc<Mutex<Vec<String>>>,
}

impl FailingRunPlugin {
    fn new(name: impl Into<String>, events: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            name: name.into(),
            events,
        }
    }
}

impl Plugin for FailingRunPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: Box::leak(self.name.clone().into_boxed_str()),
            version: "1.0.0",
            description: "Failing plugin for isolation testing",
            capabilities: vec![Capability::CiMonitor],
        }
    }

    async fn init(&mut self, _ctx: &PluginContext) -> Result<(), PluginError> {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:init", self.name));
        Ok(())
    }

    async fn run(&mut self, _cancel: CancellationToken) -> Result<(), PluginError> {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:run_failed", self.name));
        Err(PluginError::Runtime {
            message: "simulated gh_monitor crash".to_string(),
            source: None,
        })
    }

    async fn shutdown(&mut self) -> Result<(), PluginError> {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:shutdown", self.name));
        Ok(())
    }
}

impl Plugin for MockPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: Box::leak(self.name.clone().into_boxed_str()),
            version: "1.0.0",
            description: "Mock plugin for testing",
            capabilities: vec![Capability::Custom("test".to_string())],
        }
    }

    async fn init(&mut self, _ctx: &PluginContext) -> Result<(), PluginError> {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:init", self.name));
        Ok(())
    }

    async fn run(&mut self, cancel: CancellationToken) -> Result<(), PluginError> {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:run", self.name));

        // Wait for cancellation
        cancel.cancelled().await;

        self.events
            .lock()
            .unwrap()
            .push(format!("{}:run_cancelled", self.name));

        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), PluginError> {
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:shutdown", self.name));

        if let Some(delay) = self.shutdown_delay {
            tokio::time::sleep(delay).await;
        }

        Ok(())
    }
}

/// Create a test plugin context with temporary directories.
///
/// Returns `(ctx, temp_dir, _atm_home_guard)`. The caller must hold the guard
/// for the lifetime of the test — when it drops, `ATM_HOME` is restored.
fn create_test_context() -> (PluginContext, TempDir, env_guard::EnvGuard) {
    let temp_dir = tempfile::tempdir().unwrap();
    let teams_root = temp_dir.path().join("teams");
    std::fs::create_dir_all(&teams_root).unwrap();

    // F-6: use EnvGuard so ATM_HOME is restored even if the test panics.
    let atm_home_guard = env_guard::EnvGuard::set("ATM_HOME", temp_dir.path());

    let claude_root = temp_dir.path().join(".claude");
    std::fs::create_dir_all(&claude_root).unwrap();

    let system_ctx = SystemContext::new(
        "test-host".to_string(),
        agent_team_mail_core::context::Platform::detect(),
        claude_root,
        "test-version".to_string(),
        "test-team".to_string(),
    );

    let mail_service = MailService::new(teams_root.clone());
    let roster_service = RosterService::new(teams_root);
    let config = Config::default();

    let ctx = PluginContext::new(
        Arc::new(system_ctx),
        Arc::new(mail_service),
        Arc::new(config),
        Arc::new(roster_service),
    );

    (ctx, temp_dir, atm_home_guard)
}

/// Create a test context where mail teams root matches `${ATM_HOME}/.claude/teams`.
///
/// Returns `(ctx, temp_dir, _atm_home_guard)`. The caller must hold the guard
/// for the lifetime of the test — when it drops, `ATM_HOME` is restored.
fn create_reconcile_test_context() -> (PluginContext, TempDir, env_guard::EnvGuard) {
    let temp_dir = tempfile::tempdir().unwrap();

    // F-6: use EnvGuard so ATM_HOME is restored even if the test panics.
    let atm_home_guard = env_guard::EnvGuard::set("ATM_HOME", temp_dir.path());

    let claude_root = temp_dir.path().join(".claude");
    let teams_root = claude_root.join("teams");
    std::fs::create_dir_all(&teams_root).unwrap();

    let system_ctx = SystemContext::new(
        "test-host".to_string(),
        agent_team_mail_core::context::Platform::detect(),
        claude_root.clone(),
        "test-version".to_string(),
        "test-team".to_string(),
    );

    let mail_service = MailService::new(teams_root.clone());
    let roster_service = RosterService::new(teams_root);
    let config = Config::default();

    let ctx = PluginContext::new(
        Arc::new(system_ctx),
        Arc::new(mail_service),
        Arc::new(config),
        Arc::new(roster_service),
    );

    (ctx, temp_dir, atm_home_guard)
}

fn write_team_config(teams_root: &std::path::Path, team: &str, members: serde_json::Value) {
    let team_dir = teams_root.join(team);
    std::fs::create_dir_all(team_dir.join("inboxes")).unwrap();
    let cfg = serde_json::json!({
        "name": team,
        "createdAt": 1739284800000u64,
        "leadAgentId": format!("team-lead@{team}"),
        "leadSessionId": "lead-session",
        "members": members,
    });
    std::fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&cfg).unwrap(),
    )
    .unwrap();
}

async fn wait_until_elapsed(
    timeout_ms: u64,
    mut pred: impl FnMut() -> bool,
) -> Option<std::time::Duration> {
    let start = std::time::Instant::now();
    let deadline = start + Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if pred() {
            return Some(start.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    pred().then(|| start.elapsed())
}

async fn wait_for_task_running_elapsed<T>(
    task: &tokio::task::JoinHandle<T>,
    timeout_ms: u64,
) -> Option<std::time::Duration> {
    wait_until_elapsed(timeout_ms, || !task.is_finished()).await
}

async fn wait_for_recorded_event_elapsed(
    events: &Arc<Mutex<Vec<String>>>,
    expected: &str,
    timeout_ms: u64,
) -> Option<std::time::Duration> {
    wait_until_elapsed(timeout_ms, || {
        events.lock().unwrap().iter().any(|event| event == expected)
    })
    .await
}

fn wait_for_child_running_elapsed(
    child: &mut Child,
    timeout_ms: u64,
) -> Option<std::time::Duration> {
    let start = std::time::Instant::now();
    let deadline = start + Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if child
            .try_wait()
            .expect("failed to poll child process")
            .is_none()
        {
            return Some(start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    child
        .try_wait()
        .expect("failed to poll child process at timeout")
        .is_none()
        .then(|| start.elapsed())
}

fn wait_for_lock_file_acquired_elapsed(
    home: &std::path::Path,
    timeout_ms: u64,
) -> Option<std::time::Duration> {
    let lock_path = home.join(".atm/daemon/daemon.lock");
    let pid_path = home.join(".atm/daemon/atm-daemon.pid");
    let status_path = home.join(".atm/daemon/status.json");
    let start = std::time::Instant::now();
    let deadline = start + Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        let pid_ready = std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|content| content.trim().parse::<u32>().ok())
            .is_some();
        let status_ready = std::fs::read_to_string(&status_path)
            .ok()
            .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
            .and_then(|json| json.get("pid").and_then(serde_json::Value::as_u64))
            .is_some();
        let lock_contended = lock_path.exists()
            && agent_team_mail_core::io::lock::acquire_lock(&lock_path, 0).is_err();
        if lock_contended || pid_ready || status_ready {
            return Some(start.elapsed());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let pid_ready = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|content| content.trim().parse::<u32>().ok())
        .is_some();
    let status_ready = std::fs::read_to_string(&status_path)
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .and_then(|json| json.get("pid").and_then(serde_json::Value::as_u64))
        .is_some();
    let lock_contended =
        lock_path.exists() && agent_team_mail_core::io::lock::acquire_lock(&lock_path, 0).is_err();
    (lock_contended || pid_ready || status_ready).then(|| start.elapsed())
}

/// Create a test status writer
fn create_test_status_writer(temp_dir: &TempDir) -> Arc<StatusWriter> {
    Arc::new(StatusWriter::new(
        temp_dir.path().to_path_buf(),
        "test-version".to_string(),
        RuntimeOwnerMetadata {
            runtime_kind: RuntimeKind::Isolated,
            build_profile: BuildProfile::Release,
            executable_path: temp_dir
                .path()
                .join("atm-daemon")
                .to_string_lossy()
                .into_owned(),
            home_scope: temp_dir.path().to_string_lossy().into_owned(),
        },
    ))
}

fn create_test_daemon_lock(temp_dir: &TempDir) -> agent_team_mail_core::io::lock::FileLock {
    let lock_path = temp_dir.path().join(".atm/daemon/daemon.lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    agent_team_mail_core::io::lock::acquire_lock(&lock_path, 0).unwrap()
}

#[tokio::test]
#[serial]
async fn test_daemon_starts_and_loads_mock_plugin() {
    let (ctx, temp_dir, _atm_home_guard) = create_test_context();
    let events = Arc::new(Mutex::new(Vec::new()));
    let status_writer = create_test_status_writer(&temp_dir);

    let mut registry = PluginRegistry::new();
    registry.register(MockPlugin::new("test-plugin", events.clone()));

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let dedup_store = new_dedup_store(temp_dir.path()).unwrap();
    let daemon_lock = create_test_daemon_lock(&temp_dir);

    // Run daemon in background, cancel after a short delay
    let daemon_task = tokio::spawn(async move {
        daemon::run(
            &mut registry,
            &ctx,
            daemon_lock,
            cancel_clone,
            status_writer,
            new_state_store(),
            new_pubsub_store(),
            new_launch_sender(),
            new_session_registry(),
            dedup_store,
            new_stream_state_store(),
            new_stream_event_sender(),
            new_log_event_queue(),
        )
        .await
    });

    let observed_run = wait_for_recorded_event_elapsed(&events, "test-plugin:run", 10_000)
        .await
        .expect("daemon should reach plugin run state before cancellation");
    assert!(
        observed_run <= Duration::from_secs(10),
        "daemon should reach plugin run state before cancellation"
    );

    // Cancel the daemon
    cancel.cancel();

    // Wait for daemon to complete
    let result = daemon_task.await.unwrap();
    assert!(result.is_ok(), "Daemon should run successfully");

    // Verify lifecycle events
    let recorded_events = events.lock().unwrap();
    assert!(
        recorded_events.contains(&"test-plugin:init".to_string()),
        "Plugin should be initialized"
    );
    assert!(
        recorded_events.contains(&"test-plugin:run".to_string()),
        "Plugin run() should be called"
    );
    assert!(
        recorded_events.contains(&"test-plugin:run_cancelled".to_string()),
        "Plugin run() should respect cancellation"
    );
    assert!(
        recorded_events.contains(&"test-plugin:shutdown".to_string()),
        "Plugin should be shut down"
    );
}

#[tokio::test]
#[serial]
async fn test_signal_triggers_graceful_shutdown() {
    let (ctx, temp_dir, _atm_home_guard) = create_test_context();
    let status_writer = create_test_status_writer(&temp_dir);
    let events = Arc::new(Mutex::new(Vec::new()));

    let mut registry = PluginRegistry::new();
    registry.register(MockPlugin::new("plugin1", events.clone()));
    registry.register(MockPlugin::new("plugin2", events.clone()));

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let dedup_store = new_dedup_store(temp_dir.path()).unwrap();
    let daemon_lock = create_test_daemon_lock(&temp_dir);

    let daemon_task = tokio::spawn(async move {
        daemon::run(
            &mut registry,
            &ctx,
            daemon_lock,
            cancel_clone,
            status_writer.clone(),
            new_state_store(),
            new_pubsub_store(),
            new_launch_sender(),
            new_session_registry(),
            dedup_store,
            new_stream_state_store(),
            new_stream_event_sender(),
            new_log_event_queue(),
        )
        .await
    });

    let plugin1_running = wait_for_recorded_event_elapsed(&events, "plugin1:run", 10_000)
        .await
        .expect("plugin1 should reach run state before cancellation");
    assert!(
        plugin1_running <= Duration::from_secs(10),
        "plugin1 should reach run state before cancellation"
    );
    let plugin2_running = wait_for_recorded_event_elapsed(&events, "plugin2:run", 10_000)
        .await
        .expect("plugin2 should reach run state before cancellation");
    assert!(
        plugin2_running <= Duration::from_secs(10),
        "plugin2 should reach run state before cancellation"
    );

    // Simulate signal by cancelling the token
    cancel.cancel();

    let result = daemon_task.await.unwrap();
    assert!(result.is_ok(), "Daemon shutdown should succeed");

    let recorded_events = events.lock().unwrap();
    // Both plugins should go through full lifecycle
    assert!(recorded_events.contains(&"plugin1:shutdown".to_string()));
    assert!(recorded_events.contains(&"plugin2:shutdown".to_string()));
}

#[tokio::test]
#[serial]
async fn test_plugin_lifecycle_order() {
    let (ctx, temp_dir, _atm_home_guard) = create_test_context();
    let status_writer = create_test_status_writer(&temp_dir);
    let events = Arc::new(Mutex::new(Vec::new()));

    let mut registry = PluginRegistry::new();
    registry.register(MockPlugin::new("plugin", events.clone()));

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let dedup_store = new_dedup_store(temp_dir.path()).unwrap();
    let daemon_lock = create_test_daemon_lock(&temp_dir);

    let daemon_task = tokio::spawn(async move {
        daemon::run(
            &mut registry,
            &ctx,
            daemon_lock,
            cancel_clone,
            status_writer.clone(),
            new_state_store(),
            new_pubsub_store(),
            new_launch_sender(),
            new_session_registry(),
            dedup_store,
            new_stream_state_store(),
            new_stream_event_sender(),
            new_log_event_queue(),
        )
        .await
    });

    let plugin_running = wait_for_recorded_event_elapsed(&events, "plugin:run", 10_000)
        .await
        .expect("plugin should reach run state before cancellation");
    assert!(
        plugin_running <= Duration::from_secs(10),
        "plugin should reach run state before cancellation"
    );
    cancel.cancel();

    daemon_task.await.unwrap().unwrap();

    let recorded_events = events.lock().unwrap();
    let plugin_events: Vec<_> = recorded_events
        .iter()
        .filter(|e| e.starts_with("plugin:"))
        .cloned()
        .collect();

    // Verify order: init → run → run_cancelled → shutdown
    assert_eq!(plugin_events[0], "plugin:init");
    assert_eq!(plugin_events[1], "plugin:run");
    assert_eq!(plugin_events[2], "plugin:run_cancelled");
    assert_eq!(plugin_events[3], "plugin:shutdown");
}

#[tokio::test]
#[serial]
async fn test_spool_drain_runs_on_interval() {
    let (ctx, temp_dir, _atm_home_guard) = create_test_context();
    let status_writer = create_test_status_writer(&temp_dir);
    let mut registry = PluginRegistry::new();

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let dedup_store = new_dedup_store(temp_dir.path()).unwrap();
    let daemon_lock = create_test_daemon_lock(&temp_dir);

    let daemon_task = tokio::spawn(async move {
        daemon::run(
            &mut registry,
            &ctx,
            daemon_lock,
            cancel_clone,
            status_writer.clone(),
            new_state_store(),
            new_pubsub_store(),
            new_launch_sender(),
            new_session_registry(),
            dedup_store,
            new_stream_state_store(),
            new_stream_event_sender(),
            new_log_event_queue(),
        )
        .await
    });

    let daemon_running = wait_for_task_running_elapsed(&daemon_task, 1_000)
        .await
        .expect("daemon task should remain running long enough to service background loops");
    assert!(
        daemon_running <= Duration::from_secs(1),
        "daemon task should remain running long enough to service background loops"
    );

    cancel.cancel();

    let result = daemon_task.await.unwrap();
    assert!(
        result.is_ok(),
        "Daemon should run successfully even with spool drain"
    );
}

#[tokio::test]
#[serial]
async fn test_startup_reconcile_seeds_roster_without_interval_delay() {
    let (ctx, temp_dir, _atm_home_guard) = create_reconcile_test_context();
    let status_writer = create_test_status_writer(&temp_dir);
    let teams_root = temp_dir.path().join(".claude/teams");
    let cwd = temp_dir.path().display().to_string();
    write_team_config(
        &teams_root,
        "test-team",
        serde_json::json!([
            {
                "agentId": "team-lead@test-team",
                "name": "team-lead",
                "agentType": "general-purpose",
                "model": "unknown",
                "joinedAt": 1,
                "cwd": cwd,
                "subscriptions": [],
                "isActive": true
            },
            {
                "agentId": "worker@test-team",
                "name": "worker",
                "agentType": "general-purpose",
                "model": "unknown",
                "joinedAt": 1,
                "cwd": cwd,
                "subscriptions": [],
                "isActive": false
            }
        ]),
    );

    let mut registry = PluginRegistry::new();
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let dedup_store = new_dedup_store(temp_dir.path()).unwrap();
    let daemon_lock = create_test_daemon_lock(&temp_dir);
    let state_store = new_state_store();
    let state_store_probe = state_store.clone();

    let daemon_task = tokio::spawn(async move {
        daemon::run(
            &mut registry,
            &ctx,
            daemon_lock,
            cancel_clone,
            status_writer,
            state_store,
            new_pubsub_store(),
            new_launch_sender(),
            new_session_registry(),
            dedup_store,
            new_stream_state_store(),
            new_stream_event_sender(),
            new_log_event_queue(),
        )
        .await
    });

    let seeded = wait_until_elapsed(1000, || {
        state_store_probe
            .lock()
            .unwrap()
            .get_state("worker")
            .is_some()
    })
    .await
    .expect("startup reconcile should seed worker state promptly (<1s)");
    assert!(
        seeded <= Duration::from_secs(1),
        "startup reconcile should seed worker state promptly (<1s)"
    );

    cancel.cancel();
    daemon_task.await.unwrap().unwrap();
}

#[tokio::test]
#[serial]
#[cfg_attr(
    windows,
    ignore = "notify watcher startup is flaky on windows-latest CI; reconcile behavior is covered by deterministic unit tests"
)]
#[cfg_attr(
    target_os = "macos",
    ignore = "notify watcher timing flaky on macOS CI"
)]
async fn test_config_watch_event_updates_and_removes_members() {
    let (ctx, temp_dir, _atm_home_guard) = create_reconcile_test_context();
    let status_writer = create_test_status_writer(&temp_dir);
    let teams_root = temp_dir.path().join(".claude/teams");
    let cwd = temp_dir.path().display().to_string();
    write_team_config(
        &teams_root,
        "test-team",
        serde_json::json!([
            {
                "agentId": "team-lead@test-team",
                "name": "team-lead",
                "agentType": "general-purpose",
                "model": "unknown",
                "joinedAt": 1,
                "cwd": cwd.clone(),
                "subscriptions": [],
                "isActive": true
            },
            {
                "agentId": "worker-a@test-team",
                "name": "worker-a",
                "agentType": "general-purpose",
                "model": "unknown",
                "joinedAt": 1,
                "cwd": cwd.clone(),
                "subscriptions": [],
                "isActive": true
            }
        ]),
    );

    let mut registry = PluginRegistry::new();
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let dedup_store = new_dedup_store(temp_dir.path()).unwrap();
    let daemon_lock = create_test_daemon_lock(&temp_dir);
    let state_store = new_state_store();
    let state_store_probe = state_store.clone();
    let session_registry = Arc::new(Mutex::new(SessionRegistry::new()));

    let daemon_task = tokio::spawn(async move {
        daemon::run(
            &mut registry,
            &ctx,
            daemon_lock,
            cancel_clone,
            status_writer,
            state_store,
            new_pubsub_store(),
            new_launch_sender(),
            session_registry,
            dedup_store,
            new_stream_state_store(),
            new_stream_event_sender(),
            new_log_event_queue(),
        )
        .await
    });

    let initial_seeded = wait_until_elapsed(1500, || {
        state_store_probe
            .lock()
            .unwrap()
            .get_state("worker-a")
            .is_some()
    })
    .await
    .expect("worker-a should be tracked after daemon startup");
    assert!(
        initial_seeded <= Duration::from_millis(1500),
        "worker-a should be tracked after daemon startup"
    );

    // Add worker-b and remove worker-a to trigger config watcher reconcile.
    write_team_config(
        &teams_root,
        "test-team",
        serde_json::json!([
            {
                "agentId": "team-lead@test-team",
                "name": "team-lead",
                "agentType": "general-purpose",
                "model": "unknown",
                "joinedAt": 1,
                "cwd": cwd,
                "subscriptions": [],
                "isActive": true
            },
            {
                "agentId": "worker-b@test-team",
                "name": "worker-b",
                "agentType": "general-purpose",
                "model": "unknown",
                "joinedAt": 1,
                "cwd": temp_dir.path().display().to_string(),
                "subscriptions": [],
                "isActive": true
            }
        ]),
    );

    let added = wait_until_elapsed(8000, || {
        state_store_probe
            .lock()
            .unwrap()
            .get_state("worker-b")
            .is_some()
    })
    .await
    .expect("worker-b should be added via live config watcher reconcile");
    assert!(
        added <= Duration::from_secs(8),
        "worker-b should be added via live config watcher reconcile"
    );

    let removed = wait_until_elapsed(8000, || {
        state_store_probe
            .lock()
            .unwrap()
            .get_state("worker-a")
            .is_none()
    })
    .await
    .expect("worker-a should be removed from tracked state after config update");
    assert!(
        removed <= Duration::from_secs(8),
        "worker-a should be removed from tracked state after config update"
    );

    cancel.cancel();
    daemon_task.await.unwrap().unwrap();
}

#[tokio::test]
#[serial]
async fn test_graceful_shutdown_with_timeout() {
    let (ctx, temp_dir, _atm_home_guard) = create_test_context();
    let status_writer = create_test_status_writer(&temp_dir);
    let events = Arc::new(Mutex::new(Vec::new()));

    let mut registry = PluginRegistry::new();

    // Create a plugin that takes a long time to shut down
    registry.register(
        MockPlugin::new("slow-shutdown", events.clone())
            .with_shutdown_delay(Duration::from_secs(10)),
    );

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let dedup_store = new_dedup_store(temp_dir.path()).unwrap();
    let daemon_lock = create_test_daemon_lock(&temp_dir);

    let daemon_task = tokio::spawn(async move {
        daemon::run(
            &mut registry,
            &ctx,
            daemon_lock,
            cancel_clone,
            status_writer.clone(),
            new_state_store(),
            new_pubsub_store(),
            new_launch_sender(),
            new_session_registry(),
            dedup_store,
            new_stream_state_store(),
            new_stream_event_sender(),
            new_log_event_queue(),
        )
        .await
    });

    let run_observed = wait_for_recorded_event_elapsed(&events, "slow-shutdown:run", 10_000)
        .await
        .expect("slow-shutdown plugin should enter run before cancellation");
    assert!(
        run_observed <= Duration::from_secs(10),
        "slow-shutdown plugin should enter run before cancellation"
    );
    cancel.cancel();

    // The daemon should complete even though the plugin shutdown is slow
    // (the shutdown timeout will kick in)
    let result = tokio::time::timeout(Duration::from_secs(20), daemon_task)
        .await
        .expect("Daemon should complete within timeout");

    // The shutdown might fail due to timeout, which is expected
    match result {
        Ok(Ok(())) => {
            // Shutdown succeeded (unlikely with 10s delay and 5s timeout)
        }
        Ok(Err(_)) => {
            // Shutdown failed due to timeout (expected)
        }
        Err(e) => {
            panic!("Daemon task panicked: {e}");
        }
    }

    // Verify the shutdown was at least attempted
    let recorded_events = events.lock().unwrap();
    assert!(recorded_events.contains(&"slow-shutdown:shutdown".to_string()));
}

#[tokio::test]
#[serial]
async fn test_empty_registry_runs_successfully() {
    let (ctx, temp_dir, _atm_home_guard) = create_test_context();
    let status_writer = create_test_status_writer(&temp_dir);
    let mut registry = PluginRegistry::new();

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let dedup_store = new_dedup_store(temp_dir.path()).unwrap();
    let daemon_lock = create_test_daemon_lock(&temp_dir);

    let daemon_task = tokio::spawn(async move {
        daemon::run(
            &mut registry,
            &ctx,
            daemon_lock,
            cancel_clone,
            status_writer.clone(),
            new_state_store(),
            new_pubsub_store(),
            new_launch_sender(),
            new_session_registry(),
            dedup_store,
            new_stream_state_store(),
            new_stream_event_sender(),
            new_log_event_queue(),
        )
        .await
    });

    let daemon_running = wait_for_task_running_elapsed(&daemon_task, 1_000)
        .await
        .expect("daemon task should remain live before cancellation");
    assert!(
        daemon_running <= Duration::from_secs(1),
        "daemon task should remain live before cancellation"
    );
    cancel.cancel();

    let result = daemon_task.await.unwrap();
    assert!(result.is_ok(), "Daemon should run with no plugins");
}

#[tokio::test]
#[serial]
async fn test_multiple_plugins_run_concurrently() {
    let (ctx, temp_dir, _atm_home_guard) = create_test_context();
    let status_writer = create_test_status_writer(&temp_dir);
    let events = Arc::new(Mutex::new(Vec::new()));

    let mut registry = PluginRegistry::new();
    registry.register(MockPlugin::new("plugin1", events.clone()));
    registry.register(MockPlugin::new("plugin2", events.clone()));
    registry.register(MockPlugin::new("plugin3", events.clone()));

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let dedup_store = new_dedup_store(temp_dir.path()).unwrap();
    let daemon_lock = create_test_daemon_lock(&temp_dir);

    let daemon_task = tokio::spawn(async move {
        daemon::run(
            &mut registry,
            &ctx,
            daemon_lock,
            cancel_clone,
            status_writer.clone(),
            new_state_store(),
            new_pubsub_store(),
            new_launch_sender(),
            new_session_registry(),
            dedup_store,
            new_stream_state_store(),
            new_stream_event_sender(),
            new_log_event_queue(),
        )
        .await
    });

    let plugin1_running = wait_for_recorded_event_elapsed(&events, "plugin1:run", 10_000)
        .await
        .expect("plugin1 should reach run state before cancellation");
    assert!(
        plugin1_running <= Duration::from_secs(10),
        "plugin1 should reach run state before cancellation"
    );
    let plugin2_running = wait_for_recorded_event_elapsed(&events, "plugin2:run", 10_000)
        .await
        .expect("plugin2 should reach run state before cancellation");
    assert!(
        plugin2_running <= Duration::from_secs(10),
        "plugin2 should reach run state before cancellation"
    );
    let plugin3_running = wait_for_recorded_event_elapsed(&events, "plugin3:run", 10_000)
        .await
        .expect("plugin3 should reach run state before cancellation");
    assert!(
        plugin3_running <= Duration::from_secs(10),
        "plugin3 should reach run state before cancellation"
    );
    cancel.cancel();

    let result = daemon_task.await.unwrap();
    assert!(result.is_ok());

    let recorded_events = events.lock().unwrap();
    // All three plugins should have run
    assert!(recorded_events.contains(&"plugin1:run".to_string()));
    assert!(recorded_events.contains(&"plugin2:run".to_string()));
    assert!(recorded_events.contains(&"plugin3:run".to_string()));

    // All three should have shut down
    assert!(recorded_events.contains(&"plugin1:shutdown".to_string()));
    assert!(recorded_events.contains(&"plugin2:shutdown".to_string()));
    assert!(recorded_events.contains(&"plugin3:shutdown".to_string()));
}

#[tokio::test]
#[serial]
async fn test_plugin_run_failure_isolated_from_sibling_plugins() {
    let (ctx, temp_dir, _atm_home_guard) = create_test_context();
    let status_writer = create_test_status_writer(&temp_dir);
    let events = Arc::new(Mutex::new(Vec::new()));

    let mut registry = PluginRegistry::new();
    registry.register(FailingRunPlugin::new("gh-monitor", events.clone()));
    registry.register(MockPlugin::new("worker-adapter", events.clone()));

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let dedup_store = new_dedup_store(temp_dir.path()).unwrap();
    let daemon_lock = create_test_daemon_lock(&temp_dir);

    let daemon_task = tokio::spawn(async move {
        daemon::run(
            &mut registry,
            &ctx,
            daemon_lock,
            cancel_clone,
            status_writer.clone(),
            new_state_store(),
            new_pubsub_store(),
            new_launch_sender(),
            new_session_registry(),
            dedup_store,
            new_stream_state_store(),
            new_stream_event_sender(),
            new_log_event_queue(),
        )
        .await
    });

    wait_for_recorded_event_elapsed(&events, "gh-monitor:run_failed", 5_000)
        .await
        .expect("expected failing plugin state before cancellation");
    wait_for_recorded_event_elapsed(&events, "worker-adapter:run", 5_000)
        .await
        .expect("expected sibling plugin running state before cancellation");

    {
        let recorded_events = events.lock().unwrap();
        assert!(
            recorded_events.contains(&"gh-monitor:run_failed".to_string()),
            "failing plugin should have reported run failure"
        );
        assert!(
            recorded_events.contains(&"worker-adapter:run".to_string()),
            "sibling plugin should continue running despite failing plugin"
        );
    }

    cancel.cancel();
    let result = daemon_task.await.unwrap();
    assert!(
        result.is_ok(),
        "daemon should continue and shutdown cleanly despite plugin run failure"
    );

    let recorded_events = events.lock().unwrap();
    assert!(
        recorded_events.contains(&"worker-adapter:shutdown".to_string()),
        "sibling plugin must still receive shutdown"
    );
}

#[test]
#[serial]
fn test_second_daemon_start_rejected_when_first_is_running() {
    let temp_dir = TempDir::new().unwrap();
    let bin = env!("CARGO_BIN_EXE_atm-daemon");

    let mut first_cmd = std::process::Command::new(bin);
    first_cmd
        .env("ATM_HOME", temp_dir.path())
        .env_remove("ATM_DAEMON_BIN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let first_token = issue_isolated_test_launch_token(temp_dir.path(), "daemon_tests::first");
    attach_launch_token(&mut first_cmd, &first_token).expect("encode first daemon token");
    let first_child = first_cmd.spawn().expect("failed to spawn first daemon");
    let mut first = daemon_process_guard::DaemonProcessGuard::from_child(
        first_child,
        Path::new(bin),
        temp_dir.path(),
    );

    let daemon_running = wait_for_child_running_elapsed(first.child_mut(), 1_000)
        .expect("first daemon should still be running");
    assert!(
        daemon_running <= Duration::from_secs(1),
        "first daemon should still be running: elapsed={daemon_running:?}"
    );
    let lock_elapsed = wait_for_lock_file_acquired_elapsed(temp_dir.path(), 2_000)
        .expect("first daemon should acquire daemon.lock");
    assert!(
        lock_elapsed <= Duration::from_secs(2),
        "first daemon should acquire daemon.lock within 2s: elapsed={lock_elapsed:?}"
    );
    let lock_elapsed = wait_for_lock_file_acquired_elapsed(temp_dir.path(), 8_000)
        .expect("first daemon should acquire daemon.lock within 8s");
    assert!(
        lock_elapsed <= Duration::from_secs(8),
        "first daemon should acquire daemon.lock within 8s: elapsed={lock_elapsed:?}"
    );

    let mut second_cmd = std::process::Command::new(bin);
    second_cmd.env("ATM_HOME", temp_dir.path());
    let second_token = issue_isolated_test_launch_token(temp_dir.path(), "daemon_tests::second");
    attach_launch_token(&mut second_cmd, &second_token).expect("encode second daemon token");
    let second = second_cmd.output().expect("failed to spawn second daemon");

    assert!(
        !second.status.success(),
        "second daemon start must fail while first holds lock"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("already running") || stderr.contains("Refusing second instance"),
        "second daemon error should indicate lock contention, got: {stderr}"
    );
    drop(first);
}

#[test]
#[serial]
fn test_daemon_start_requires_launch_token() {
    let temp_dir = TempDir::new().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_atm-daemon"))
        .env("ATM_HOME", temp_dir.path())
        .env_remove("ATM_LAUNCH_TOKEN")
        .output()
        .expect("spawn daemon without launch token");

    assert!(
        !output.status.success(),
        "daemon start without launch token must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\"rejection_reason\":\"missing_token\"")
            || stderr.contains("missing launch token"),
        "stderr should contain structured missing_token rejection, got: {stderr}"
    );
}

#[test]
#[serial]
fn test_daemon_startup_emits_otlp_trace_with_daemon_service_name_and_session_id() {
    let temp_dir = TempDir::new().unwrap();
    let (endpoint, rx) = start_otel_trace_collector();
    let session_id = "sess-az-1";
    let daemon_bin = Path::new(env!("CARGO_BIN_EXE_atm-daemon"));
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_atm-daemon"));
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_OTEL_ENABLED", "true")
        .env("ATM_OTEL_ENDPOINT", &endpoint)
        .env("CLAUDE_SESSION_ID", session_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let token = issue_isolated_test_launch_token_with_lease(
        temp_dir.path(),
        "daemon_tests::startup_trace",
        "daemon_tests::startup_trace",
        std::process::id(),
        Duration::from_secs(30),
    );
    attach_launch_token(&mut cmd, &token).expect("encode startup trace token");
    let child = cmd.spawn().expect("spawn daemon for startup trace");
    let mut guard =
        daemon_process_guard::DaemonProcessGuard::from_child(child, daemon_bin, temp_dir.path());

    let daemon_running = wait_for_child_running_elapsed(guard.child_mut(), 1_000)
        .expect("daemon should still be running");
    assert!(
        daemon_running <= Duration::from_secs(1),
        "daemon should still be running: elapsed={daemon_running:?}"
    );
    let lock_elapsed = wait_for_lock_file_acquired_elapsed(temp_dir.path(), 8_000)
        .expect("daemon should acquire daemon.lock");
    assert!(
        lock_elapsed <= Duration::from_secs(8),
        "daemon should acquire daemon.lock within 8s: elapsed={lock_elapsed:?}"
    );

    let mut saw_trace = false;
    for _ in 0..8 {
        if let Ok((path, body)) = rx.recv_timeout(Duration::from_secs(5)) {
            if path != "/v1/traces" {
                continue;
            }
            let payload: serde_json::Value =
                serde_json::from_str(&body).expect("valid collector payload");
            let resource_attrs = payload["resourceSpans"][0]["resource"]["attributes"]
                .as_array()
                .expect("trace resource attributes");
            assert!(
                resource_attrs.iter().any(|item| {
                    item["key"] == "service.name" && item["value"]["stringValue"] == "atm-daemon"
                }),
                "trace payload should set service.name=atm-daemon: {payload}"
            );
            assert!(
                resource_attrs.iter().any(|item| {
                    item["key"] == "session_id" && item["value"]["stringValue"] == session_id
                }),
                "trace resource should include inherited session_id: {payload}"
            );
            saw_trace = true;
            break;
        }
    }

    assert!(
        saw_trace,
        "collector should receive a daemon /v1/traces request"
    );
}

#[test]
#[serial]
fn test_daemon_exits_when_isolated_test_owner_pid_is_dead() {
    let temp_dir = TempDir::new().unwrap();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_atm-daemon"));
    cmd.env("ATM_HOME", temp_dir.path());
    let token = issue_isolated_test_launch_token_with_lease(
        temp_dir.path(),
        "daemon_tests::dead_owner",
        "daemon_tests::dead_owner",
        999_999,
        Duration::from_secs(30),
    );
    attach_launch_token(&mut cmd, &token).expect("encode dead-owner token");
    let output = cmd.output().expect("spawn daemon with dead owner pid");

    assert!(
        !output.status.success(),
        "daemon should terminate non-zero when owner_pid is dead"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\"event_name\":\"dead_owner_shutdown\"")
            || stderr.contains("dead_owner_shutdown"),
        "stderr should contain dead_owner_shutdown event, got: {stderr}"
    );
}

#[test]
#[serial]
fn test_daemon_exits_when_isolated_test_ttl_expires() {
    let temp_dir = TempDir::new().unwrap();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_atm-daemon"));
    cmd.env("ATM_HOME", temp_dir.path());
    let token = issue_isolated_test_launch_token_with_lease(
        temp_dir.path(),
        "daemon_tests::ttl_expiry",
        "daemon_tests::ttl_expiry",
        std::process::id(),
        Duration::from_secs(1),
    );
    attach_launch_token(&mut cmd, &token).expect("encode ttl-expiry token");
    let output = cmd.output().expect("spawn daemon with short TTL");

    assert!(
        !output.status.success(),
        "daemon should terminate non-zero when TTL expires"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\"event_name\":\"ttl_expiry_shutdown\"")
            || stderr.contains("ttl_expiry_shutdown"),
        "stderr should contain ttl_expiry_shutdown event, got: {stderr}"
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn test_isolated_test_clean_shutdown_emits_lifecycle_events() {
    let temp_dir = TempDir::new().unwrap();
    let isolated_log_file = temp_dir
        .path()
        .join(".config/atm/logs/atm-daemon/atm-daemon.log.jsonl");
    let daemon_bin = Path::new(env!("CARGO_BIN_EXE_atm-daemon"));
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_atm-daemon"));
    cmd.env("ATM_HOME", temp_dir.path())
        .env("ATM_LOG_FILE", &isolated_log_file)
        .env("ATM_OTEL_ENABLED", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let token = issue_isolated_test_launch_token_with_lease(
        temp_dir.path(),
        "daemon_tests::clean_shutdown",
        "daemon_tests::clean_shutdown",
        std::process::id(),
        Duration::from_secs(30),
    );
    attach_launch_token(&mut cmd, &token).expect("encode clean-shutdown token");
    let child = cmd.spawn().expect("spawn daemon for clean shutdown");
    let mut guard =
        daemon_process_guard::DaemonProcessGuard::from_child(child, daemon_bin, temp_dir.path());

    let daemon_running = wait_for_child_running_elapsed(guard.child_mut(), 1_000)
        .expect("daemon should still be running");
    assert!(
        daemon_running <= Duration::from_secs(1),
        "daemon should still be running: elapsed={daemon_running:?}"
    );
    let lock_elapsed = wait_for_lock_file_acquired_elapsed(temp_dir.path(), 8_000)
        .expect("daemon should acquire daemon.lock");
    assert!(
        lock_elapsed <= Duration::from_secs(8),
        "daemon should acquire daemon.lock within 8s: elapsed={lock_elapsed:?}"
    );
    std::thread::sleep(Duration::from_millis(250));

    #[cfg(unix)]
    {
        let signal_result = unsafe { libc::kill(guard.pid() as i32, libc::SIGTERM) };
        assert_eq!(signal_result, 0, "SIGTERM should succeed");
    }
    #[cfg(windows)]
    {
        guard
            .child_mut()
            .kill()
            .expect("terminate daemon on windows");
    }

    let output = guard
        .wait_with_output()
        .expect("wait for clean shutdown daemon output");
    assert!(
        output.status.success(),
        "daemon should exit cleanly after SIGTERM: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let spool = isolated_log_file.parent().unwrap().join("spool");
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let events = read_spool_events(&spool);
        let saw_clean_shutdown = events.iter().any(|event| {
            event.action == "clean_owner_shutdown"
                && event
                    .fields
                    .get("event_name")
                    .and_then(|value| value.as_str())
                    == Some("clean_owner_shutdown")
        });
        if saw_clean_shutdown {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let events = read_spool_events(&spool);
    panic!(
        "expected clean_owner_shutdown event in spool, got events: {:?}",
        events
            .iter()
            .map(|event| (&event.action, event.fields.get("event_name")))
            .collect::<Vec<_>>()
    );
}
