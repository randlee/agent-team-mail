//! Roster service for managing synthetic team members

use crate::roster::tracking::MembershipTracker;
use agent_team_mail_core::schema::{AgentMember, TeamConfig};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Cleanup mode for plugin shutdown
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupMode {
    /// Set isActive to false but keep member in config
    Soft,
    /// Remove member entirely from config
    Hard,
}

/// Error type for roster operations
#[derive(Debug, thiserror::Error)]
pub enum RosterError {
    #[error("I/O error: {0}")]
    Io(String),
    #[error("JSON error: {0}")]
    Json(String),
    #[error("member '{name}' already exists in team '{team}'")]
    DuplicateMember { team: String, name: String },
    #[error("member '{name}' not found in team '{team}'")]
    MemberNotFound { team: String, name: String },
    #[error("team config not found: {0}")]
    TeamNotFound(String),
}

/// Manages synthetic team members registered by plugins
///
/// Provides atomic operations for adding, removing, and listing plugin-registered
/// team members. All config.json modifications are protected by file locks to
/// ensure consistency across concurrent operations.
#[derive(Clone)]
pub struct RosterService {
    teams_root: PathBuf,
    tracker: Arc<Mutex<MembershipTracker>>,
}

impl RosterService {
    /// Create a new roster service
    ///
    /// # Arguments
    ///
    /// * `teams_root` - Path to the teams directory (typically `~/.claude/teams`)
    pub fn new(teams_root: PathBuf) -> Self {
        Self {
            teams_root,
            tracker: Arc::new(Mutex::new(MembershipTracker::new())),
        }
    }

    /// Add a member to a team's roster
    ///
    /// Atomically reads the team config, adds the member (rejecting duplicates
    /// by name), writes back atomically, and tracks the membership.
    ///
    /// # Arguments
    ///
    /// * `team` - Team name
    /// * `member` - Agent member to add
    /// * `plugin_name` - Name of the plugin registering this member
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Team config doesn't exist
    /// - Member with same name already exists
    /// - I/O or JSON serialization fails
    pub fn add_member(
        &self,
        team: &str,
        member: AgentMember,
        plugin_name: &str,
    ) -> Result<(), RosterError> {
        let config_path = self.config_path(team);
        if !config_path.exists() {
            return Err(RosterError::TeamNotFound(team.to_string()));
        }

        let member_name = member.name.clone();
        atomic_config_update(&config_path, |config| {
            // Check for duplicate
            if config.members.iter().any(|m| m.name == member_name) {
                return Err(RosterError::DuplicateMember {
                    team: team.to_string(),
                    name: member_name.clone(),
                });
            }
            config.members.push(member);
            Ok(true)
        })?;

        // Track the membership
        self.tracker
            .lock()
            .unwrap()
            .track(plugin_name, team, &member_name);

        Ok(())
    }

    /// Remove a member from a team's roster
    ///
    /// Atomically reads the team config, removes the member by name, writes
    /// back atomically, and untracks the membership.
    ///
    /// # Arguments
    ///
    /// * `team` - Team name
    /// * `agent_name` - Name of the agent to remove
    /// * `plugin_name` - Name of the plugin that registered this member
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Team config doesn't exist
    /// - Member not found
    /// - I/O or JSON serialization fails
    pub fn remove_member(
        &self,
        team: &str,
        agent_name: &str,
        plugin_name: &str,
    ) -> Result<(), RosterError> {
        let config_path = self.config_path(team);
        if !config_path.exists() {
            return Err(RosterError::TeamNotFound(team.to_string()));
        }

        atomic_config_update(&config_path, |config| {
            let initial_len = config.members.len();
            config.members.retain(|m| m.name != agent_name);
            if config.members.len() == initial_len {
                return Err(RosterError::MemberNotFound {
                    team: team.to_string(),
                    name: agent_name.to_string(),
                });
            }
            Ok(true)
        })?;

        // Untrack the membership
        self.tracker
            .lock()
            .unwrap()
            .untrack(plugin_name, team, agent_name);

        Ok(())
    }

    /// List members in a team's roster
    ///
    /// Reads the team config and returns members, optionally filtered by plugin.
    /// When `plugin_name` is Some, only returns members whose `agent_type`
    /// starts with `"plugin:{plugin_name}"`.
    ///
    /// # Arguments
    ///
    /// * `team` - Team name
    /// * `plugin_name` - Optional plugin name filter
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Team config doesn't exist
    /// - I/O or JSON parse fails
    pub fn list_members(
        &self,
        team: &str,
        plugin_name: Option<&str>,
    ) -> Result<Vec<AgentMember>, RosterError> {
        let config_path = self.config_path(team);
        if !config_path.exists() {
            return Err(RosterError::TeamNotFound(team.to_string()));
        }

        let content = std::fs::read(&config_path)
            .map_err(|e| RosterError::Io(format!("read failed: {e}")))?;
        let config: TeamConfig = serde_json::from_slice(&content)
            .map_err(|e| RosterError::Json(format!("parse failed: {e}")))?;

        let mut members: Vec<AgentMember> = config
            .members
            .into_iter()
            .filter(|m| m.agent_type.starts_with("plugin:"))
            .collect();

        if let Some(plugin) = plugin_name {
            let prefix = format!("plugin:{plugin}");
            members.retain(|m| m.agent_type == prefix);
        }

        Ok(members)
    }

    /// Clean up plugin members on shutdown
    ///
    /// Based on the cleanup mode:
    /// - `Soft`: Sets `is_active = false` for matching members
    /// - `Hard`: Removes matching members entirely
    ///
    /// Returns the count of affected members. Idempotent - calling multiple
    /// times with the same arguments is safe.
    ///
    /// # Arguments
    ///
    /// * `team` - Team name
    /// * `plugin_name` - Plugin name to clean up
    /// * `mode` - Cleanup mode (Soft or Hard)
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Team config doesn't exist
    /// - I/O or JSON serialization fails
    pub fn cleanup_plugin(
        &self,
        team: &str,
        plugin_name: &str,
        mode: CleanupMode,
    ) -> Result<usize, RosterError> {
        let config_path = self.config_path(team);
        if !config_path.exists() {
            return Err(RosterError::TeamNotFound(team.to_string()));
        }

        let prefix = format!("plugin:{plugin_name}");
        let mut affected_count = 0;

        atomic_config_update(&config_path, |config| {
            match mode {
                CleanupMode::Soft => {
                    for member in &mut config.members {
                        if member.agent_type == prefix && member.is_active != Some(false) {
                            member.is_active = Some(false);
                            affected_count += 1;
                        }
                    }
                }
                CleanupMode::Hard => {
                    let initial_len = config.members.len();
                    config.members.retain(|m| m.agent_type != prefix);
                    affected_count = initial_len - config.members.len();
                }
            }
            Ok(affected_count > 0)
        })?;

        // Clear tracking for this plugin in this team
        if affected_count > 0 {
            let mut tracker = self.tracker.lock().unwrap();
            let members = tracker.get_members(plugin_name);
            for (member_team, agent_name) in members {
                if member_team == team {
                    tracker.untrack(plugin_name, team, &agent_name);
                }
            }
        }

        Ok(affected_count)
    }

    /// Get the path to a team's config.json
    fn config_path(&self, team: &str) -> PathBuf {
        self.teams_root.join(team).join("config.json")
    }
}

/// Atomically update a team config file
///
/// Acquires an exclusive lock, reads the config, applies the modification
/// function, and writes back atomically if changes were made.
///
/// # Arguments
///
/// * `config_path` - Path to config.json
/// * `modify_fn` - Function that modifies the config and returns Ok(true) if
///   changes were made, Ok(false) if no changes, or Err on error
///
/// # Returns
///
/// Returns Ok(()) if successful, or RosterError on failure.
fn atomic_config_update<F>(config_path: &Path, modify_fn: F) -> Result<(), RosterError>
where
    F: FnOnce(&mut TeamConfig) -> Result<bool, RosterError>,
{
    let lock_path = config_path.with_extension("lock");
    // Use 10 retries (max ~51s backoff) to handle slow I/O on Windows CI and
    // high-concurrency scenarios where multiple threads compete for the same lock.
    let _lock = agent_team_mail_core::io::lock::acquire_lock(&lock_path, 10)
        .map_err(|e| RosterError::Io(format!("lock failed: {e}")))?;

    // Read current config
    let content =
        std::fs::read(config_path).map_err(|e| RosterError::Io(format!("read failed: {e}")))?;
    let mut config: TeamConfig = serde_json::from_slice(&content)
        .map_err(|e| RosterError::Json(format!("parse failed: {e}")))?;

    // Apply modification
    let changed = modify_fn(&mut config)?;
    if !changed {
        return Ok(());
    }

    // Write back atomically (write to tmp, then rename)
    let tmp_path = config_path.with_extension("tmp");
    let new_content = serde_json::to_vec_pretty(&config)
        .map_err(|e| RosterError::Json(format!("serialize failed: {e}")))?;

    std::fs::write(&tmp_path, &new_content)
        .map_err(|e| RosterError::Io(format!("write tmp failed: {e}")))?;
    std::fs::rename(&tmp_path, config_path)
        .map_err(|e| RosterError::Io(format!("rename failed: {e}")))?;

    Ok(())
    // Lock released on drop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cleanup_mode_equality() {
        assert_eq!(CleanupMode::Soft, CleanupMode::Soft);
        assert_eq!(CleanupMode::Hard, CleanupMode::Hard);
        assert_ne!(CleanupMode::Soft, CleanupMode::Hard);
    }

    #[test]
    fn test_roster_service_new() {
        let service = RosterService::new(PathBuf::from("/tmp/teams"));
        assert_eq!(service.teams_root, PathBuf::from("/tmp/teams"));
    }

    #[test]
    fn test_config_path() {
        let service = RosterService::new(PathBuf::from("/tmp/teams"));
        let path = service.config_path("test-team");
        assert_eq!(
            path,
            PathBuf::from("/tmp/teams").join("test-team").join("config.json")
        );
    }
}
