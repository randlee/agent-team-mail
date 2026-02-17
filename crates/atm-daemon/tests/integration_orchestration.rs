//! Sprint 10.7 — Integration Orchestration Tests
//!
//! End-to-end tests validating the Phase 10 stack:
//! - Agent state tracker lifecycle
//! - Unix socket server query
//! - Pub/sub subscription round-trip via socket
//! - Alias config parsing and resolution

use agent_team_mail_core::config::aliases::resolve_alias;
use agent_team_mail_core::config::Config;
use agent_team_mail_daemon::daemon::socket::{
    new_launch_sender, new_pubsub_store, new_state_store, start_socket_server,
};
use agent_team_mail_daemon::plugins::worker_adapter::{AgentState, AgentStateTracker, PubSub};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

// ── Test 1: AgentStateTracker lifecycle ───────────────────────────────────────

#[test]
fn test_state_tracker_lifecycle() {
    let mut tracker = AgentStateTracker::new();

    // Initially no agents tracked
    assert!(tracker.get_state("arch-ctm").is_none());

    // Register and verify default state
    tracker.register_agent("arch-ctm");
    // State after registration is implementation-defined; we just verify it's tracked
    assert!(tracker.get_state("arch-ctm").is_some());

    // Transition through valid states
    tracker.set_state("arch-ctm", AgentState::Launching);
    assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Launching));

    tracker.set_state("arch-ctm", AgentState::Busy);
    assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Busy));

    tracker.set_state("arch-ctm", AgentState::Idle);
    assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Idle));

    tracker.set_state("arch-ctm", AgentState::Killed);
    assert_eq!(tracker.get_state("arch-ctm"), Some(AgentState::Killed));
}

#[test]
fn test_state_tracker_multiple_agents() {
    let mut tracker = AgentStateTracker::new();

    tracker.register_agent("agent-a");
    tracker.register_agent("agent-b");
    tracker.register_agent("agent-c");

    tracker.set_state("agent-a", AgentState::Idle);
    tracker.set_state("agent-b", AgentState::Busy);
    tracker.set_state("agent-c", AgentState::Launching);

    let states = tracker.all_states();
    assert_eq!(states.len(), 3);

    assert_eq!(tracker.get_state("agent-a"), Some(AgentState::Idle));
    assert_eq!(tracker.get_state("agent-b"), Some(AgentState::Busy));
    assert_eq!(tracker.get_state("agent-c"), Some(AgentState::Launching));
}

#[test]
fn test_state_tracker_transition_time() {
    let mut tracker = AgentStateTracker::new();
    tracker.register_agent("arch-ctm");
    tracker.set_state("arch-ctm", AgentState::Idle);

    // After a state transition, time_since_transition should be available
    let elapsed = tracker.time_since_transition("arch-ctm");
    assert!(elapsed.is_some(), "Elapsed time should be available after state set");

    let duration = elapsed.unwrap();
    // Should be very recent (< 1 second)
    assert!(duration.as_secs() < 1, "Transition should have just happened");
}

// ── Test 2: Socket server query for agent state ────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn test_socket_query_agent_state() {
    use agent_team_mail_core::daemon_client::{SocketRequest, PROTOCOL_VERSION};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio_util::sync::CancellationToken;

    let temp_dir = TempDir::new().unwrap();
    let home_dir = temp_dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    // Register agent in the shared state store
    let state_store = new_state_store();
    {
        let mut tracker = state_store.lock().unwrap();
        tracker.register_agent("arch-ctm");
        tracker.set_state("arch-ctm", AgentState::Idle);
    }

    // Start socket server
    let _handle = start_socket_server(
        home_dir.clone(),
        state_store,
        new_pubsub_store(),
        new_launch_sender(),
        cancel.clone(),
    )
    .await
    .unwrap()
    .expect("Expected socket server handle on unix");

    // Connect and query
    let socket_path = home_dir.join(".claude/daemon/atm-daemon.sock");
    let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();

    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: "orch-test-1".to_string(),
        command: "agent-state".to_string(),
        payload: serde_json::json!({"agent": "arch-ctm", "team": "atm-dev"}),
    };
    let req_line = format!("{}\n", serde_json::to_string(&request).unwrap());

    let mut reader = BufReader::new(stream);
    reader.get_mut().write_all(req_line.as_bytes()).await.unwrap();

    let mut resp_line = String::new();
    reader.read_line(&mut resp_line).await.unwrap();

    let resp: agent_team_mail_core::daemon_client::SocketResponse =
        serde_json::from_str(resp_line.trim()).unwrap();

    assert!(resp.is_ok(), "Expected ok response, got: {:?}", resp.error);
    let payload = resp.payload.unwrap();
    assert_eq!(payload["state"].as_str().unwrap(), "idle");

    cancel.cancel();
}

#[cfg(unix)]
#[tokio::test]
async fn test_socket_query_agent_not_found() {
    use agent_team_mail_core::daemon_client::{SocketRequest, PROTOCOL_VERSION};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio_util::sync::CancellationToken;

    let temp_dir = TempDir::new().unwrap();
    let home_dir = temp_dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    let _handle = start_socket_server(
        home_dir.clone(),
        new_state_store(),
        new_pubsub_store(),
        new_launch_sender(),
        cancel.clone(),
    )
    .await
    .unwrap()
    .expect("Socket server handle");

    let socket_path = home_dir.join(".claude/daemon/atm-daemon.sock");
    let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();

    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: "orch-test-not-found".to_string(),
        command: "agent-state".to_string(),
        payload: serde_json::json!({"agent": "ghost-agent", "team": "atm-dev"}),
    };
    let req_line = format!("{}\n", serde_json::to_string(&request).unwrap());

    let mut reader = BufReader::new(stream);
    reader.get_mut().write_all(req_line.as_bytes()).await.unwrap();

    let mut resp_line = String::new();
    reader.read_line(&mut resp_line).await.unwrap();

    let resp: agent_team_mail_core::daemon_client::SocketResponse =
        serde_json::from_str(resp_line.trim()).unwrap();

    assert!(!resp.is_ok(), "Expected error for untracked agent");
    assert_eq!(resp.error.unwrap().code, "AGENT_NOT_FOUND");

    cancel.cancel();
}

// ── Test 3: PubSub subscription round-trip via socket ─────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn test_pubsub_subscription_roundtrip() {
    use agent_team_mail_core::daemon_client::{SocketRequest, PROTOCOL_VERSION};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio_util::sync::CancellationToken;

    let temp_dir = TempDir::new().unwrap();
    let home_dir = temp_dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    let pubsub_store = new_pubsub_store();

    let _handle = start_socket_server(
        home_dir.clone(),
        new_state_store(),
        pubsub_store.clone(),
        new_launch_sender(),
        cancel.clone(),
    )
    .await
    .unwrap()
    .expect("Socket server handle");

    let socket_path = home_dir.join(".claude/daemon/atm-daemon.sock");

    // Send subscribe request
    let sub_request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: "sub-orch-1".to_string(),
        command: "subscribe".to_string(),
        payload: serde_json::json!({
            "subscriber": "team-lead",
            "agent": "arch-ctm",
            "events": ["idle"],
            "team": "atm-dev"
        }),
    };
    let req_line = format!("{}\n", serde_json::to_string(&sub_request).unwrap());

    let stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);
    reader.get_mut().write_all(req_line.as_bytes()).await.unwrap();

    let mut resp_line = String::new();
    reader.read_line(&mut resp_line).await.unwrap();

    let resp: agent_team_mail_core::daemon_client::SocketResponse =
        serde_json::from_str(resp_line.trim()).unwrap();

    assert!(resp.is_ok(), "Subscribe should succeed, got: {:?}", resp.error);
    let payload = resp.payload.unwrap();
    assert!(payload["subscribed"].as_bool().unwrap());

    // Verify subscription is registered in the store
    let subscribers = pubsub_store
        .lock()
        .unwrap()
        .matching_subscribers("arch-ctm", "idle");
    assert!(
        subscribers.contains(&"team-lead".to_string()),
        "team-lead should be subscribed"
    );

    // Now unsubscribe via socket
    let unsub_request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: "unsub-orch-1".to_string(),
        command: "unsubscribe".to_string(),
        payload: serde_json::json!({
            "subscriber": "team-lead",
            "agent": "arch-ctm",
            "team": "atm-dev"
        }),
    };
    let unsub_line = format!("{}\n", serde_json::to_string(&unsub_request).unwrap());

    let stream2 = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
    let mut reader2 = BufReader::new(stream2);
    reader2.get_mut().write_all(unsub_line.as_bytes()).await.unwrap();

    let mut resp2_line = String::new();
    reader2.read_line(&mut resp2_line).await.unwrap();

    let resp2: agent_team_mail_core::daemon_client::SocketResponse =
        serde_json::from_str(resp2_line.trim()).unwrap();

    assert!(resp2.is_ok(), "Unsubscribe should succeed");

    // Verify subscription is removed
    let subscribers_after = pubsub_store
        .lock()
        .unwrap()
        .matching_subscribers("arch-ctm", "idle");
    assert!(
        !subscribers_after.contains(&"team-lead".to_string()),
        "team-lead should no longer be subscribed"
    );

    cancel.cancel();
}

// ── Test 4: Alias config parsing ──────────────────────────────────────────────

#[test]
fn test_alias_resolution_config() {
    // Parse a config with [aliases] section
    let toml_str = r#"
[core]
default_team = "atm-dev"
identity = "team-lead"

[aliases]
arch-atm = "team-lead"
dev = "worker-1"
codex = "arch-ctm"
"#;

    let config: Config = toml::from_str(toml_str).expect("Config parses successfully");

    // Verify aliases are loaded
    assert_eq!(config.aliases.len(), 3);
    assert_eq!(config.aliases.get("arch-atm").map(String::as_str), Some("team-lead"));
    assert_eq!(config.aliases.get("dev").map(String::as_str), Some("worker-1"));
    assert_eq!(config.aliases.get("codex").map(String::as_str), Some("arch-ctm"));

    // Resolve aliases using the helper
    assert_eq!(resolve_alias("arch-atm", &config.aliases), "team-lead");
    assert_eq!(resolve_alias("dev", &config.aliases), "worker-1");
    assert_eq!(resolve_alias("unknown", &config.aliases), "unknown");
}

#[test]
fn test_alias_config_roundtrip_serialization() {
    let toml_str = r#"
[aliases]
arch-atm = "team-lead"
"#;

    let config: Config = toml::from_str(toml_str).unwrap();
    let reserialized = toml::to_string(&config).unwrap();
    let config2: Config = toml::from_str(&reserialized).unwrap();

    assert_eq!(config.aliases, config2.aliases);
}

#[test]
fn test_alias_config_empty_section() {
    // Config without [aliases] should default to empty HashMap
    let toml_str = r#"
[core]
default_team = "atm-dev"
identity = "team-lead"
"#;

    let config: Config = toml::from_str(toml_str).unwrap();
    assert!(config.aliases.is_empty());

    // resolve_alias should pass through with empty aliases
    assert_eq!(resolve_alias("any-agent", &config.aliases), "any-agent");
}

#[test]
fn test_alias_default_config() {
    let config = Config::default();
    assert!(config.aliases.is_empty());
}

// ── Test 5: Alias resolution for send command ─────────────────────────────────

#[test]
fn test_alias_send_resolves() {
    // Verify that alias resolution works correctly for the send command path.
    // The actual send command requires a real filesystem setup, so we test
    // the resolution function directly here.

    let mut aliases = HashMap::new();
    aliases.insert("arch-atm".to_string(), "team-lead".to_string());
    aliases.insert("qa".to_string(), "tester-1".to_string());

    // Direct alias lookup
    assert_eq!(resolve_alias("arch-atm", &aliases), "team-lead");
    assert_eq!(resolve_alias("qa", &aliases), "tester-1");

    // Unknown names pass through unchanged
    assert_eq!(resolve_alias("team-lead", &aliases), "team-lead");
    assert_eq!(resolve_alias("unknown-agent", &aliases), "unknown-agent");
}

#[test]
fn test_alias_resolution_with_at_syntax() {
    // Aliases that include @team should resolve correctly.
    // The resolved value is then parsed by parse_address.
    let mut aliases = HashMap::new();
    aliases.insert("arch-atm".to_string(), "team-lead".to_string());

    // A name with @team suffix is not in aliases — pass through
    assert_eq!(
        resolve_alias("arch-atm@atm-dev", &aliases),
        "arch-atm@atm-dev"
    );

    // The bare name matches
    assert_eq!(resolve_alias("arch-atm", &aliases), "team-lead");
}

// ── Test 6: PubSub unit tests ─────────────────────────────────────────────────

#[test]
fn test_pubsub_subscribe_and_match() {
    let mut pubsub = PubSub::new();

    pubsub
        .subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
        .unwrap();

    let matches = pubsub.matching_subscribers("arch-ctm", "idle");
    assert_eq!(matches, vec!["team-lead"]);
}

#[test]
fn test_pubsub_subscribe_and_unsubscribe() {
    let mut pubsub = PubSub::new();

    pubsub
        .subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
        .unwrap();
    pubsub.unsubscribe("team-lead", "arch-ctm");

    let matches = pubsub.matching_subscribers("arch-ctm", "idle");
    assert!(matches.is_empty());
}

#[test]
fn test_pubsub_multiple_subscribers() {
    let mut pubsub = PubSub::new();

    pubsub
        .subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
        .unwrap();
    pubsub
        .subscribe("qa-bot", "arch-ctm", vec!["idle".to_string()])
        .unwrap();

    let mut matches = pubsub.matching_subscribers("arch-ctm", "idle");
    matches.sort();
    assert_eq!(matches, vec!["qa-bot", "team-lead"]);
}

#[test]
fn test_pubsub_no_match_for_different_event() {
    let mut pubsub = PubSub::new();

    pubsub
        .subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
        .unwrap();

    // subscribed to "idle" but querying "busy"
    let matches = pubsub.matching_subscribers("arch-ctm", "busy");
    assert!(matches.is_empty());
}

#[test]
fn test_pubsub_shared_store() {
    // Verify that Arc<Mutex<PubSub>> works correctly (as used in production)
    let store: Arc<Mutex<PubSub>> = Arc::new(Mutex::new(PubSub::new()));

    {
        let mut ps = store.lock().unwrap();
        ps.subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
            .unwrap();
    }

    let matches = store.lock().unwrap().matching_subscribers("arch-ctm", "idle");
    assert_eq!(matches, vec!["team-lead"]);
}
