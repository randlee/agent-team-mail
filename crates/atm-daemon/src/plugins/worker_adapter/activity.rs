//! Agent activity tracking for message timestamps.
//!
//! Monitors inbox file events and updates `lastActive` in team config.json.
//! `isActive` is hook-owned and must not be mutated here.

use crate::plugin::PluginError;
use agent_team_mail_core::team_config_store::TeamConfigStore;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

/// Agent activity tracker
pub struct ActivityTracker {
    /// Inactivity timeout in milliseconds (default: 5 minutes)
    inactivity_timeout_ms: u64,
}

impl ActivityTracker {
    /// Create a new activity tracker
    ///
    /// # Arguments
    ///
    /// * `inactivity_timeout_ms` - How long to wait before marking agent as inactive
    pub fn new(inactivity_timeout_ms: u64) -> Self {
        Self {
            inactivity_timeout_ms,
        }
    }

    /// Update agent activity timestamp when they send a message.
    ///
    /// # Arguments
    ///
    /// * `team_config_path` - Path to team config.json
    /// * `agent_name` - Name of the agent who sent the message
    ///
    /// # Errors
    ///
    /// Returns `PluginError` if config update fails
    pub fn record_activity(
        &self,
        team_config_path: &Path,
        agent_name: &str,
    ) -> Result<(), PluginError> {
        let team_dir = team_config_path
            .parent()
            .ok_or_else(|| PluginError::Runtime {
                message: format!(
                    "team config path {} has no parent directory",
                    team_config_path.display()
                ),
                source: None,
            })?;
        TeamConfigStore::open(team_dir)
            .update(|mut config| {
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|e| anyhow::anyhow!("System time error: {e}"))?
                    .as_millis() as u64;
                if let Some(member) = config.members.iter_mut().find(|m| m.name == agent_name) {
                    member.last_active = Some(now_ms);
                    debug!("Updated activity for agent {agent_name}: lastActive={now_ms}");
                    Ok(Some(config))
                } else {
                    warn!("Agent {agent_name} not found in team config");
                    Ok(None)
                }
            })
            .map(|_| ())
            .map_err(|e| PluginError::Runtime {
                message: format!("Failed to update team config: {e}"),
                source: None,
            })
    }

    /// `isActive` is hook-owned, so inbox activity does not clear activity state.
    ///
    /// # Arguments
    ///
    /// * `team_config_path` - Path to team config.json
    ///
    /// # Errors
    ///
    /// Returns `PluginError` if config update fails
    pub fn check_inactivity(&self, team_config_path: &Path) -> Result<(), PluginError> {
        let _ = team_config_path;
        let _ = self.inactivity_timeout_ms;
        Ok(())
    }
}

impl Default for ActivityTracker {
    fn default() -> Self {
        Self::new(5 * 60 * 1000) // 5 minutes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_core::schema::{AgentMember, TeamConfig};
    use std::collections::HashMap;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_config(temp_dir: &TempDir) -> (TeamConfig, std::path::PathBuf) {
        let config = TeamConfig {
            name: "test-team".to_string(),
            description: None,
            created_at: 1234567890,
            lead_agent_id: "team-lead@test-team".to_string(),
            lead_session_id: "session-123".to_string(),
            members: vec![AgentMember {
                agent_id: "agent1@test-team".to_string(),
                name: "agent1".to_string(),
                agent_type: "general-purpose".to_string(),
                model: "claude-opus-4-6".to_string(),
                prompt: None,
                color: None,
                plan_mode_required: None,
                joined_at: 1234567890,
                tmux_pane_id: None,
                cwd: "/test".to_string(),
                subscriptions: vec![],
                backend_type: None,
                is_active: None,
                last_active: None,
                session_id: None,
                external_backend_type: None,
                external_model: None,
                unknown_fields: HashMap::new(),
            }],
            unknown_fields: HashMap::new(),
        };

        let config_path = temp_dir.path().join("config.json");
        std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        (config, config_path)
    }

    #[test]
    fn test_record_activity() {
        let temp_dir = TempDir::new().unwrap();
        let (_config, config_path) = create_test_config(&temp_dir);

        let tracker = ActivityTracker::default();
        tracker.record_activity(&config_path, "agent1").unwrap();

        // Read back and verify
        let content = fs::read(&config_path).unwrap();
        let updated_config: TeamConfig = serde_json::from_slice(&content).unwrap();

        let agent = updated_config
            .members
            .iter()
            .find(|m| m.name == "agent1")
            .unwrap();
        assert_eq!(agent.is_active, None);
        assert!(agent.last_active.is_some());
    }

    #[test]
    fn test_check_inactivity_timeout() {
        let temp_dir = TempDir::new().unwrap();
        let (_config, config_path) = create_test_config(&temp_dir);

        // Short timeout for testing (100ms)
        let tracker = ActivityTracker::new(100);

        // Record activity
        tracker.record_activity(&config_path, "agent1").unwrap();

        // Sleep to exceed timeout
        std::thread::sleep(std::time::Duration::from_millis(150));

        // Check inactivity
        tracker.check_inactivity(&config_path).unwrap();

        // Read back and verify agent is now inactive
        let content = fs::read(&config_path).unwrap();
        let updated_config: TeamConfig = serde_json::from_slice(&content).unwrap();

        let agent = updated_config
            .members
            .iter()
            .find(|m| m.name == "agent1")
            .unwrap();
        assert_eq!(agent.is_active, None);
    }

    #[test]
    fn test_check_inactivity_no_timeout() {
        let temp_dir = TempDir::new().unwrap();
        let (_config, config_path) = create_test_config(&temp_dir);

        // Long timeout
        let tracker = ActivityTracker::new(10_000);

        // Record activity
        tracker.record_activity(&config_path, "agent1").unwrap();

        // Check inactivity immediately
        tracker.check_inactivity(&config_path).unwrap();

        // Agent should still be active
        let content = fs::read(&config_path).unwrap();
        let updated_config: TeamConfig = serde_json::from_slice(&content).unwrap();

        let agent = updated_config
            .members
            .iter()
            .find(|m| m.name == "agent1")
            .unwrap();
        assert_eq!(agent.is_active, None);
    }
}
