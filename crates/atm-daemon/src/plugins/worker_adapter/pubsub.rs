//! Ephemeral in-memory pub/sub registry for agent state change notifications.
//!
//! Subscribers register interest in specific agent + state event combinations.
//! When an agent transitions to a matching state, all non-expired subscribers
//! receive a notification delivered to their ATM inbox.
//!
//! ## Design
//!
//! - **Ephemeral**: All subscriptions are lost on daemon restart. Subscribers
//!   must re-subscribe after reconnecting.
//! - **TTL-based**: Subscriptions expire after a configurable duration (default 1 h).
//!   Call [`PubSub::gc`] periodically (e.g., every 60 s) to reclaim memory.
//! - **Upsert semantics**: Re-subscribing with the same `(subscriber, agent)` pair
//!   refreshes the TTL and updates the event filter rather than creating a duplicate.
//! - **Cap enforcement**: Each subscriber is limited to at most
//!   [`DEFAULT_MAX_PER_SUBSCRIBER`] subscriptions. The cap is checked only on new
//!   inserts; refreshes (upserts) never fail with [`PubSubError::CapExceeded`].
//!
//! ## Example
//!
//! ```rust
//! use std::time::Duration;
//! use agent_team_mail_daemon::plugins::worker_adapter::pubsub::PubSub;
//!
//! let mut ps = PubSub::new();
//! ps.subscribe("team-lead", "arch-ctm", vec!["idle".to_string()]).unwrap();
//!
//! let notified = ps.matching_subscribers("arch-ctm", "idle");
//! assert_eq!(notified, vec!["team-lead".to_string()]);
//! ```

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Default TTL for subscriptions: 1 hour.
pub const DEFAULT_TTL_SECS: u64 = 3600;

/// Default maximum number of subscriptions per subscriber.
pub const DEFAULT_MAX_PER_SUBSCRIBER: usize = 50;

/// A single subscription entry.
#[derive(Debug, Clone)]
pub struct Subscription {
    /// ATM identity of the subscriber (e.g., `"team-lead"`).
    pub subscriber: String,
    /// Agent being watched (e.g., `"arch-ctm"`).
    pub agent: String,
    /// State events that trigger a notification (e.g., `["idle"]`).
    ///
    /// An empty list is treated as a wildcard: all state transitions match.
    pub events: Vec<String>,
    /// When the subscription was created or last refreshed.
    pub created_at: Instant,
}

impl Subscription {
    /// Returns `true` if this subscription has not yet expired given `ttl`.
    pub fn is_alive(&self, ttl: Duration) -> bool {
        self.created_at.elapsed() < ttl
    }

    /// Returns `true` if `state` matches the event filter.
    ///
    /// An empty `events` list is a wildcard and matches any state.
    pub fn matches_event(&self, state: &str) -> bool {
        self.events.is_empty() || self.events.iter().any(|e| e == state)
    }
}

/// Ephemeral in-memory pub/sub registry.
///
/// All subscriptions are keyed by `(subscriber, agent)` for upsert semantics
/// and are stored in a flat `HashMap` for O(1) insert/lookup/remove.
pub struct PubSub {
    /// Subscriptions keyed by `(subscriber, agent)`.
    subscriptions: HashMap<(String, String), Subscription>,
    /// Time-to-live for each subscription.
    ttl: Duration,
    /// Maximum number of subscriptions allowed per subscriber.
    max_per_subscriber: usize,
}

impl PubSub {
    /// Create a new registry with default configuration:
    /// 1-hour TTL and a cap of 50 subscriptions per subscriber.
    pub fn new() -> Self {
        Self::with_config(
            Duration::from_secs(DEFAULT_TTL_SECS),
            DEFAULT_MAX_PER_SUBSCRIBER,
        )
    }

    /// Create a new registry with explicit configuration.
    ///
    /// # Arguments
    ///
    /// * `ttl` - Lifetime of each subscription; after this duration `gc()` will
    ///   remove the entry.
    /// * `max_per_subscriber` - Maximum number of simultaneously active
    ///   subscriptions for a single subscriber identity.
    pub fn with_config(ttl: Duration, max_per_subscriber: usize) -> Self {
        Self {
            subscriptions: HashMap::new(),
            ttl,
            max_per_subscriber,
        }
    }

    /// Subscribe to state change notifications for `agent`, or refresh an
    /// existing subscription (upsert semantics).
    ///
    /// If `(subscriber, agent)` already exists the TTL is reset and the event
    /// filter is updated without checking the cap. New subscriptions are
    /// rejected with [`PubSubError::CapExceeded`] when the subscriber already
    /// holds `max_per_subscriber` active entries.
    ///
    /// # Errors
    ///
    /// Returns [`PubSubError::CapExceeded`] when attempting to create a *new*
    /// subscription that would exceed the per-subscriber cap.
    pub fn subscribe(
        &mut self,
        subscriber: &str,
        agent: &str,
        events: Vec<String>,
    ) -> Result<(), PubSubError> {
        let key = (subscriber.to_string(), agent.to_string());

        // If an entry already exists, refresh it (upsert — never fails with cap).
        if self.subscriptions.contains_key(&key) {
            let entry = self.subscriptions.get_mut(&key).unwrap();
            entry.events = events;
            entry.created_at = Instant::now();
            debug!("Refreshed subscription: {} watching {} events={:?}", subscriber, agent, entry.events);
            return Ok(());
        }

        // New subscription — check per-subscriber cap.
        let current_count = self.count_for_subscriber(subscriber);
        if current_count >= self.max_per_subscriber {
            warn!(
                "Subscription cap exceeded for '{}': {} >= {}",
                subscriber, current_count, self.max_per_subscriber
            );
            return Err(PubSubError::CapExceeded {
                subscriber: subscriber.to_string(),
                max: self.max_per_subscriber,
            });
        }

        self.subscriptions.insert(
            key,
            Subscription {
                subscriber: subscriber.to_string(),
                agent: agent.to_string(),
                events,
                created_at: Instant::now(),
            },
        );
        debug!("New subscription: {} watching {}", subscriber, agent);
        Ok(())
    }

    /// Remove the subscription for `(subscriber, agent)`, if present.
    pub fn unsubscribe(&mut self, subscriber: &str, agent: &str) {
        let key = (subscriber.to_string(), agent.to_string());
        if self.subscriptions.remove(&key).is_some() {
            debug!("Removed subscription: {} was watching {}", subscriber, agent);
        }
    }

    /// Remove all subscriptions for a subscriber.
    pub fn unsubscribe_all(&mut self, subscriber: &str) {
        let before = self.subscriptions.len();
        self.subscriptions
            .retain(|(sub, _), _| sub != subscriber);
        let removed = before - self.subscriptions.len();
        if removed > 0 {
            debug!("Removed {} subscription(s) for {}", removed, subscriber);
        }
    }

    /// Return the identities of all non-expired subscribers that are watching
    /// `agent` and have `state` in their event filter.
    ///
    /// Expired subscriptions are skipped but not removed here; call [`gc`] to
    /// reclaim memory.
    pub fn matching_subscribers(&self, agent: &str, state: &str) -> Vec<String> {
        self.subscriptions
            .values()
            .filter(|sub| {
                sub.agent == agent
                    && sub.is_alive(self.ttl)
                    && sub.matches_event(state)
            })
            .map(|sub| sub.subscriber.clone())
            .collect()
    }

    /// Remove all expired subscriptions.
    ///
    /// Returns the number of entries removed. Call this periodically (e.g.,
    /// every 60 s from the plugin's `run()` loop) to prevent unbounded growth.
    pub fn gc(&mut self) -> usize {
        let ttl = self.ttl;
        let before = self.subscriptions.len();
        self.subscriptions.retain(|_, sub| sub.is_alive(ttl));
        let removed = before - self.subscriptions.len();
        if removed > 0 {
            debug!("PubSub GC removed {} expired subscription(s)", removed);
        }
        removed
    }

    /// Count the number of active (including expired) subscriptions held by
    /// `subscriber`.
    ///
    /// This is used by the cap check and includes subscriptions that have not
    /// yet been GC'd, so the effective limit is consistent regardless of when
    /// GC last ran.
    pub fn count_for_subscriber(&self, subscriber: &str) -> usize {
        self.subscriptions
            .keys()
            .filter(|(sub, _)| sub == subscriber)
            .count()
    }

    /// Total number of subscriptions (including expired, pre-GC).
    pub fn len(&self) -> usize {
        self.subscriptions.len()
    }

    /// Returns `true` when no subscriptions are registered.
    pub fn is_empty(&self) -> bool {
        self.subscriptions.is_empty()
    }
}

impl Default for PubSub {
    fn default() -> Self {
        Self::new()
    }
}

/// Error returned by [`PubSub::subscribe`].
#[derive(Debug, thiserror::Error)]
pub enum PubSubError {
    /// The subscriber already holds the maximum number of subscriptions.
    #[error(
        "Subscription cap exceeded for '{subscriber}': max {max} subscriptions"
    )]
    CapExceeded {
        /// The subscriber identity that exceeded the cap.
        subscriber: String,
        /// The configured cap.
        max: usize,
    },
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn new_ps() -> PubSub {
        PubSub::new()
    }

    // ── subscribe / matching_subscribers ─────────────────────────────────────

    #[test]
    fn test_subscribe_and_match_by_agent_and_event() {
        let mut ps = new_ps();
        ps.subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
            .unwrap();

        let matches = ps.matching_subscribers("arch-ctm", "idle");
        assert_eq!(matches, vec!["team-lead"]);
    }

    #[test]
    fn test_no_match_on_different_event() {
        let mut ps = new_ps();
        ps.subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
            .unwrap();

        let matches = ps.matching_subscribers("arch-ctm", "busy");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_no_match_on_different_agent() {
        let mut ps = new_ps();
        ps.subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
            .unwrap();

        let matches = ps.matching_subscribers("other-agent", "idle");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_wildcard_events_match_any_state() {
        let mut ps = new_ps();
        // Empty events list = wildcard
        ps.subscribe("team-lead", "arch-ctm", vec![]).unwrap();

        for state in &["idle", "busy", "killed", "launching"] {
            let matches = ps.matching_subscribers("arch-ctm", state);
            assert_eq!(matches, vec!["team-lead"], "should match state={state}");
        }
    }

    #[test]
    fn test_fan_out_multiple_subscribers() {
        let mut ps = new_ps();
        ps.subscribe("sub-a", "arch-ctm", vec!["idle".to_string()])
            .unwrap();
        ps.subscribe("sub-b", "arch-ctm", vec!["idle".to_string()])
            .unwrap();
        ps.subscribe("sub-c", "arch-ctm", vec!["busy".to_string()])
            .unwrap();

        let mut idle_matches = ps.matching_subscribers("arch-ctm", "idle");
        idle_matches.sort();
        assert_eq!(idle_matches, vec!["sub-a", "sub-b"]);

        let busy_matches = ps.matching_subscribers("arch-ctm", "busy");
        assert_eq!(busy_matches, vec!["sub-c"]);
    }

    // ── upsert semantics ──────────────────────────────────────────────────────

    #[test]
    fn test_upsert_refreshes_ttl_and_events() {
        let mut ps = PubSub::with_config(Duration::from_secs(1), 10);
        ps.subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
            .unwrap();
        // Re-subscribe with different events — should update, not add duplicate
        ps.subscribe("team-lead", "arch-ctm", vec!["killed".to_string()])
            .unwrap();

        assert_eq!(ps.len(), 1, "upsert must not create duplicate");

        // Now the subscription should respond to "killed" not "idle"
        let matches = ps.matching_subscribers("arch-ctm", "killed");
        assert_eq!(matches, vec!["team-lead"]);

        let old_matches = ps.matching_subscribers("arch-ctm", "idle");
        assert!(old_matches.is_empty());
    }

    #[test]
    fn test_upsert_does_not_count_against_cap() {
        // Cap = 1; first subscribe succeeds, re-subscribe (upsert) must also succeed
        let mut ps = PubSub::with_config(Duration::from_secs(3600), 1);
        ps.subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
            .unwrap();
        // This is an upsert, not a new entry — must not fail
        ps.subscribe("team-lead", "arch-ctm", vec!["busy".to_string()])
            .unwrap();
        assert_eq!(ps.len(), 1);
    }

    // ── TTL / GC ──────────────────────────────────────────────────────────────

    #[test]
    fn test_expired_subscriptions_not_returned() {
        let mut ps = PubSub::with_config(Duration::from_nanos(1), 10);
        ps.subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
            .unwrap();
        // The subscription's TTL (1 ns) has definitely elapsed by now
        let matches = ps.matching_subscribers("arch-ctm", "idle");
        assert!(
            matches.is_empty(),
            "expired subscriptions must not be returned"
        );
    }

    #[test]
    fn test_gc_removes_expired_entries() {
        let mut ps = PubSub::with_config(Duration::from_nanos(1), 10);
        ps.subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
            .unwrap();

        assert_eq!(ps.len(), 1);
        let removed = ps.gc();
        assert_eq!(removed, 1);
        assert!(ps.is_empty());
    }

    #[test]
    fn test_gc_keeps_valid_entries() {
        let mut ps = new_ps();
        ps.subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
            .unwrap();

        let removed = ps.gc();
        assert_eq!(removed, 0);
        assert_eq!(ps.len(), 1);
    }

    // ── cap enforcement ───────────────────────────────────────────────────────

    #[test]
    fn test_cap_exceeded_returns_error() {
        let mut ps = PubSub::with_config(Duration::from_secs(3600), 2);
        ps.subscribe("team-lead", "agent-a", vec!["idle".to_string()])
            .unwrap();
        ps.subscribe("team-lead", "agent-b", vec!["idle".to_string()])
            .unwrap();

        // Third subscription for same subscriber should fail
        let result = ps.subscribe("team-lead", "agent-c", vec!["idle".to_string()]);
        assert!(result.is_err());
        match result.unwrap_err() {
            PubSubError::CapExceeded { subscriber, max } => {
                assert_eq!(subscriber, "team-lead");
                assert_eq!(max, 2);
            }
        }
    }

    #[test]
    fn test_cap_is_per_subscriber() {
        let mut ps = PubSub::with_config(Duration::from_secs(3600), 1);
        ps.subscribe("sub-a", "agent-x", vec!["idle".to_string()])
            .unwrap();
        // Different subscriber can still subscribe
        ps.subscribe("sub-b", "agent-x", vec!["idle".to_string()])
            .unwrap();
        assert_eq!(ps.len(), 2);
    }

    // ── unsubscribe ───────────────────────────────────────────────────────────

    #[test]
    fn test_unsubscribe_removes_specific_entry() {
        let mut ps = new_ps();
        ps.subscribe("team-lead", "arch-ctm", vec!["idle".to_string()])
            .unwrap();
        ps.subscribe("team-lead", "worker-1", vec!["idle".to_string()])
            .unwrap();

        ps.unsubscribe("team-lead", "arch-ctm");

        assert_eq!(ps.len(), 1);
        let matches = ps.matching_subscribers("arch-ctm", "idle");
        assert!(matches.is_empty());
        let other = ps.matching_subscribers("worker-1", "idle");
        assert_eq!(other, vec!["team-lead"]);
    }

    #[test]
    fn test_unsubscribe_nonexistent_is_noop() {
        let mut ps = new_ps();
        // Should not panic
        ps.unsubscribe("nobody", "ghost");
        assert!(ps.is_empty());
    }

    #[test]
    fn test_unsubscribe_all_removes_all_for_subscriber() {
        let mut ps = new_ps();
        ps.subscribe("team-lead", "agent-a", vec!["idle".to_string()])
            .unwrap();
        ps.subscribe("team-lead", "agent-b", vec!["idle".to_string()])
            .unwrap();
        ps.subscribe("other", "agent-a", vec!["idle".to_string()])
            .unwrap();

        ps.unsubscribe_all("team-lead");

        assert_eq!(ps.len(), 1);
        let other_matches = ps.matching_subscribers("agent-a", "idle");
        assert_eq!(other_matches, vec!["other"]);
    }

    // ── count / len / is_empty ────────────────────────────────────────────────

    #[test]
    fn test_count_for_subscriber() {
        let mut ps = new_ps();
        ps.subscribe("team-lead", "agent-a", vec!["idle".to_string()])
            .unwrap();
        ps.subscribe("team-lead", "agent-b", vec!["idle".to_string()])
            .unwrap();
        ps.subscribe("other", "agent-a", vec!["idle".to_string()])
            .unwrap();

        assert_eq!(ps.count_for_subscriber("team-lead"), 2);
        assert_eq!(ps.count_for_subscriber("other"), 1);
        assert_eq!(ps.count_for_subscriber("nobody"), 0);
    }

    #[test]
    fn test_is_empty_and_len() {
        let mut ps = new_ps();
        assert!(ps.is_empty());
        assert_eq!(ps.len(), 0);

        ps.subscribe("sub", "agent", vec!["idle".to_string()]).unwrap();
        assert!(!ps.is_empty());
        assert_eq!(ps.len(), 1);
    }

    // ── multiple events in filter ─────────────────────────────────────────────

    #[test]
    fn test_multiple_events_in_filter() {
        let mut ps = new_ps();
        ps.subscribe(
            "team-lead",
            "arch-ctm",
            vec!["idle".to_string(), "killed".to_string()],
        )
        .unwrap();

        assert!(!ps.matching_subscribers("arch-ctm", "idle").is_empty());
        assert!(!ps.matching_subscribers("arch-ctm", "killed").is_empty());
        assert!(ps.matching_subscribers("arch-ctm", "busy").is_empty());
        assert!(ps.matching_subscribers("arch-ctm", "launching").is_empty());
    }
}
