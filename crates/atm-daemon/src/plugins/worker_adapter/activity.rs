//! Agent activity tracking for message timestamps.
//!
//! Monitors inbox file events and updates `lastActive` in team config.json.
//! `isActive` is hook-owned and must not be mutated here.

use crate::plugin::PluginError;
use agent_team_mail_core::io::{atomic::atomic_swap, lock::acquire_lock};
use agent_team_mail_core::schema::TeamConfig;
use std::fs;
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
        self.update_config_atomic_with_time(team_config_path, |config, now_ms| {
            if let Some(member) = config.members.iter_mut().find(|m| m.name == agent_name) {
                member.last_active = Some(now_ms);
                debug!("Updated activity for agent {agent_name}: lastActive={now_ms}");
                true
            } else {
                warn!("Agent {agent_name} not found in team config");
                false
            }
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

    /// Atomically update team config using lock/swap infrastructure
    ///
    /// # Arguments
    ///
    /// * `team_config_path` - Path to team config.json
    /// * `update_fn` - Closure that modifies config, returns true if changes were made
    ///
    /// # Errors
    ///
    /// Returns `PluginError` for I/O errors, JSON errors, or lock timeout
    fn update_config_atomic_with_time<F>(
        &self,
        team_config_path: &Path,
        update_fn: F,
    ) -> Result<(), PluginError>
    where
        F: FnOnce(&mut TeamConfig, u64) -> bool,
    {
        let lock_path = team_config_path.with_extension("lock");

        // Step 1: Acquire lock with retry (5 attempts)
        let _lock = acquire_lock(&lock_path, 5).map_err(|e| PluginError::Runtime {
            message: format!("Failed to acquire lock for team config: {e}"),
            source: None,
        })?;

        // Step 2: Read current config
        let content = fs::read(team_config_path).map_err(|e| PluginError::Runtime {
            message: format!("Failed to read team config: {e}"),
            source: Some(Box::new(e)),
        })?;

        let mut config: TeamConfig =
            serde_json::from_slice(&content).map_err(|e| PluginError::Runtime {
                message: format!("Failed to parse team config JSON: {e}"),
                source: Some(Box::new(e)),
            })?;

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| PluginError::Runtime {
                message: format!("System time error: {e}"),
                source: Some(Box::new(e)),
            })?
            .as_millis() as u64;

        // Step 3: Apply modification
        if !update_fn(&mut config, now_ms) {
            // No changes needed
            return Ok(());
        }

        // Step 4: Write to temp file with fsync, then swap
        let tmp_path = team_config_path.with_extension("tmp");
        let new_content =
            serde_json::to_string_pretty(&config).map_err(|e| PluginError::Runtime {
                message: format!("Failed to serialize team config: {e}"),
                source: Some(Box::new(e)),
            })?;

        // Write to temp file
        let mut file = std::fs::File::create(&tmp_path).map_err(|e| PluginError::Runtime {
            message: format!("Failed to create temp file: {e}"),
            source: Some(Box::new(e)),
        })?;

        std::io::Write::write_all(&mut file, new_content.as_bytes()).map_err(|e| {
            PluginError::Runtime {
                message: format!("Failed to write temp file: {e}"),
                source: Some(Box::new(e)),
            }
        })?;

        file.sync_all().map_err(|e| PluginError::Runtime {
            message: format!("Failed to sync temp file: {e}"),
            source: Some(Box::new(e)),
        })?;

        drop(file);

        // Atomic swap
        atomic_swap(team_config_path, &tmp_path).map_err(|e| PluginError::Runtime {
            message: format!("Failed to swap team config: {e}"),
            source: None,
        })?;

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
    use agent_team_mail_core::schema::AgentMember;
    use std::collections::HashMap;
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
        assert_eq!(agent.is_active, Some(true));
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
        assert_eq!(agent.is_active, Some(false));
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
        assert_eq!(agent.is_active, Some(true));
    }
}
