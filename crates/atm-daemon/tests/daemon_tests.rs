//! Integration tests for daemon event loop

use atm_core::config::Config;
use atm_core::context::SystemContext;
use atm_daemon::daemon;
use atm_daemon::plugin::{
    Capability, MailService, Plugin, PluginContext, PluginError, PluginMetadata, PluginRegistry,
};
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
    unsafe {
        std::env::set_var("ATM_HOME", temp_dir.path());
    }

    let claude_root = temp_dir.path().join(".claude");
    std::fs::create_dir_all(&claude_root).unwrap();

    let system_ctx = SystemContext::new(
        "test-host".to_string(),
        atm_core::context::Platform::detect(),
        claude_root,
        "test-version".to_string(),
        "test-team".to_string(),
    );

    let mail_service = MailService::new(teams_root);
    let config = Config::default();

    let ctx = PluginContext::new(
        Arc::new(system_ctx),
        Arc::new(mail_service),
        Arc::new(config),
    );

    (ctx, temp_dir)
}

#[tokio::test]
async fn test_daemon_starts_and_loads_mock_plugin() {
    let (ctx, _temp_dir) = create_test_context();
    let events = Arc::new(Mutex::new(Vec::new()));

    let mut registry = PluginRegistry::new();
    registry.register(MockPlugin::new("test-plugin", events.clone()));

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    // Run daemon in background, cancel after a short delay
    let daemon_task = tokio::spawn(async move {
        daemon::run(&mut registry, &ctx, cancel_clone).await
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
async fn test_signal_triggers_graceful_shutdown() {
    let (ctx, _temp_dir) = create_test_context();
    let events = Arc::new(Mutex::new(Vec::new()));

    let mut registry = PluginRegistry::new();
    registry.register(MockPlugin::new("plugin1", events.clone()));
    registry.register(MockPlugin::new("plugin2", events.clone()));

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let daemon_task = tokio::spawn(async move {
        daemon::run(&mut registry, &ctx, cancel_clone).await
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
async fn test_plugin_lifecycle_order() {
    let (ctx, _temp_dir) = create_test_context();
    let events = Arc::new(Mutex::new(Vec::new()));

    let mut registry = PluginRegistry::new();
    registry.register(MockPlugin::new("plugin", events.clone()));

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let daemon_task = tokio::spawn(async move {
        daemon::run(&mut registry, &ctx, cancel_clone).await
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
async fn test_spool_drain_runs_on_interval() {
    let (ctx, _temp_dir) = create_test_context();
    let mut registry = PluginRegistry::new();

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let daemon_task = tokio::spawn(async move {
        daemon::run(&mut registry, &ctx, cancel_clone).await
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
async fn test_graceful_shutdown_with_timeout() {
    let (ctx, _temp_dir) = create_test_context();
    let events = Arc::new(Mutex::new(Vec::new()));

    let mut registry = PluginRegistry::new();

    // Create a plugin that takes a long time to shut down
    registry.register(
        MockPlugin::new("slow-shutdown", events.clone())
            .with_shutdown_delay(Duration::from_secs(10)),
    );

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let daemon_task = tokio::spawn(async move {
        daemon::run(&mut registry, &ctx, cancel_clone).await
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
async fn test_empty_registry_runs_successfully() {
    let (ctx, _temp_dir) = create_test_context();
    let mut registry = PluginRegistry::new();

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let daemon_task = tokio::spawn(async move {
        daemon::run(&mut registry, &ctx, cancel_clone).await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();

    let result = daemon_task.await.unwrap();
    assert!(result.is_ok(), "Daemon should run with no plugins");
}

#[tokio::test]
async fn test_multiple_plugins_run_concurrently() {
    let (ctx, _temp_dir) = create_test_context();
    let events = Arc::new(Mutex::new(Vec::new()));

    let mut registry = PluginRegistry::new();
    registry.register(MockPlugin::new("plugin1", events.clone()));
    registry.register(MockPlugin::new("plugin2", events.clone()));
    registry.register(MockPlugin::new("plugin3", events.clone()));

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let daemon_task = tokio::spawn(async move {
        daemon::run(&mut registry, &ctx, cancel_clone).await
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
