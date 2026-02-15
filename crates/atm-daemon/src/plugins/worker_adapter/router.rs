//! Message routing logic for worker adapter
//!
//! Routes inbox messages to configured worker agents with concurrency control.

use crate::plugin::PluginError;
use atm_core::schema::InboxMessage;
use std::collections::{HashMap, VecDeque};
use tracing::{debug, warn};

/// Concurrency policy for handling multiple messages to the same agent
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConcurrencyPolicy {
    /// Queue incoming messages (default) — only one message processed at a time
    #[default]
    Queue,
    /// Reject new messages if agent is busy
    Reject,
    /// Allow concurrent message processing
    Concurrent,
}

/// Message router with concurrency control
pub struct MessageRouter {
    /// Per-agent message queues
    queues: HashMap<String, VecDeque<InboxMessage>>,
    /// Per-agent busy status
    busy_agents: HashMap<String, bool>,
    /// Per-agent concurrency policy
    policies: HashMap<String, ConcurrencyPolicy>,
}

impl MessageRouter {
    /// Create a new message router
    pub fn new() -> Self {
        Self {
            queues: HashMap::new(),
            busy_agents: HashMap::new(),
            policies: HashMap::new(),
        }
    }

    /// Configure concurrency policy for an agent
    ///
    /// # Arguments
    ///
    /// * `agent_name` - Name of the agent
    /// * `policy` - Concurrency policy to apply
    pub fn set_policy(&mut self, agent_name: String, policy: ConcurrencyPolicy) {
        self.policies.insert(agent_name, policy);
    }

    /// Attempt to route a message to an agent
    ///
    /// Returns `Ok(Some(message))` if the message can be delivered now,
    /// `Ok(None)` if queued or rejected, `Err` if routing failed.
    ///
    /// # Arguments
    ///
    /// * `agent_name` - Target agent name
    /// * `message` - Inbox message to route
    ///
    /// # Errors
    ///
    /// Returns `PluginError` if the agent's policy is Reject and agent is busy
    pub fn route_message(
        &mut self,
        agent_name: &str,
        message: InboxMessage,
    ) -> Result<Option<InboxMessage>, PluginError> {
        let policy = self
            .policies
            .get(agent_name)
            .copied()
            .unwrap_or_default();

        let is_busy = self.busy_agents.get(agent_name).copied().unwrap_or(false);

        match policy {
            ConcurrencyPolicy::Concurrent => {
                // Always deliver immediately
                debug!("Routing message to {agent_name} (concurrent policy)");
                Ok(Some(message))
            }
            ConcurrencyPolicy::Queue => {
                if is_busy {
                    // Agent is busy, queue the message
                    debug!("Queueing message for {agent_name} (agent busy)");
                    self.queues
                        .entry(agent_name.to_string())
                        .or_default()
                        .push_back(message);
                    Ok(None)
                } else {
                    // Agent is idle, deliver immediately
                    debug!("Routing message to {agent_name} (queue policy, agent idle)");
                    self.busy_agents.insert(agent_name.to_string(), true);
                    Ok(Some(message))
                }
            }
            ConcurrencyPolicy::Reject => {
                if is_busy {
                    // Agent is busy, reject the message
                    warn!("Rejecting message for {agent_name} (agent busy, reject policy)");
                    Err(PluginError::Runtime {
                        message: format!("Agent {agent_name} is busy (reject policy)"),
                        source: None,
                    })
                } else {
                    // Agent is idle, deliver immediately
                    debug!("Routing message to {agent_name} (reject policy, agent idle)");
                    self.busy_agents.insert(agent_name.to_string(), true);
                    Ok(Some(message))
                }
            }
        }
    }

    /// Mark agent as no longer busy and dequeue next message if available
    ///
    /// Returns the next queued message for the agent, if any.
    ///
    /// # Arguments
    ///
    /// * `agent_name` - Name of the agent that finished processing
    pub fn agent_finished(&mut self, agent_name: &str) -> Option<InboxMessage> {
        self.busy_agents.insert(agent_name.to_string(), false);

        // Check if there are queued messages
        if let Some(queue) = self.queues.get_mut(agent_name)
            && let Some(next_message) = queue.pop_front()
        {
            debug!("Dequeuing next message for {agent_name}");
            self.busy_agents.insert(agent_name.to_string(), true);
            Some(next_message)
        } else {
            None
        }
    }

    /// Get the number of queued messages for an agent
    ///
    /// # Arguments
    ///
    /// * `agent_name` - Name of the agent
    pub fn queue_depth(&self, agent_name: &str) -> usize {
        self.queues
            .get(agent_name)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    /// Check if an agent is currently busy
    ///
    /// # Arguments
    ///
    /// * `agent_name` - Name of the agent
    pub fn is_busy(&self, agent_name: &str) -> bool {
        self.busy_agents.get(agent_name).copied().unwrap_or(false)
    }
}

impl Default for MessageRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_test_message(from: &str, text: &str) -> InboxMessage {
        InboxMessage {
            from: from.to_string(),
            text: text.to_string(),
            timestamp: "2026-02-14T00:00:00Z".to_string(),
            read: false,
            summary: None,
            message_id: None,
            unknown_fields: HashMap::new(),
        }
    }

    #[test]
    fn test_concurrent_policy() {
        let mut router = MessageRouter::new();
        router.set_policy("agent1".to_string(), ConcurrencyPolicy::Concurrent);

        let msg1 = make_test_message("sender", "message 1");
        let msg2 = make_test_message("sender", "message 2");

        // Both messages should be delivered immediately
        let result1 = router.route_message("agent1", msg1.clone()).unwrap();
        assert!(result1.is_some());

        let result2 = router.route_message("agent1", msg2.clone()).unwrap();
        assert!(result2.is_some());

        assert_eq!(router.queue_depth("agent1"), 0);
    }

    #[test]
    fn test_queue_policy() {
        let mut router = MessageRouter::new();
        router.set_policy("agent1".to_string(), ConcurrencyPolicy::Queue);

        let msg1 = make_test_message("sender", "message 1");
        let msg2 = make_test_message("sender", "message 2");
        let msg3 = make_test_message("sender", "message 3");

        // First message should be delivered
        let result1 = router.route_message("agent1", msg1.clone()).unwrap();
        assert!(result1.is_some());
        assert!(router.is_busy("agent1"));

        // Second and third messages should be queued
        let result2 = router.route_message("agent1", msg2.clone()).unwrap();
        assert!(result2.is_none());
        assert_eq!(router.queue_depth("agent1"), 1);

        let result3 = router.route_message("agent1", msg3.clone()).unwrap();
        assert!(result3.is_none());
        assert_eq!(router.queue_depth("agent1"), 2);

        // Mark agent as finished, should dequeue msg2
        let next = router.agent_finished("agent1");
        assert!(next.is_some());
        assert_eq!(next.unwrap().text, "message 2");
        assert_eq!(router.queue_depth("agent1"), 1);
        assert!(router.is_busy("agent1"));

        // Mark agent as finished again, should dequeue msg3
        let next = router.agent_finished("agent1");
        assert!(next.is_some());
        assert_eq!(next.unwrap().text, "message 3");
        assert_eq!(router.queue_depth("agent1"), 0);
        assert!(router.is_busy("agent1"));

        // No more messages
        let next = router.agent_finished("agent1");
        assert!(next.is_none());
        assert!(!router.is_busy("agent1"));
    }

    #[test]
    fn test_reject_policy() {
        let mut router = MessageRouter::new();
        router.set_policy("agent1".to_string(), ConcurrencyPolicy::Reject);

        let msg1 = make_test_message("sender", "message 1");
        let msg2 = make_test_message("sender", "message 2");

        // First message should be delivered
        let result1 = router.route_message("agent1", msg1.clone()).unwrap();
        assert!(result1.is_some());
        assert!(router.is_busy("agent1"));

        // Second message should be rejected
        let result2 = router.route_message("agent1", msg2.clone());
        assert!(result2.is_err());
        assert_eq!(router.queue_depth("agent1"), 0);

        // After agent finishes, new message should be accepted
        let next = router.agent_finished("agent1");
        assert!(next.is_none());
        assert!(!router.is_busy("agent1"));

        let msg3 = make_test_message("sender", "message 3");
        let result3 = router.route_message("agent1", msg3.clone()).unwrap();
        assert!(result3.is_some());
    }

    #[test]
    fn test_default_policy_is_queue() {
        let mut router = MessageRouter::new();

        let msg1 = make_test_message("sender", "message 1");
        let msg2 = make_test_message("sender", "message 2");

        // First message should be delivered
        let result1 = router.route_message("agent1", msg1.clone()).unwrap();
        assert!(result1.is_some());

        // Second message should be queued (default policy)
        let result2 = router.route_message("agent1", msg2.clone()).unwrap();
        assert!(result2.is_none());
        assert_eq!(router.queue_depth("agent1"), 1);
    }
}
