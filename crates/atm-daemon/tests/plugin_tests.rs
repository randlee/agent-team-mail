use atm_core::config::Config;
use atm_core::context::SystemContext;
use atm_core::schema::InboxMessage;
use atm_daemon::plugin::{
    Capability, MailService, Plugin, PluginContext, PluginError, PluginMetadata, PluginRegistry,
    PluginState,
};
use atm_daemon::roster::RosterService;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

// ============================================================================
// Mock/Test Plugins
// ============================================================================

/// A simple test plugin that does nothing
struct MockPlugin {
    name: &'static str,
    capabilities: Vec<Capability>,
    init_count: usize,
    shutdown_count: usize,
}

impl MockPlugin {
    fn new(name: &'static str, capabilities: Vec<Capability>) -> Self {
        Self {
            name,
            capabilities,
            init_count: 0,
            shutdown_count: 0,
        }
    }
}

impl Plugin for MockPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: self.name,
            version: "1.0.0",
            description: "Mock plugin for testing",
            capabilities: self.capabilities.clone(),
        }
    }

    async fn init(&mut self, _ctx: &PluginContext) -> Result<(), PluginError> {
        self.init_count += 1;
        Ok(())
    }

    async fn run(&mut self, cancel: CancellationToken) -> Result<(), PluginError> {
        // Wait for cancellation
        cancel.cancelled().await;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), PluginError> {
        self.shutdown_count += 1;
        Ok(())
    }
}

/// Echo plugin that stores received messages
struct EchoPlugin {
    received_messages: Vec<InboxMessage>,
}

impl EchoPlugin {
    fn new() -> Self {
        Self {
            received_messages: Vec::new(),
        }
    }
}

impl Plugin for EchoPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "echo",
            version: "1.0.0",
            description: "Echo plugin for testing message handling",
            capabilities: vec![Capability::Custom("echo".to_string())],
        }
    }

    async fn init(&mut self, _ctx: &PluginContext) -> Result<(), PluginError> {
        Ok(())
    }

    async fn run(&mut self, cancel: CancellationToken) -> Result<(), PluginError> {
        cancel.cancelled().await;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), PluginError> {
        Ok(())
    }

    async fn handle_message(&mut self, msg: &InboxMessage) -> Result<(), PluginError> {
        self.received_messages.push(msg.clone());
        Ok(())
    }
}

// ============================================================================
// Test Helpers
// ============================================================================

fn create_test_context(teams_root: std::path::PathBuf) -> PluginContext {
    let system = Arc::new(SystemContext::new(
        "test-host".to_string(),
        atm_core::context::Platform::Linux,
        std::path::PathBuf::from("/tmp/.claude"),
        "2.1.39".to_string(),
        "test-team".to_string(),
    ));
    let mail = Arc::new(MailService::new(teams_root.clone()));
    let config = Arc::new(Config::default());
    let roster = Arc::new(RosterService::new(teams_root));
    PluginContext::new(system, mail, config, roster)
}

fn create_test_message(from: &str, text: &str) -> InboxMessage {
    InboxMessage {
        from: from.to_string(),
        text: text.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        read: false,
        summary: None,
        message_id: Some(uuid::Uuid::new_v4().to_string()),
        unknown_fields: HashMap::new(),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
async fn test_mock_plugin_implementation() {
    let temp_dir = TempDir::new().unwrap();
    let ctx = create_test_context(temp_dir.path().to_path_buf());

    let mut plugin = MockPlugin::new("test-plugin", vec![Capability::IssueTracking]);

    // Test metadata
    let metadata = plugin.metadata();
    assert_eq!(metadata.name, "test-plugin");
    assert_eq!(metadata.version, "1.0.0");
    assert_eq!(metadata.capabilities, vec![Capability::IssueTracking]);

    // Test init
    assert_eq!(plugin.init_count, 0);
    plugin.init(&ctx).await.unwrap();
    assert_eq!(plugin.init_count, 1);

    // Test shutdown
    assert_eq!(plugin.shutdown_count, 0);
    plugin.shutdown().await.unwrap();
    assert_eq!(plugin.shutdown_count, 1);
}

#[tokio::test]
async fn test_registry_register_and_init() {
    let temp_dir = TempDir::new().unwrap();
    let ctx = create_test_context(temp_dir.path().to_path_buf());

    let mut registry = PluginRegistry::new();
    assert_eq!(registry.len(), 0);
    assert!(registry.is_empty());

    // Register a plugin
    let plugin = MockPlugin::new("test-plugin", vec![Capability::IssueTracking]);
    registry.register(plugin);

    assert_eq!(registry.len(), 1);
    assert!(!registry.is_empty());

    // Check initial state
    let state = registry.state_of("test-plugin");
    assert_eq!(state, Some(PluginState::Created));

    // Initialize all plugins
    registry.init_all(&ctx).await.unwrap();

    // Check state after init
    let state = registry.state_of("test-plugin");
    assert_eq!(state, Some(PluginState::Initialized));
}

#[tokio::test]
async fn test_registry_capability_lookup() {
    let temp_dir = TempDir::new().unwrap();
    let ctx = create_test_context(temp_dir.path().to_path_buf());

    let mut registry = PluginRegistry::new();

    // Register plugins with different capabilities
    registry.register(MockPlugin::new(
        "issue-plugin",
        vec![Capability::IssueTracking],
    ));
    registry.register(MockPlugin::new("ci-plugin", vec![Capability::CiMonitor]));
    registry.register(MockPlugin::new(
        "multi-plugin",
        vec![Capability::IssueTracking, Capability::Bridge],
    ));

    registry.init_all(&ctx).await.unwrap();

    // Test capability lookup
    let issue_plugins = registry.get_by_capability(&Capability::IssueTracking);
    assert_eq!(issue_plugins.len(), 2);
    let names: Vec<_> = issue_plugins.iter().map(|(m, _)| m.name).collect();
    assert!(names.contains(&"issue-plugin"));
    assert!(names.contains(&"multi-plugin"));

    let ci_plugins = registry.get_by_capability(&Capability::CiMonitor);
    assert_eq!(ci_plugins.len(), 1);
    assert_eq!(ci_plugins[0].0.name, "ci-plugin");

    let bridge_plugins = registry.get_by_capability(&Capability::Bridge);
    assert_eq!(bridge_plugins.len(), 1);
    assert_eq!(bridge_plugins[0].0.name, "multi-plugin");

    // Test non-existent capability
    let chat_plugins = registry.get_by_capability(&Capability::Chat);
    assert_eq!(chat_plugins.len(), 0);
}

#[tokio::test]
async fn test_registry_name_lookup() {
    let temp_dir = TempDir::new().unwrap();
    let ctx = create_test_context(temp_dir.path().to_path_buf());

    let mut registry = PluginRegistry::new();

    registry.register(MockPlugin::new(
        "my-plugin",
        vec![Capability::IssueTracking],
    ));

    registry.init_all(&ctx).await.unwrap();

    // Test name lookup
    let result = registry.get_by_name("my-plugin");
    assert!(result.is_some());
    let (metadata, state) = result.unwrap();
    assert_eq!(metadata.name, "my-plugin");
    assert_eq!(state, PluginState::Initialized);

    // Test non-existent name
    let result = registry.get_by_name("nonexistent");
    assert!(result.is_none());
}

#[tokio::test]
async fn test_mail_service_send_and_read() {
    let temp_dir = TempDir::new().unwrap();
    let mail_service = MailService::new(temp_dir.path().to_path_buf());

    let message = create_test_message("team-lead", "Test message for plugin");

    // Send a message
    let outcome = mail_service.send("test-team", "test-agent", &message);
    assert!(outcome.is_ok());

    // Read the inbox
    let messages = mail_service
        .read_inbox("test-team", "test-agent")
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].from, "team-lead");
    assert_eq!(messages[0].text, "Test message for plugin");
}

#[tokio::test]
async fn test_mail_service_read_empty_inbox() {
    let temp_dir = TempDir::new().unwrap();
    let mail_service = MailService::new(temp_dir.path().to_path_buf());

    // Read from non-existent inbox should return empty vec
    let messages = mail_service
        .read_inbox("test-team", "nonexistent-agent")
        .unwrap();
    assert_eq!(messages.len(), 0);
}

#[tokio::test]
async fn test_plugin_context_provides_working_mail_service() {
    let temp_dir = TempDir::new().unwrap();
    let ctx = create_test_context(temp_dir.path().to_path_buf());

    let message = create_test_message("human", "Context test message");

    // Use MailService through context
    let outcome = ctx.mail.send("test-team", "test-agent", &message);
    assert!(outcome.is_ok());

    // Verify message was written
    let messages = ctx.mail.read_inbox("test-team", "test-agent").unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].from, "human");
    assert_eq!(messages[0].text, "Context test message");
}

#[tokio::test]
async fn test_multiple_plugin_registration() {
    let temp_dir = TempDir::new().unwrap();
    let ctx = create_test_context(temp_dir.path().to_path_buf());

    let mut registry = PluginRegistry::new();

    // Register multiple plugins
    registry.register(MockPlugin::new("plugin-1", vec![Capability::IssueTracking]));
    registry.register(MockPlugin::new("plugin-2", vec![Capability::CiMonitor]));
    registry.register(MockPlugin::new("plugin-3", vec![Capability::Bridge]));
    registry.register(MockPlugin::new("plugin-4", vec![Capability::Chat]));

    assert_eq!(registry.len(), 4);

    // Init all
    registry.init_all(&ctx).await.unwrap();

    // Verify all are initialized
    assert_eq!(
        registry.state_of("plugin-1"),
        Some(PluginState::Initialized)
    );
    assert_eq!(
        registry.state_of("plugin-2"),
        Some(PluginState::Initialized)
    );
    assert_eq!(
        registry.state_of("plugin-3"),
        Some(PluginState::Initialized)
    );
    assert_eq!(
        registry.state_of("plugin-4"),
        Some(PluginState::Initialized)
    );
}

#[tokio::test]
async fn test_handle_message_default_impl() {
    let temp_dir = TempDir::new().unwrap();
    let ctx = create_test_context(temp_dir.path().to_path_buf());

    let mut plugin = MockPlugin::new("test-plugin", vec![Capability::IssueTracking]);
    plugin.init(&ctx).await.unwrap();

    let message = create_test_message("sender", "test message");

    // Default handle_message should return Ok(())
    let result = plugin.handle_message(&message).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_echo_plugin_handles_messages() {
    let temp_dir = TempDir::new().unwrap();
    let ctx = create_test_context(temp_dir.path().to_path_buf());

    let mut plugin = EchoPlugin::new();
    plugin.init(&ctx).await.unwrap();

    assert_eq!(plugin.received_messages.len(), 0);

    // Send messages to plugin
    let msg1 = create_test_message("sender-1", "message 1");
    let msg2 = create_test_message("sender-2", "message 2");

    plugin.handle_message(&msg1).await.unwrap();
    plugin.handle_message(&msg2).await.unwrap();

    assert_eq!(plugin.received_messages.len(), 2);
    assert_eq!(plugin.received_messages[0].text, "message 1");
    assert_eq!(plugin.received_messages[1].text, "message 2");
}

#[tokio::test]
async fn test_plugin_run_respects_cancellation() {
    let temp_dir = TempDir::new().unwrap();
    let ctx = create_test_context(temp_dir.path().to_path_buf());

    let mut plugin = MockPlugin::new("test-plugin", vec![Capability::IssueTracking]);
    plugin.init(&ctx).await.unwrap();

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    // Spawn run task
    let run_handle = tokio::spawn(async move { plugin.run(cancel_clone).await });

    // Give it a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Cancel and verify it completes
    cancel.cancel();

    let result = tokio::time::timeout(tokio::time::Duration::from_secs(1), run_handle).await;
    assert!(result.is_ok());
    assert!(result.unwrap().unwrap().is_ok());
}
