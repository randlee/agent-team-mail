//! Plugin membership tracking

use std::collections::HashMap;

/// Tracks which agents belong to which plugins
///
/// Used by RosterService to maintain a mapping of plugin names to their
/// registered team members. This allows efficient cleanup when a plugin
/// shuts down.
#[derive(Clone, Debug)]
pub struct MembershipTracker {
    /// Map of plugin name → Vec<(team_name, agent_name)>
    memberships: HashMap<String, Vec<(String, String)>>,
}

impl MembershipTracker {
    /// Create a new empty tracker
    pub fn new() -> Self {
        Self {
            memberships: HashMap::new(),
        }
    }

    /// Track a new member registration
    ///
    /// Records that the given plugin has registered an agent in the given team.
    pub fn track(&mut self, plugin: &str, team: &str, agent: &str) {
        self.memberships
            .entry(plugin.to_string())
            .or_default()
            .push((team.to_string(), agent.to_string()));
    }

    /// Untrack a member
    ///
    /// Removes the tracking entry for the given plugin/team/agent combination.
    pub fn untrack(&mut self, plugin: &str, team: &str, agent: &str) {
        if let Some(members) = self.memberships.get_mut(plugin) {
            members.retain(|(t, a)| t != team || a != agent);
            if members.is_empty() {
                self.memberships.remove(plugin);
            }
        }
    }

    /// Get all members tracked for a plugin
    ///
    /// Returns a vector of (team_name, agent_name) tuples for the given plugin.
    pub fn get_members(&self, plugin: &str) -> Vec<(String, String)> {
        self.memberships
            .get(plugin)
            .cloned()
            .unwrap_or_default()
    }

    /// Clear all tracking for a plugin
    ///
    /// Removes all tracked members for the given plugin.
    /// Returns the number of members that were removed.
    pub fn clear_plugin(&mut self, plugin: &str) -> usize {
        self.memberships
            .remove(plugin)
            .map(|v| v.len())
            .unwrap_or(0)
    }
}

impl Default for MembershipTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_track_and_get() {
        let mut tracker = MembershipTracker::new();
        tracker.track("issues", "team-a", "issues-bot");
        tracker.track("issues", "team-b", "issues-watcher");

        let members = tracker.get_members("issues");
        assert_eq!(members.len(), 2);
        assert!(members.contains(&("team-a".to_string(), "issues-bot".to_string())));
        assert!(members.contains(&("team-b".to_string(), "issues-watcher".to_string())));
    }

    #[test]
    fn test_untrack() {
        let mut tracker = MembershipTracker::new();
        tracker.track("issues", "team-a", "issues-bot");
        tracker.track("issues", "team-b", "issues-watcher");

        tracker.untrack("issues", "team-a", "issues-bot");

        let members = tracker.get_members("issues");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0], ("team-b".to_string(), "issues-watcher".to_string()));
    }

    #[test]
    fn test_clear_plugin() {
        let mut tracker = MembershipTracker::new();
        tracker.track("issues", "team-a", "issues-bot");
        tracker.track("issues", "team-b", "issues-watcher");
        tracker.track("ci", "team-a", "ci-monitor");

        let count = tracker.clear_plugin("issues");
        assert_eq!(count, 2);

        let issues_members = tracker.get_members("issues");
        assert_eq!(issues_members.len(), 0);

        let ci_members = tracker.get_members("ci");
        assert_eq!(ci_members.len(), 1);
    }

    #[test]
    fn test_get_nonexistent_plugin() {
        let tracker = MembershipTracker::new();
        let members = tracker.get_members("nonexistent");
        assert_eq!(members.len(), 0);
    }

    #[test]
    fn test_multiple_plugins() {
        let mut tracker = MembershipTracker::new();
        tracker.track("issues", "team-a", "issues-bot");
        tracker.track("ci", "team-a", "ci-monitor");
        tracker.track("chat", "team-b", "chatbot");

        assert_eq!(tracker.get_members("issues").len(), 1);
        assert_eq!(tracker.get_members("ci").len(), 1);
        assert_eq!(tracker.get_members("chat").len(), 1);
    }
}
