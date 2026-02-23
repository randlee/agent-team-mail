//! In-process watch stream hub for direct `atm-agent-mcp -> atm-tui` session viewing.
//!
//! This module provides foundational plumbing for Sprint L.5:
//! - per-agent replay buffer (bounded ring, default configured by caller)
//! - live event fanout via `tokio::sync::broadcast`
//! - attach semantics: caller receives replay snapshot + live receiver
//!
//! The hub is intentionally transport-agnostic and stores raw JSON payloads.

use std::collections::{HashMap, VecDeque};

use serde_json::{Value, json};
use tokio::sync::broadcast;

/// Default replay size for active-session watch attach.
pub const DEFAULT_REPLAY_CAPACITY: usize = 50;

/// Result of attaching a watcher to an agent stream.
#[derive(Debug)]
pub struct WatchSubscription {
    /// Bounded replay snapshot (oldest to newest) captured at attach time.
    pub replay: Vec<Value>,
    /// Live stream receiver for subsequent published events.
    pub rx: broadcast::Receiver<Value>,
}

/// Source envelope for published watch frames (FR-22).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEnvelope {
    pub kind: String,
    pub actor: String,
    pub channel: String,
}

impl SourceEnvelope {
    pub fn new(
        kind: impl Into<String>,
        actor: impl Into<String>,
        channel: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            actor: actor.into(),
            channel: channel.into(),
        }
    }
}

#[derive(Debug)]
struct AgentWatchState {
    replay: VecDeque<Value>,
    tx: broadcast::Sender<Value>,
}

impl AgentWatchState {
    fn new(replay_capacity: usize) -> Self {
        // 128 is enough for bursty live deltas while keeping memory bounded.
        let (tx, _rx) = broadcast::channel(128);
        Self {
            replay: VecDeque::with_capacity(replay_capacity.max(1)),
            tx,
        }
    }

    fn push_replay(&mut self, replay_capacity: usize, event: Value) {
        if self.replay.len() >= replay_capacity.max(1) {
            let _ = self.replay.pop_front();
        }
        self.replay.push_back(event);
    }
}

/// Per-proxy hub for direct watch stream fanout.
#[derive(Debug)]
pub struct WatchStreamHub {
    replay_capacity: usize,
    by_agent: HashMap<String, AgentWatchState>,
}

impl WatchStreamHub {
    /// Create a new hub with the given replay capacity.
    pub fn new(replay_capacity: usize) -> Self {
        Self {
            replay_capacity: replay_capacity.max(1),
            by_agent: HashMap::new(),
        }
    }

    /// Publish one event for an agent.
    ///
    /// The event is appended to replay and broadcast to live receivers.
    /// Broadcast errors (no receivers or lag) are intentionally ignored.
    pub fn publish(&mut self, agent_id: &str, event: Value) {
        let state = self
            .by_agent
            .entry(agent_id.to_string())
            .or_insert_with(|| AgentWatchState::new(self.replay_capacity));

        state.push_replay(self.replay_capacity, event.clone());
        // Optimization: skip fanout work when there are no active receivers.
        if state.tx.receiver_count() > 0 {
            let _ = state.tx.send(event);
        }
    }

    /// Publish one event wrapped with source attribution.
    pub fn publish_frame(&mut self, agent_id: &str, source: SourceEnvelope, event: Value) {
        let frame = build_watch_frame(agent_id, &source, event);
        self.publish(agent_id, frame);
    }

    /// Attach a watcher for an agent stream.
    ///
    /// Returns a replay snapshot plus a live broadcast receiver.
    pub fn subscribe(&mut self, agent_id: &str) -> WatchSubscription {
        let state = self
            .by_agent
            .entry(agent_id.to_string())
            .or_insert_with(|| AgentWatchState::new(self.replay_capacity));
        WatchSubscription {
            replay: state.replay.iter().cloned().collect(),
            rx: state.tx.subscribe(),
        }
    }

    /// Number of currently attached live watchers for one agent.
    pub fn watcher_count(&mut self, agent_id: &str) -> usize {
        self.by_agent
            .get(agent_id)
            .map(|state| state.tx.receiver_count())
            .unwrap_or(0)
    }

    /// Detach bookkeeping for compatibility with existing call sites.
    ///
    /// In multi-watcher mode subscriptions are detached by dropping the
    /// corresponding receiver. Once the receiver count reaches zero, the
    /// per-agent state is evicted (replay buffer + channel).
    pub fn detach(&mut self, agent_id: &str) -> bool {
        let Some(receiver_count) = self.by_agent.get(agent_id).map(|s| s.tx.receiver_count())
        else {
            return false;
        };
        if receiver_count == 0 {
            self.by_agent.remove(agent_id);
            return true;
        }
        false
    }
}

/// Build a structured watch frame shared by live fanout and direct TUI feed.
pub fn build_watch_frame(agent_id: &str, source: &SourceEnvelope, event: Value) -> Value {
    json!({
        "agent_id": agent_id,
        "source": {
            "kind": source.kind,
            "actor": source.actor,
            "channel": source.channel,
        },
        "event": event,
    })
}

impl Default for WatchStreamHub {
    fn default() -> Self {
        Self::new(DEFAULT_REPLAY_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replay_buffer_is_bounded_oldest_first() {
        let mut hub = WatchStreamHub::new(3);
        hub.publish("a1", json!({"n": 1}));
        hub.publish("a1", json!({"n": 2}));
        hub.publish("a1", json!({"n": 3}));
        hub.publish("a1", json!({"n": 4}));

        let sub = hub.subscribe("a1");
        let nums: Vec<i64> = sub
            .replay
            .into_iter()
            .filter_map(|v| v.get("n").and_then(|n| n.as_i64()))
            .collect();
        assert_eq!(nums, vec![2, 3, 4]);
    }

    #[tokio::test]
    async fn subscribe_gets_replay_and_live() {
        let mut hub = WatchStreamHub::new(2);
        hub.publish("a1", json!({"n": 1}));
        hub.publish("a1", json!({"n": 2}));
        let mut sub = hub.subscribe("a1");
        assert_eq!(sub.replay.len(), 2);

        hub.publish("a1", json!({"n": 3}));
        let live = sub.rx.recv().await.expect("live event");
        assert_eq!(live.get("n").and_then(|n| n.as_i64()), Some(3));
    }

    #[test]
    fn supports_multi_watcher_fanout() {
        let mut hub = WatchStreamHub::default();
        let mut first = hub.subscribe("a1");
        let mut second = hub.subscribe("a1");
        assert_eq!(hub.watcher_count("a1"), 2);
        hub.publish("a1", json!({"n": 1}));
        let live1 = first.rx.try_recv().expect("first receives");
        let live2 = second.rx.try_recv().expect("second receives");
        assert_eq!(live1["n"], 1);
        assert_eq!(live2["n"], 1);
    }

    #[test]
    fn detach_evicts_state_when_no_receivers() {
        let mut hub = WatchStreamHub::default();
        hub.publish("a1", json!({"n": 1}));
        // No active watchers yet; detach should evict buffered state.
        assert!(hub.detach("a1"));
        assert_eq!(hub.watcher_count("a1"), 0);
        let sub = hub.subscribe("a1");
        assert!(
            sub.replay.is_empty(),
            "evicted state must not retain prior replay entries"
        );
    }

    #[tokio::test]
    async fn publish_frame_wraps_source_envelope() {
        let mut hub = WatchStreamHub::default();
        let mut sub = hub.subscribe("a1");
        hub.publish_frame(
            "a1",
            SourceEnvelope::new("client_prompt", "arch-atm", "mcp_primary"),
            json!({"type":"agent_message_delta"}),
        );
        let live = sub.rx.recv().await.expect("live frame");
        assert_eq!(live.get("agent_id").and_then(|v| v.as_str()), Some("a1"));
        assert_eq!(
            live.pointer("/source/kind").and_then(|v| v.as_str()),
            Some("client_prompt")
        );
        assert_eq!(
            live.pointer("/source/actor").and_then(|v| v.as_str()),
            Some("arch-atm")
        );
        assert_eq!(
            live.pointer("/source/channel").and_then(|v| v.as_str()),
            Some("mcp_primary")
        );
    }
}
