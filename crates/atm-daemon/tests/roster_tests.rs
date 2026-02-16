use agent_team_mail_core::schema::{AgentMember, TeamConfig};
use agent_team_mail_daemon::roster::{CleanupMode, MembershipTracker, RosterError, RosterService};
use std::collections::HashMap;
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::TempDir;

// ============================================================================
// Test Helpers
// ============================================================================

/// Create a team config.json in a temp directory
fn setup_team(temp_dir: &TempDir, team_name: &str) -> std::path::PathBuf {
    let team_dir = temp_dir.path().join(team_name);
    std::fs::create_dir_all(&team_dir).unwrap();

    let config = TeamConfig {
        name: team_name.to_string(),
        description: Some("Test team".to_string()),
        created_at: 1770765919076,
        lead_agent_id: format!("team-lead@{team_name}"),
        lead_session_id: "test-session-id".to_string(),
        members: vec![create_lead_member(team_name)],
        unknown_fields: HashMap::new(),
    };

    let config_path = team_dir.join("config.json");
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    team_dir
}

fn create_lead_member(team_name: &str) -> AgentMember {
    AgentMember {
        agent_id: format!("team-lead@{team_name}"),
        name: "team-lead".to_string(),
        agent_type: "general-purpose".to_string(),
        model: "claude-opus-4-6".to_string(),
        prompt: None,
        color: None,
        plan_mode_required: None,
        joined_at: 1770765919076,
        tmux_pane_id: None,
        cwd: "/test".to_string(),
        subscriptions: vec![],
        backend_type: None,
        is_active: Some(true),
        last_active: Some(1770765919076),
        unknown_fields: HashMap::new(),
    }
}

fn create_synthetic_member(plugin_name: &str, function_name: &str, team_name: &str) -> AgentMember {
    let now_ms = chrono::Utc::now().timestamp_millis() as u64;
    AgentMember {
        agent_id: format!("{plugin_name}-{function_name}@{team_name}"),
        name: format!("{plugin_name}-{function_name}"),
        agent_type: format!("plugin:{plugin_name}"),
        model: "synthetic".to_string(),
        prompt: None,
        color: None,
        plan_mode_required: None,
        joined_at: now_ms,
        tmux_pane_id: None,
        cwd: "/test".to_string(),
        subscriptions: vec![],
        backend_type: None,
        is_active: Some(true),
        last_active: Some(now_ms),
        unknown_fields: HashMap::new(),
    }
}

// ============================================================================
// MembershipTracker Unit Tests
// ============================================================================

#[test]
fn test_membership_tracker_basic() {
    let mut tracker = MembershipTracker::new();

    tracker.track("issues", "team-a", "issues-bot");
    tracker.track("issues", "team-b", "issues-watcher");

    let members = tracker.get_members("issues");
    assert_eq!(members.len(), 2);
    assert!(members.contains(&("team-a".to_string(), "issues-bot".to_string())));
    assert!(members.contains(&("team-b".to_string(), "issues-watcher".to_string())));

    let count = tracker.clear_plugin("issues");
    assert_eq!(count, 2);

    let members_after = tracker.get_members("issues");
    assert_eq!(members_after.len(), 0);
}

// ============================================================================
// RosterService Tests
// ============================================================================

#[test]
fn test_add_member_success() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let service = RosterService::new(temp_dir.path().to_path_buf());
    let member = create_synthetic_member("issues", "bot", "test-team");

    service.add_member("test-team", member.clone(), "issues").unwrap();

    // Read config and verify member was added
    let config_path = temp_dir.path().join("test-team").join("config.json");
    let content = std::fs::read(&config_path).unwrap();
    let config: TeamConfig = serde_json::from_slice(&content).unwrap();

    assert_eq!(config.members.len(), 2); // Lead + synthetic member
    assert!(config.members.iter().any(|m| m.name == "issues-bot"));
}

#[test]
fn test_add_member_duplicate_rejected() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let service = RosterService::new(temp_dir.path().to_path_buf());
    let member = create_synthetic_member("issues", "bot", "test-team");

    // First add should succeed
    service.add_member("test-team", member.clone(), "issues").unwrap();

    // Second add should fail with DuplicateMember error
    let result = service.add_member("test-team", member, "issues");
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        RosterError::DuplicateMember { .. }
    ));
}

#[test]
fn test_remove_member_success() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let service = RosterService::new(temp_dir.path().to_path_buf());
    let member = create_synthetic_member("issues", "bot", "test-team");

    service.add_member("test-team", member, "issues").unwrap();
    service.remove_member("test-team", "issues-bot", "issues").unwrap();

    // Verify member was removed
    let config_path = temp_dir.path().join("test-team").join("config.json");
    let content = std::fs::read(&config_path).unwrap();
    let config: TeamConfig = serde_json::from_slice(&content).unwrap();

    assert_eq!(config.members.len(), 1); // Only lead remains
    assert!(!config.members.iter().any(|m| m.name == "issues-bot"));
}

#[test]
fn test_remove_member_not_found() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let service = RosterService::new(temp_dir.path().to_path_buf());

    let result = service.remove_member("test-team", "nonexistent-bot", "issues");
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        RosterError::MemberNotFound { .. }
    ));
}

#[test]
fn test_list_members_all() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let service = RosterService::new(temp_dir.path().to_path_buf());
    let member1 = create_synthetic_member("issues", "bot", "test-team");
    let member2 = create_synthetic_member("ci", "monitor", "test-team");

    service.add_member("test-team", member1, "issues").unwrap();
    service.add_member("test-team", member2, "ci").unwrap();

    // List all synthetic members (no plugin filter)
    let members = service.list_members("test-team", None).unwrap();
    assert_eq!(members.len(), 2);
    assert!(members.iter().any(|m| m.name == "issues-bot"));
    assert!(members.iter().any(|m| m.name == "ci-monitor"));
}

#[test]
fn test_list_members_filtered_by_plugin() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let service = RosterService::new(temp_dir.path().to_path_buf());
    let member1 = create_synthetic_member("issues", "bot", "test-team");
    let member2 = create_synthetic_member("issues", "watcher", "test-team");
    let member3 = create_synthetic_member("ci", "monitor", "test-team");

    service.add_member("test-team", member1, "issues").unwrap();
    service.add_member("test-team", member2, "issues").unwrap();
    service.add_member("test-team", member3, "ci").unwrap();

    // Filter by "issues" plugin
    let issues_members = service.list_members("test-team", Some("issues")).unwrap();
    assert_eq!(issues_members.len(), 2);
    assert!(issues_members.iter().all(|m| m.agent_type == "plugin:issues"));

    // Filter by "ci" plugin
    let ci_members = service.list_members("test-team", Some("ci")).unwrap();
    assert_eq!(ci_members.len(), 1);
    assert_eq!(ci_members[0].name, "ci-monitor");
}

#[test]
fn test_cleanup_plugin_hard() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let service = RosterService::new(temp_dir.path().to_path_buf());
    let member1 = create_synthetic_member("issues", "bot", "test-team");
    let member2 = create_synthetic_member("issues", "watcher", "test-team");
    let member3 = create_synthetic_member("ci", "monitor", "test-team");

    service.add_member("test-team", member1, "issues").unwrap();
    service.add_member("test-team", member2, "issues").unwrap();
    service.add_member("test-team", member3, "ci").unwrap();

    // Hard cleanup of "issues" plugin
    let count = service
        .cleanup_plugin("test-team", "issues", CleanupMode::Hard)
        .unwrap();
    assert_eq!(count, 2);

    // Verify only "ci" member remains
    let members = service.list_members("test-team", None).unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].name, "ci-monitor");
}

#[test]
fn test_cleanup_plugin_soft() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let service = RosterService::new(temp_dir.path().to_path_buf());
    let member = create_synthetic_member("issues", "bot", "test-team");

    service.add_member("test-team", member, "issues").unwrap();

    // Soft cleanup
    let count = service
        .cleanup_plugin("test-team", "issues", CleanupMode::Soft)
        .unwrap();
    assert_eq!(count, 1);

    // Verify member still exists but isActive is false
    let config_path = temp_dir.path().join("test-team").join("config.json");
    let content = std::fs::read(&config_path).unwrap();
    let config: TeamConfig = serde_json::from_slice(&content).unwrap();

    let issues_member = config
        .members
        .iter()
        .find(|m| m.name == "issues-bot")
        .unwrap();
    assert_eq!(issues_member.is_active, Some(false));
}

#[test]
fn test_cleanup_plugin_idempotent() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let service = RosterService::new(temp_dir.path().to_path_buf());
    let member = create_synthetic_member("issues", "bot", "test-team");

    service.add_member("test-team", member, "issues").unwrap();

    // First cleanup
    let count1 = service
        .cleanup_plugin("test-team", "issues", CleanupMode::Hard)
        .unwrap();
    assert_eq!(count1, 1);

    // Second cleanup should return 0 (nothing to clean)
    let count2 = service
        .cleanup_plugin("test-team", "issues", CleanupMode::Hard)
        .unwrap();
    assert_eq!(count2, 0);
}

#[test]
fn test_concurrent_add_remove() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let service = Arc::new(RosterService::new(temp_dir.path().to_path_buf()));
    let barrier = Arc::new(Barrier::new(4));

    let mut handles = vec![];

    // Spawn 4 threads that add and remove members concurrently
    for i in 0..4 {
        let service_clone = Arc::clone(&service);
        let barrier_clone = Arc::clone(&barrier);

        let handle = thread::spawn(move || {
            barrier_clone.wait();

            let member_name = format!("test-{i}");
            let member = AgentMember {
                agent_id: format!("{member_name}@test-team"),
                name: member_name.clone(),
                agent_type: "plugin:test".to_string(),
                model: "synthetic".to_string(),
                prompt: None,
                color: None,
                plan_mode_required: None,
                joined_at: 1770765919076,
                tmux_pane_id: None,
                cwd: "/test".to_string(),
                subscriptions: vec![],
                backend_type: None,
                is_active: Some(true),
                last_active: Some(1770765919076),
                unknown_fields: HashMap::new(),
            };

            // Add member
            service_clone
                .add_member("test-team", member, "test")
                .unwrap();

            // Small delay
            thread::sleep(std::time::Duration::from_millis(10));

            // Remove member
            service_clone
                .remove_member("test-team", &member_name, "test")
                .unwrap();
        });

        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    // Verify config is still valid JSON and only lead remains
    let config_path = temp_dir.path().join("test-team").join("config.json");
    let content = std::fs::read(&config_path).unwrap();
    let config: TeamConfig = serde_json::from_slice(&content).unwrap();

    assert_eq!(config.members.len(), 1); // Only lead should remain
    assert_eq!(config.members[0].name, "team-lead");
}

#[test]
fn test_multiple_plugins_isolation() {
    let temp_dir = TempDir::new().unwrap();
    setup_team(&temp_dir, "test-team");

    let service = RosterService::new(temp_dir.path().to_path_buf());

    // Add members from two different plugins
    let issues1 = create_synthetic_member("issues", "bot", "test-team");
    let issues2 = create_synthetic_member("issues", "watcher", "test-team");
    let ci1 = create_synthetic_member("ci", "monitor", "test-team");
    let ci2 = create_synthetic_member("ci", "reporter", "test-team");

    service.add_member("test-team", issues1, "issues").unwrap();
    service.add_member("test-team", issues2, "issues").unwrap();
    service.add_member("test-team", ci1, "ci").unwrap();
    service.add_member("test-team", ci2, "ci").unwrap();

    // Cleanup issues plugin
    service
        .cleanup_plugin("test-team", "issues", CleanupMode::Hard)
        .unwrap();

    // Verify only ci members remain
    let members = service.list_members("test-team", None).unwrap();
    assert_eq!(members.len(), 2);
    assert!(members.iter().all(|m| m.agent_type == "plugin:ci"));
}

#[test]
fn test_team_not_found() {
    let temp_dir = TempDir::new().unwrap();
    let service = RosterService::new(temp_dir.path().to_path_buf());

    let member = create_synthetic_member("issues", "bot", "nonexistent-team");

    let result = service.add_member("nonexistent-team", member, "issues");
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        RosterError::TeamNotFound(_)
    ));
}

#[test]
fn test_preserves_unknown_fields() {
    let temp_dir = TempDir::new().unwrap();
    let team_dir = setup_team(&temp_dir, "test-team");

    // Add unknown fields to the config
    let config_path = team_dir.join("config.json");
    let mut content: serde_json::Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    content["unknownField"] = serde_json::json!("test-value");
    content["futureFeature"] = serde_json::json!({"nested": "data"});
    std::fs::write(&config_path, serde_json::to_vec_pretty(&content).unwrap()).unwrap();

    // Add a member through the service
    let service = RosterService::new(temp_dir.path().to_path_buf());
    let member = create_synthetic_member("issues", "bot", "test-team");
    service.add_member("test-team", member, "issues").unwrap();

    // Verify unknown fields are preserved
    let content: serde_json::Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    assert_eq!(content["unknownField"], "test-value");
    assert_eq!(content["futureFeature"]["nested"], "data");
}
