//! Integration tests for daemon event loop

use agent_team_mail_core::config::Config;
use agent_team_mail_core::context::SystemContext;
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
use serial_test::serial;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

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

/// Create a test plugin context with temporary directories
fn create_test_context() -> (PluginContext, TempDir) {
    let temp_dir = tempfile::tempdir().unwrap();
    let teams_root = temp_dir.path().join("teams");
    std::fs::create_dir_all(&teams_root).unwrap();

    // Set ATM_HOME for cross-platform testing
    // SAFETY: Tests are serialized via #[serial], so no parallel mutation
    unsafe {
        std::env::set_var("ATM_HOME", temp_dir.path());
    }

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

    (ctx, temp_dir)
}

/// Create a test context where mail teams root matches `${ATM_HOME}/.claude/teams`.
fn create_reconcile_test_context() -> (PluginContext, TempDir) {
    let temp_dir = tempfile::tempdir().unwrap();

    // SAFETY: Tests are serialized via #[serial], so no parallel mutation
    unsafe {
        std::env::set_var("ATM_HOME", temp_dir.path());
    }

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

    (ctx, temp_dir)
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

async fn wait_until(timeout_ms: u64, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if pred() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    pred()
}

/// Create a test status writer
fn create_test_status_writer(temp_dir: &TempDir) -> Arc<StatusWriter> {
    Arc::new(StatusWriter::new(
        temp_dir.path().to_path_buf(),
        "test-version".to_string(),
    ))
}

fn create_test_daemon_lock(temp_dir: &TempDir) -> agent_team_mail_core::io::lock::FileLock {
    let lock_path = temp_dir.path().join(".config/atm/daemon.lock");
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    agent_team_mail_core::io::lock::acquire_lock(&lock_path, 0).unwrap()
}

#[tokio::test]
#[serial]
async fn test_daemon_starts_and_loads_mock_plugin() {
    let (ctx, temp_dir) = create_test_context();
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

    // Wait a bit for daemon to start
    tokio::time::sleep(Duration::from_millis(100)).await;

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
    let (ctx, temp_dir) = create_test_context();
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

    tokio::time::sleep(Duration::from_millis(100)).await;

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
    let (ctx, temp_dir) = create_test_context();
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

    tokio::time::sleep(Duration::from_millis(100)).await;
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
    let (ctx, temp_dir) = create_test_context();
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

    // Let the daemon run for a bit to allow spool drain to run
    tokio::time::sleep(Duration::from_millis(500)).await;

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
    let (ctx, temp_dir) = create_reconcile_test_context();
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
                "cwd": "/tmp",
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

    let seeded = wait_until(1000, || {
        state_store_probe
            .lock()
            .unwrap()
            .get_state("worker")
            .is_some()
    })
    .await;
    assert!(
        seeded,
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
    let (ctx, temp_dir) = create_reconcile_test_context();
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

    let initial_seeded = wait_until(1500, || {
        state_store_probe
            .lock()
            .unwrap()
            .get_state("worker-a")
            .is_some()
    })
    .await;
    assert!(
        initial_seeded,
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
                "cwd": "/tmp",
                "subscriptions": [],
                "isActive": true
            }
        ]),
    );

    let added = wait_until(8000, || {
        state_store_probe
            .lock()
            .unwrap()
            .get_state("worker-b")
            .is_some()
    })
    .await;
    assert!(
        added,
        "worker-b should be added via live config watcher reconcile"
    );

    let removed = wait_until(8000, || {
        state_store_probe
            .lock()
            .unwrap()
            .get_state("worker-a")
            .is_none()
    })
    .await;
    assert!(
        removed,
        "worker-a should be removed from tracked state after config update"
    );

    cancel.cancel();
    daemon_task.await.unwrap().unwrap();
}

#[tokio::test]
#[serial]
async fn test_graceful_shutdown_with_timeout() {
    let (ctx, temp_dir) = create_test_context();
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

    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();

    // The daemon should complete even though the plugin shutdown is slow
    // (the shutdown timeout will kick in)
    let result = tokio::time::timeout(Duration::from_secs(10), daemon_task)
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
    let (ctx, temp_dir) = create_test_context();
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

    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();

    let result = daemon_task.await.unwrap();
    assert!(result.is_ok(), "Daemon should run with no plugins");
}

#[tokio::test]
#[serial]
async fn test_multiple_plugins_run_concurrently() {
    let (ctx, temp_dir) = create_test_context();
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

    tokio::time::sleep(Duration::from_millis(100)).await;
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
    let (ctx, temp_dir) = create_test_context();
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

    tokio::time::sleep(Duration::from_millis(150)).await;

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

    let mut first = std::process::Command::new(bin)
        .env("ATM_HOME", temp_dir.path())
        .spawn()
        .expect("failed to spawn first daemon");

    // Give the first daemon a brief moment to acquire lock and bind socket.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        first
            .try_wait()
            .expect("failed to poll first daemon")
            .is_none(),
        "first daemon should still be running"
    );

    let second = std::process::Command::new(bin)
        .env("ATM_HOME", temp_dir.path())
        .output()
        .expect("failed to spawn second daemon");

    assert!(
        !second.status.success(),
        "second daemon start must fail while first holds lock"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("already running") || stderr.contains("Refusing second instance"),
        "second daemon error should indicate lock contention, got: {stderr}"
    );

    let _ = first.kill();
    let _ = first.wait();
}
