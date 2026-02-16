//! Integration tests for bridge sync engine

use atm_core::config::{BridgeConfig, BridgeRole, HostnameRegistry, RemoteConfig};
use atm_core::schema::InboxMessage;
use atm_daemon::plugins::bridge::{BridgePluginConfig, MockTransport, SyncEngine, SyncState, Transport, SelfWriteFilter};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::fs;
use tokio::sync::Mutex as TokioMutex;

/// Helper to create a test message
fn create_test_message(from: &str, text: &str, message_id: Option<String>) -> InboxMessage {
    InboxMessage {
        from: from.to_string(),
        text: text.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        read: false,
        summary: None,
        message_id,
        unknown_fields: HashMap::new(),
    }
}

/// Helper to create test bridge config
fn create_test_config(local_hostname: &str, remote_hostname: &str) -> Arc<BridgePluginConfig> {
    let mut registry = HostnameRegistry::new();
    registry
        .register(RemoteConfig {
            hostname: remote_hostname.to_string(),
            address: format!("user@{remote_hostname}"),
            ssh_key_path: None,
            aliases: Vec::new(),
        })
        .unwrap();

    Arc::new(BridgePluginConfig {
        core: BridgeConfig {
            enabled: true,
            local_hostname: Some(local_hostname.to_string()),
            role: BridgeRole::Spoke,
            sync_interval_secs: 60,
            remotes: vec![RemoteConfig {
                hostname: remote_hostname.to_string(),
                address: format!("user@{remote_hostname}"),
                ssh_key_path: None,
                aliases: Vec::new(),
            }],
        },
        registry,
        local_hostname: local_hostname.to_string(),
    })
}

fn new_filter() -> Arc<TokioMutex<SelfWriteFilter>> {
    Arc::new(TokioMutex::new(SelfWriteFilter::default()))
}

#[tokio::test]
async fn test_sync_state_persistence() {
    let temp_dir = TempDir::new().unwrap();
    // Note: ATM_HOME env var not needed for bridge tests (team_dir passed explicitly)

    let state_path = temp_dir.path().join(".bridge-state.json");

    // Create and save state
    let mut state = SyncState::new();
    state.set_cursor(PathBuf::from("inboxes/agent-1.json"), 5);
    state.mark_synced("msg-001".to_string());
    state.save(&state_path).await.unwrap();

    // Load state
    let loaded = SyncState::load(&state_path).await.unwrap();
    assert_eq!(loaded.get_cursor(&PathBuf::from("inboxes/agent-1.json")), 5);
    assert!(loaded.is_synced("msg-001"));
}

#[tokio::test]
async fn test_sync_push_with_mock_transport() {
    let temp_dir = TempDir::new().unwrap();
    // Note: ATM_HOME env var not needed for bridge tests (team_dir passed explicitly)

    let team_dir = temp_dir.path().join("my-team");
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).await.unwrap();

    // Create local inbox with messages
    let inbox_path = inboxes_dir.join("agent-1.json");
    let messages = vec![
        create_test_message("user-a", "Message 1", Some("msg-001".to_string())),
        create_test_message("user-b", "Message 2", Some("msg-002".to_string())),
    ];
    let json = serde_json::to_string_pretty(&messages).unwrap();
    fs::write(&inbox_path, json).await.unwrap();

    // Setup sync engine with mock transport
    let config = create_test_config("laptop", "desktop");

    // Connect transport (required before operations)
    let mut transport_mut = MockTransport::new();
    transport_mut.connect().await.unwrap();
    let transport = Arc::new(tokio::sync::Mutex::new(transport_mut)) as Arc<tokio::sync::Mutex<dyn atm_daemon::plugins::bridge::Transport>>;

    let mut transports = HashMap::new();
    transports.insert("desktop".to_string(), transport);
    let mut engine = SyncEngine::new(config, transports, team_dir.clone(), new_filter())
        .await
        .unwrap();

    // Push messages
    let stats = engine.sync_push().await.unwrap();

    // Verify stats
    assert_eq!(stats.messages_pushed, 2);
    assert_eq!(stats.errors, 0);

    // Verify cursor advanced (per-remote cursor key)
    assert_eq!(
        engine.state().get_cursor(&PathBuf::from("inboxes/agent-1.json:desktop")),
        2
    );

    // Verify message_ids marked as synced
    assert!(engine.state().is_synced("msg-001"));
    assert!(engine.state().is_synced("msg-002"));
}

#[tokio::test]
async fn test_sync_push_dedup() {
    let temp_dir = TempDir::new().unwrap();
    // Note: ATM_HOME env var not needed for bridge tests (team_dir passed explicitly)

    let team_dir = temp_dir.path().join("my-team");
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).await.unwrap();

    // Create local inbox
    let inbox_path = inboxes_dir.join("agent-1.json");
    let messages = vec![
        create_test_message("user-a", "Message 1", Some("msg-001".to_string())),
    ];
    let json = serde_json::to_string_pretty(&messages).unwrap();
    fs::write(&inbox_path, json).await.unwrap();

    // Setup sync engine
    let config = create_test_config("laptop", "desktop");
    let mut transport_mut = MockTransport::new();
    transport_mut.connect().await.unwrap();
    let transport = Arc::new(tokio::sync::Mutex::new(transport_mut)) as Arc<tokio::sync::Mutex<dyn atm_daemon::plugins::bridge::Transport>>;

    let mut transports = HashMap::new();
    transports.insert("desktop".to_string(), transport);
    let mut engine = SyncEngine::new(config, transports, team_dir.clone(), new_filter())
        .await
        .unwrap();

    // First push
    let stats1 = engine.sync_push().await.unwrap();
    assert_eq!(stats1.messages_pushed, 1);

    // Second push (should not push again - already synced)
    let stats2 = engine.sync_push().await.unwrap();
    assert_eq!(stats2.messages_pushed, 0);
}

#[tokio::test]
async fn test_sync_push_assigns_message_ids() {
    let temp_dir = TempDir::new().unwrap();
    // Note: ATM_HOME env var not needed for bridge tests (team_dir passed explicitly)

    let team_dir = temp_dir.path().join("my-team");
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).await.unwrap();

    // Create local inbox with messages WITHOUT message_ids
    let inbox_path = inboxes_dir.join("agent-1.json");
    let messages = vec![
        create_test_message("user-a", "Message 1", None),
        create_test_message("user-b", "Message 2", None),
    ];
    let json = serde_json::to_string_pretty(&messages).unwrap();
    fs::write(&inbox_path, json).await.unwrap();

    // Setup sync engine
    let config = create_test_config("laptop", "desktop");
    let mut transport_mut = MockTransport::new();
    transport_mut.connect().await.unwrap();
    let transport = Arc::new(tokio::sync::Mutex::new(transport_mut)) as Arc<tokio::sync::Mutex<dyn atm_daemon::plugins::bridge::Transport>>;

    let mut transports = HashMap::new();
    transports.insert("desktop".to_string(), transport);
    let mut engine = SyncEngine::new(config, transports, team_dir.clone(), new_filter())
        .await
        .unwrap();

    // Push messages
    let stats = engine.sync_push().await.unwrap();
    assert_eq!(stats.messages_pushed, 2);

    // Verify message_ids were assigned (tracked in sync state)
    assert_eq!(engine.state().synced_message_ids.len(), 2);
}

#[tokio::test]
async fn test_sync_engine_empty_inbox() {
    let temp_dir = TempDir::new().unwrap();
    // Note: ATM_HOME env var not needed for bridge tests (team_dir passed explicitly)

    let team_dir = temp_dir.path().join("my-team");
    let config = create_test_config("laptop", "desktop");

    let mut transport_mut = MockTransport::new();
    transport_mut.connect().await.unwrap();
    let transport = Arc::new(tokio::sync::Mutex::new(transport_mut)) as Arc<tokio::sync::Mutex<dyn atm_daemon::plugins::bridge::Transport>>;

    let mut transports = HashMap::new();
    transports.insert("desktop".to_string(), transport);
    let mut engine = SyncEngine::new(config, transports, team_dir, new_filter())
        .await
        .unwrap();

    // Push with no inboxes directory
    let stats = engine.sync_push().await.unwrap();
    assert_eq!(stats.messages_pushed, 0);
    assert_eq!(stats.errors, 0);
}

#[tokio::test]
async fn test_sync_cycle() {
    let temp_dir = TempDir::new().unwrap();
    // Note: ATM_HOME env var not needed for bridge tests (team_dir passed explicitly)

    let team_dir = temp_dir.path().join("my-team");
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).await.unwrap();

    // Create local inbox
    let inbox_path = inboxes_dir.join("agent-1.json");
    let messages = vec![
        create_test_message("user-a", "Message 1", Some("msg-001".to_string())),
    ];
    let json = serde_json::to_string_pretty(&messages).unwrap();
    fs::write(&inbox_path, json).await.unwrap();

    // Setup sync engine
    let config = create_test_config("laptop", "desktop");
    let mut transport_mut = MockTransport::new();
    transport_mut.connect().await.unwrap();
    let transport = Arc::new(tokio::sync::Mutex::new(transport_mut)) as Arc<tokio::sync::Mutex<dyn atm_daemon::plugins::bridge::Transport>>;

    let mut transports = HashMap::new();
    transports.insert("desktop".to_string(), transport);
    let mut engine = SyncEngine::new(config, transports, team_dir, new_filter())
        .await
        .unwrap();

    // Run sync cycle (push + pull)
    let stats = engine.sync_cycle().await.unwrap();

    // Should have pushed 1 message
    assert_eq!(stats.messages_pushed, 1);

    // Note: With mock transport, push writes to mock "remote" which then gets pulled back
    // Pull will download the file we just pushed (agent-1.laptop.json from remote becomes agent-1.desktop.json locally)
    // This is expected behavior - testing the full round trip
}

#[tokio::test]
async fn test_sync_cursor_advancement() {
    let temp_dir = TempDir::new().unwrap();
    // Note: ATM_HOME env var not needed for bridge tests (team_dir passed explicitly)

    let team_dir = temp_dir.path().join("my-team");
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).await.unwrap();

    // Create local inbox with 2 messages
    let inbox_path = inboxes_dir.join("agent-1.json");
    let messages = vec![
        create_test_message("user-a", "Message 1", Some("msg-001".to_string())),
        create_test_message("user-b", "Message 2", Some("msg-002".to_string())),
    ];
    let json = serde_json::to_string_pretty(&messages).unwrap();
    fs::write(&inbox_path, json).await.unwrap();

    // Setup sync engine
    let config = create_test_config("laptop", "desktop");
    let mut transport_mut = MockTransport::new();
    transport_mut.connect().await.unwrap();
    let transport = Arc::new(tokio::sync::Mutex::new(transport_mut)) as Arc<tokio::sync::Mutex<dyn atm_daemon::plugins::bridge::Transport>>;

    let mut transports = HashMap::new();
    transports.insert("desktop".to_string(), transport);
    let mut engine = SyncEngine::new(config, transports, team_dir.clone(), new_filter())
        .await
        .unwrap();

    // First push - should sync both messages
    let stats1 = engine.sync_push().await.unwrap();
    assert_eq!(stats1.messages_pushed, 2);

    // Append a third message to local inbox
    let mut all_messages = messages;
    all_messages.push(create_test_message("user-c", "Message 3", Some("msg-003".to_string())));
    let json = serde_json::to_string_pretty(&all_messages).unwrap();
    fs::write(&inbox_path, json).await.unwrap();

    // Second push - should only sync the new message
    let stats2 = engine.sync_push().await.unwrap();
    assert_eq!(stats2.messages_pushed, 1);

    // Verify cursor advanced to 3 (per-remote cursor key)
    assert_eq!(
        engine.state().get_cursor(&PathBuf::from("inboxes/agent-1.json:desktop")),
        3
    );
}
