//! Request deduplication store for control receiver operations.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Default max in-memory dedupe keys when `ATM_DEDUP_CAPACITY` is unset.
const DEFAULT_CAPACITY: usize = 1000;
/// Default dedupe retention window in seconds when `ATM_DEDUP_TTL_SECS` is unset.
const DEFAULT_TTL_SECS: u64 = 600;

/// Composite idempotency key for control requests.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DedupeKey {
    pub team: String,
    pub session_id: String,
    pub agent_id: String,
    pub request_id: String,
}

impl DedupeKey {
    pub fn new(team: &str, session_id: &str, agent_id: &str, request_id: &str) -> Self {
        Self {
            team: team.to_string(),
            session_id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            request_id: request_id.to_string(),
        }
    }
}

/// In-memory dedupe store with bounded capacity and TTL expiry.
#[derive(Debug)]
pub struct DedupeStore {
    entries: HashMap<DedupeKey, Instant>,
    order: VecDeque<DedupeKey>,
    ttl: Duration,
    capacity: usize,
}

impl DedupeStore {
    pub fn from_env() -> Self {
        let capacity = std::env::var("ATM_DEDUP_CAPACITY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_CAPACITY);
        let ttl_secs = std::env::var("ATM_DEDUP_TTL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_TTL_SECS);

        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            ttl: Duration::from_secs(ttl_secs),
            capacity,
        }
    }

    #[cfg(test)]
    fn with_config(capacity: usize, ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            ttl,
            capacity,
        }
    }

    /// Returns `true` when key already exists and has not expired.
    pub fn check_and_insert(&mut self, key: DedupeKey) -> bool {
        let now = Instant::now();
        self.purge_expired(now);
        if self.entries.contains_key(&key) {
            return true;
        }
        self.entries.insert(key.clone(), now);
        self.order.push_back(key);
        self.evict_to_capacity();
        false
    }

    /// Purge expired keys based on the configured TTL.
    pub fn cleanup_expired(&mut self) {
        self.purge_expired(Instant::now());
    }

    #[cfg(test)]
    fn check_and_insert_at(&mut self, key: DedupeKey, now: Instant) -> bool {
        self.purge_expired(now);
        if self.entries.contains_key(&key) {
            return true;
        }
        self.entries.insert(key.clone(), now);
        self.order.push_back(key);
        self.evict_to_capacity();
        false
    }

    fn purge_expired(&mut self, now: Instant) {
        while let Some(front_key) = self.order.front().cloned() {
            let expired = self
                .entries
                .get(&front_key)
                .map(|ts| now.saturating_duration_since(*ts) >= self.ttl)
                .unwrap_or(true);
            if !expired {
                break;
            }
            self.order.pop_front();
            self.entries.remove(&front_key);
        }
    }

    fn evict_to_capacity(&mut self) {
        while self.entries.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(i: usize) -> DedupeKey {
        DedupeKey::new("atm-dev", "sess-1", "arch-ctm", &format!("req-{i}"))
    }

    #[test]
    fn insert_then_duplicate() {
        let mut d = DedupeStore::with_config(4, Duration::from_secs(600));
        assert!(!d.check_and_insert(key(1)));
        assert!(d.check_and_insert(key(1)));
    }

    #[test]
    fn ttl_expiry_allows_reinsert() {
        let mut d = DedupeStore::with_config(4, Duration::from_secs(10));
        let t0 = Instant::now();
        let k = key(1);
        assert!(!d.check_and_insert_at(k.clone(), t0));
        assert!(d.check_and_insert_at(k.clone(), t0 + Duration::from_secs(1)));
        assert!(!d.check_and_insert_at(k, t0 + Duration::from_secs(11)));
    }

    #[test]
    fn capacity_eviction_discards_oldest() {
        let mut d = DedupeStore::with_config(2, Duration::from_secs(600));
        assert!(!d.check_and_insert(key(1)));
        assert!(!d.check_and_insert(key(2)));
        assert!(!d.check_and_insert(key(3))); // evicts key(1)
        assert!(!d.check_and_insert(key(1))); // no longer duplicate
    }

    #[test]
    fn dedupe_key_isolated_by_team() {
        let mut d = DedupeStore::with_config(4, Duration::from_secs(600));
        let k1 = DedupeKey::new("atm-dev", "sess-1", "arch-ctm", "req-iso");
        let k2 = DedupeKey::new("other-team", "sess-1", "arch-ctm", "req-iso");
        assert!(!d.check_and_insert(k1));
        assert!(!d.check_and_insert(k2));
    }

    #[test]
    fn dedupe_key_isolated_by_session_id() {
        let mut d = DedupeStore::with_config(4, Duration::from_secs(600));
        let k1 = DedupeKey::new("atm-dev", "sess-1", "arch-ctm", "req-iso");
        let k2 = DedupeKey::new("atm-dev", "sess-2", "arch-ctm", "req-iso");
        assert!(!d.check_and_insert(k1));
        assert!(!d.check_and_insert(k2));
    }

    #[test]
    fn dedupe_key_isolated_by_agent_id() {
        let mut d = DedupeStore::with_config(4, Duration::from_secs(600));
        let k1 = DedupeKey::new("atm-dev", "sess-1", "arch-ctm", "req-iso");
        let k2 = DedupeKey::new("atm-dev", "sess-1", "sm-b-1", "req-iso");
        assert!(!d.check_and_insert(k1));
        assert!(!d.check_and_insert(k2));
    }
}
