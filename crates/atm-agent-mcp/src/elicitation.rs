//! Elicitation/approval request bridging (FR-18).
//!
//! When the Codex child sends an `elicitation/create` request (server-initiated),
//! the proxy bridges it upstream to Claude with correlation tracking.
//!
//! [`ElicitationRegistry`] maps upstream request IDs to pending [`PendingElicitation`]
//! entries so that responses received from upstream can be correlated back to the
//! correct child request and forwarded downstream.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

/// A single pending elicitation waiting for an upstream (Claude) response.
pub struct PendingElicitation {
    /// The agent_id whose session triggered this elicitation.
    pub agent_id: String,
    /// The downstream request_id as sent by the Codex child.
    pub downstream_request_id: serde_json::Value,
    /// The upstream request_id assigned by the proxy (used as map key).
    pub upstream_request_id: serde_json::Value,
    /// When the elicitation was created (for timeout tracking).
    pub created_at: Instant,
    /// Timeout duration for this elicitation.
    pub timeout: Duration,
    /// Channel to deliver the downstream response back to the child handler.
    pub response_tx: oneshot::Sender<serde_json::Value>,
}

impl std::fmt::Debug for PendingElicitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingElicitation")
            .field("agent_id", &self.agent_id)
            .field("downstream_request_id", &self.downstream_request_id)
            .field("upstream_request_id", &self.upstream_request_id)
            .finish()
    }
}

/// Registry of pending elicitations keyed by `upstream_request_id.to_string()`.
///
/// Wrap in `Arc<tokio::sync::Mutex<ElicitationRegistry>>` when sharing across
/// async tasks.
///
/// # Examples
///
/// ```
/// use atm_agent_mcp::elicitation::ElicitationRegistry;
/// use tokio::sync::oneshot;
///
/// let mut reg = ElicitationRegistry::new(30);
/// let (tx, _rx) = oneshot::channel();
/// reg.register(
///     "agent-1".to_string(),
///     serde_json::json!(1),
///     serde_json::json!(100),
///     tx,
/// );
/// assert!(reg.resolve(&serde_json::json!(100), serde_json::json!({"ok": true})));
/// ```
#[derive(Debug)]
pub struct ElicitationRegistry {
    /// Map from `upstream_request_id.to_string()` → pending entry.
    pending: HashMap<String, PendingElicitation>,
    /// Default timeout applied to new registrations.
    default_timeout: Duration,
}

impl ElicitationRegistry {
    /// Create a new registry with the given default timeout in seconds.
    pub fn new(default_timeout_secs: u64) -> Self {
        Self {
            pending: HashMap::new(),
            default_timeout: Duration::from_secs(default_timeout_secs),
        }
    }

    /// Register a new pending elicitation.
    ///
    /// `upstream_request_id` is used as the lookup key when the upstream
    /// response arrives.
    pub fn register(
        &mut self,
        agent_id: String,
        downstream_request_id: serde_json::Value,
        upstream_request_id: serde_json::Value,
        response_tx: oneshot::Sender<serde_json::Value>,
    ) {
        let key = upstream_request_id.to_string();
        self.pending.insert(
            key,
            PendingElicitation {
                agent_id,
                downstream_request_id,
                upstream_request_id,
                created_at: Instant::now(),
                timeout: self.default_timeout,
                response_tx,
            },
        );
    }

    /// Resolve a pending elicitation by its upstream request ID.
    ///
    /// Sends `response` to the waiting child handler and removes the entry from
    /// the registry.
    ///
    /// Returns `true` if an entry was found and resolved, `false` otherwise.
    pub fn resolve(&mut self, upstream_request_id: &serde_json::Value, response: serde_json::Value) -> bool {
        let key = upstream_request_id.to_string();
        if let Some(entry) = self.pending.remove(&key) {
            // Best-effort send — if the receiver was dropped, ignore the error.
            let _ = entry.response_tx.send(response);
            true
        } else {
            false
        }
    }

    /// Resolve a pending elicitation and rewrite `response.id` back to the
    /// original downstream request ID for delivery to the child process.
    ///
    /// Returns `Some(rewritten_response)` when a pending elicitation was
    /// found, otherwise `None`.
    pub fn resolve_for_downstream(
        &mut self,
        upstream_request_id: &serde_json::Value,
        mut response: serde_json::Value,
    ) -> Option<serde_json::Value> {
        let key = upstream_request_id.to_string();
        let entry = self.pending.remove(&key)?;

        if let Some(id_field) = response.get_mut("id") {
            *id_field = entry.downstream_request_id.clone();
        }

        // Keep existing semantics for callers relying on resolve() side effects.
        // Ignore send failures if no receiver is waiting.
        let _ = entry.response_tx.send(response.clone());
        Some(response)
    }

    /// Cancel all pending elicitations for the given agent, sending `rejection_result`
    /// to each waiting channel.
    ///
    /// Called when a session is closed (FR-18.5) to unblock any awaiting elicitation
    /// futures.
    pub fn cancel_for_agent(&mut self, agent_id: &str, rejection_result: serde_json::Value) {
        let keys_to_remove: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, e)| e.agent_id == agent_id)
            .map(|(k, _)| k.clone())
            .collect();

        for key in keys_to_remove {
            if let Some(entry) = self.pending.remove(&key) {
                let _ = entry.response_tx.send(rejection_result.clone());
            }
        }
    }

    /// Remove and reject all entries whose `created_at + timeout` has elapsed.
    ///
    /// Returns the upstream request IDs (as strings) of timed-out entries for
    /// logging. Each timed-out entry receives a rejection payload:
    ///
    /// ```json
    /// {"result": null, "error": {"code": -32006, "message": "elicitation timeout"}}
    /// ```
    ///
    /// Error code `-32006` maps to `REQUEST_TIMEOUT` per NFR-6.
    pub fn expire_timeouts(&mut self) -> Vec<String> {
        let now = Instant::now();
        let timeout_rejection = serde_json::json!({
            "result": null,
            "error": {
                "code": -32006,
                "message": "elicitation timeout"
            }
        });

        let expired_keys: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, e)| now.duration_since(e.created_at) >= e.timeout)
            .map(|(k, _)| k.clone())
            .collect();

        for key in &expired_keys {
            if let Some(entry) = self.pending.remove(key) {
                let _ = entry.response_tx.send(timeout_rejection.clone());
            }
        }

        expired_keys
    }

    /// Number of pending elicitations currently tracked.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Returns `true` when no elicitations are pending.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    fn make_reg(timeout_secs: u64) -> ElicitationRegistry {
        ElicitationRegistry::new(timeout_secs)
    }

    // ─── register + resolve ──────────────────────────────────────────────────

    #[tokio::test]
    async fn register_and_resolve_returns_true_and_delivers_response() {
        let mut reg = make_reg(30);
        let (tx, mut rx) = oneshot::channel::<serde_json::Value>();

        reg.register(
            "agent-1".to_string(),
            serde_json::json!(1),
            serde_json::json!(100),
            tx,
        );

        assert_eq!(reg.len(), 1);

        let response = serde_json::json!({"result": "ok"});
        let found = reg.resolve(&serde_json::json!(100), response.clone());
        assert!(found, "resolve must return true for a registered ID");
        assert_eq!(reg.len(), 0, "entry must be removed after resolve");

        // The response_tx must have received the value
        let received = rx.try_recv().expect("response must be delivered");
        assert_eq!(received, response);
    }

    // ─── resolve unknown ID returns false ────────────────────────────────────

    #[test]
    fn resolve_unknown_id_returns_false() {
        let mut reg = make_reg(30);
        let found = reg.resolve(&serde_json::json!(999), serde_json::json!({}));
        assert!(!found, "resolve must return false for unknown ID");
    }

    // ─── cancel_for_agent ────────────────────────────────────────────────────

    #[tokio::test]
    async fn cancel_for_agent_sends_rejection_and_removes_entries() {
        let mut reg = make_reg(30);

        let (tx1, mut rx1) = oneshot::channel::<serde_json::Value>();
        let (tx2, mut rx2) = oneshot::channel::<serde_json::Value>();
        let (tx3, mut rx3) = oneshot::channel::<serde_json::Value>();

        reg.register("agent-a".to_string(), serde_json::json!(1), serde_json::json!(101), tx1);
        reg.register("agent-a".to_string(), serde_json::json!(2), serde_json::json!(102), tx2);
        reg.register("agent-b".to_string(), serde_json::json!(3), serde_json::json!(103), tx3);

        let rejection = serde_json::json!({"error": "cancelled"});
        reg.cancel_for_agent("agent-a", rejection.clone());

        // agent-a entries must be removed
        assert_eq!(reg.len(), 1, "only agent-b entry should remain");

        // rx1 and rx2 must have received the rejection
        let r1 = rx1.try_recv().expect("rx1 must have received rejection");
        let r2 = rx2.try_recv().expect("rx2 must have received rejection");
        assert_eq!(r1, rejection);
        assert_eq!(r2, rejection);

        // agent-b entry must still be present and untouched
        assert!(rx3.try_recv().is_err(), "rx3 must not have received anything");
    }

    // ─── expire_timeouts ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn expire_timeouts_removes_and_rejects_timed_out_entries() {
        // Use a 0-second timeout so entries expire immediately
        let mut reg = make_reg(0);

        let (tx1, mut rx1) = oneshot::channel::<serde_json::Value>();
        let (tx2, mut rx2) = oneshot::channel::<serde_json::Value>();

        reg.register("agent-x".to_string(), serde_json::json!(10), serde_json::json!(200), tx1);
        reg.register("agent-y".to_string(), serde_json::json!(11), serde_json::json!(201), tx2);

        // Sleep briefly to ensure duration_since >= 0s threshold
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;

        let expired = reg.expire_timeouts();
        assert_eq!(expired.len(), 2, "both entries must expire");
        assert!(reg.is_empty(), "registry must be empty after expiry");

        // Both channels must have received the timeout rejection
        let r1 = rx1.try_recv().expect("rx1 must have received timeout rejection");
        let r2 = rx2.try_recv().expect("rx2 must have received timeout rejection");

        assert_eq!(r1["error"]["message"], "elicitation timeout");
        assert_eq!(r2["error"]["message"], "elicitation timeout");
    }

    #[tokio::test]
    async fn expire_timeouts_does_not_expire_fresh_entries() {
        // Very long timeout (30s) — entries should not expire
        let mut reg = make_reg(30);
        let (tx, _rx) = oneshot::channel::<serde_json::Value>();
        reg.register("agent-z".to_string(), serde_json::json!(20), serde_json::json!(300), tx);

        let expired = reg.expire_timeouts();
        assert!(expired.is_empty(), "fresh entries must not expire");
        assert_eq!(reg.len(), 1);
    }
}
