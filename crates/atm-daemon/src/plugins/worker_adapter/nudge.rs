//! Nudge engine for idle Codex agents with unread ATM messages.
//!
//! The nudge engine fires when a Codex agent transitions from `Busy` to `Idle`
//! and has unread messages in its ATM inbox. It sends a human-readable reminder
//! via `tmux send-keys` so the agent knows to run `atm read`.
//!
//! ## Safety
//!
//! - **Only nudges when `AgentState::Idle`** — never during `Busy` or `Launching`
//!   to avoid corrupting in-progress tool responses (sentinel injection risk).
//! - **Per-agent cooldown** (default 30 s) prevents spam between consecutive turns.
//! - **Watermark tracking** (`last_nudged_message_id`) avoids re-nudging the
//!   same unread message repeatedly across multiple idle transitions.
//! - **Max 1 retry** (Enter-only) after 3 s, then gives up until the next
//!   idle transition.
//!
//! ## tmux availability
//!
//! Nudging via `tmux send-keys` is only available on Unix. On Windows the
//! engine is compiled but `should_nudge()` always returns `false` (no tmux),
//! and `send_nudge()` is a no-op. Code paths that call `tmux` are gated with
//! `#[cfg(unix)]` so the Windows build stays clean.
//!
//! ## Integration
//!
//! `NudgeEngine` is owned by `WorkerAdapterPlugin`. The plugin calls
//! `on_idle_transition()` whenever the agent-state tracker records a
//! `Busy → Idle` transition.

use super::agent_state::AgentState;
use super::config::NudgeConfig;
use super::tmux_sender::{DefaultTmuxSender, DeliveryMethod, TmuxSender};
use crate::plugin::PluginError;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// NudgeDecision — result of the pre-nudge eligibility check
// ---------------------------------------------------------------------------

/// Outcome of [`NudgeEngine::should_nudge`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NudgeDecision {
    /// Nudge the agent: includes the formatted nudge text and unread count.
    Nudge {
        /// Formatted nudge text (ready to send via send-keys).
        text: String,
        /// Number of unread messages in the inbox.
        unread_count: usize,
        /// `message_id` of the newest unread message (used as watermark).
        newest_message_id: String,
    },
    /// Skip because the agent is not in `Idle` state.
    SkippedNotIdle,
    /// Skip because the cooldown has not yet expired.
    SkippedCooldown,
    /// Skip because the nudge engine is disabled.
    SkippedDisabled,
    /// Skip because there are no unread messages (or no messages at all).
    SkippedNoUnread,
    /// Skip because the newest unread message was already nudged (watermark match).
    SkippedWatermark,
}

// ---------------------------------------------------------------------------
// NudgeEngine
// ---------------------------------------------------------------------------

/// Automatic nudge engine for idle Codex agents.
///
/// Call [`NudgeEngine::on_idle_transition`] whenever the agent state machine
/// records a `Busy → Idle` (or `Launching → Idle`) transition.
pub struct NudgeEngine {
    /// Configuration (cooldown, template, enabled flag).
    config: NudgeConfig,
    /// Last nudge wall-clock time per agent.
    last_nudge: HashMap<String, Instant>,
    /// The `message_id` of the last message we nudged about, per agent.
    ///
    /// Prevents re-nudging the same unread message if the agent goes idle
    /// multiple times without reading it.
    last_nudged_message_id: HashMap<String, String>,
    /// Shared tmux sender for reliability protections.
    sender: DefaultTmuxSender,
    /// Delivery method for nudges.
    delivery_method: DeliveryMethod,
}

impl NudgeEngine {
    /// Create a new `NudgeEngine` with the given configuration.
    pub fn new(config: NudgeConfig) -> Self {
        Self {
            config,
            last_nudge: HashMap::new(),
            last_nudged_message_id: HashMap::new(),
            sender: DefaultTmuxSender,
            delivery_method: DeliveryMethod::from_env().unwrap_or(DeliveryMethod::PasteBuffer),
        }
    }

    /// Render the nudge text template.
    ///
    /// Replaces `{count}` in `text_template` with the actual unread count.
    pub fn format_nudge_text(&self, unread_count: usize) -> String {
        self.config
            .text_template
            .replace("{count}", &unread_count.to_string())
    }

    /// Determine whether this agent should be nudged right now, and what text to send.
    ///
    /// # Arguments
    ///
    /// * `agent_id` - ATM identity of the agent (e.g. `"arch-ctm"`).
    /// * `current_state` - Current turn-level state of the agent.
    /// * `inbox_messages` - Slice of `(message_id, read)` tuples from the inbox.
    ///   Caller is responsible for loading these from the inbox file.
    pub fn should_nudge(
        &self,
        agent_id: &str,
        current_state: AgentState,
        inbox_messages: &[InboxEntry],
    ) -> NudgeDecision {
        if !self.config.enabled {
            return NudgeDecision::SkippedDisabled;
        }

        if !matches!(current_state, AgentState::Idle) {
            return NudgeDecision::SkippedNotIdle;
        }

        // Cooldown check
        let cooldown = Duration::from_secs(self.config.cooldown_secs);
        if let Some(last) = self.last_nudge.get(agent_id) {
            if last.elapsed() < cooldown {
                debug!(
                    "Nudge cooldown active for {agent_id}: {}s remaining",
                    (cooldown - last.elapsed()).as_secs()
                );
                return NudgeDecision::SkippedCooldown;
            }
        }

        // Collect unread messages
        let unread: Vec<&InboxEntry> = inbox_messages.iter().filter(|e| !e.read).collect();

        if unread.is_empty() {
            return NudgeDecision::SkippedNoUnread;
        }

        // Use the newest unread message_id as the watermark.
        // We take the last entry (most recently appended).
        let newest_id = unread
            .last()
            .and_then(|e| e.message_id.as_deref())
            .unwrap_or("")
            .to_string();

        // Watermark check: skip if we already nudged about this exact message.
        if let Some(last_id) = self.last_nudged_message_id.get(agent_id) {
            if !newest_id.is_empty() && last_id == &newest_id {
                debug!("Watermark match for {agent_id}: already nudged about message {newest_id}");
                return NudgeDecision::SkippedWatermark;
            }
        }

        let text = self.format_nudge_text(unread.len());

        NudgeDecision::Nudge {
            text,
            unread_count: unread.len(),
            newest_message_id: newest_id,
        }
    }

    /// Record that a nudge was sent to `agent_id` with `message_id` as the watermark.
    ///
    /// Updates the cooldown timer and the watermark. Must be called after
    /// successfully delivering a nudge.
    pub fn record_nudge(&mut self, agent_id: &str, message_id: String) {
        self.last_nudge.insert(agent_id.to_string(), Instant::now());
        if !message_id.is_empty() {
            self.last_nudged_message_id
                .insert(agent_id.to_string(), message_id);
        }
    }

    /// Handle a `Busy → Idle` state transition for `agent_id`.
    ///
    /// Loads the inbox, evaluates nudge eligibility, and — on Unix — fires the
    /// nudge via `tmux send-keys` if appropriate. On Windows this is a no-op
    /// (tmux is not available).
    ///
    /// # Arguments
    ///
    /// * `agent_id` - ATM identity of the agent.
    /// * `pane_id` - tmux pane ID (e.g. `"%42"`).
    /// * `inbox_path` - Path to the agent's inbox JSON file.
    pub async fn on_idle_transition(
        &mut self,
        agent_id: &str,
        pane_id: &str,
        inbox_path: &Path,
    ) -> Result<(), PluginError> {
        let entries = load_inbox_entries(inbox_path);

        let decision = self.should_nudge(agent_id, AgentState::Idle, &entries);

        match decision {
            NudgeDecision::Nudge {
                text,
                unread_count,
                newest_message_id,
            } => {
                info!(
                    "Nudging {agent_id} ({unread_count} unread messages) via pane {pane_id}"
                );
                self.sender
                    .send_text_and_enter(
                        pane_id,
                        &text,
                        self.delivery_method,
                        "nudge-primary",
                    )
                    .await?;
                self.record_nudge(agent_id, newest_message_id);

                // One retry: after 3 seconds, send Enter only.
                let pane_owned = pane_id.to_string();
                let sender = self.sender.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    if let Err(e) = sender.send_enter(&pane_owned, "nudge-retry").await {
                        warn!("Nudge retry Enter failed for pane {pane_owned}: {e}");
                    } else {
                        debug!("Nudge retry Enter sent to pane {pane_owned}");
                    }
                });
            }
            NudgeDecision::SkippedNotIdle => {
                debug!("Nudge skipped for {agent_id}: not idle");
            }
            NudgeDecision::SkippedCooldown => {
                debug!("Nudge skipped for {agent_id}: cooldown active");
            }
            NudgeDecision::SkippedDisabled => {
                debug!("Nudge skipped for {agent_id}: nudge engine disabled");
            }
            NudgeDecision::SkippedNoUnread => {
                debug!("Nudge skipped for {agent_id}: no unread messages");
            }
            NudgeDecision::SkippedWatermark => {
                debug!("Nudge skipped for {agent_id}: watermark match (already nudged)");
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// InboxEntry — lightweight inbox message representation for the nudge engine
// ---------------------------------------------------------------------------

/// Lightweight inbox message representation used by [`NudgeEngine`].
///
/// The nudge engine only needs `read` status and `message_id`; full
/// deserialization of the inbox JSON is done by the caller.
#[derive(Debug, Clone)]
pub struct InboxEntry {
    /// Whether the message has been read.
    pub read: bool,
    /// Unique message identifier (may be `None` for old-format messages).
    pub message_id: Option<String>,
}

// ---------------------------------------------------------------------------
// inbox loading
// ---------------------------------------------------------------------------

/// Load inbox entries from `path`. Returns an empty vec on any error.
fn load_inbox_entries(path: &Path) -> Vec<InboxEntry> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse inbox JSON at {}: {e}", path.display());
            return Vec::new();
        }
    };

    let arr = match value.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };

    arr.iter()
        .map(|msg| InboxEntry {
            read: msg.get("read").and_then(|v| v.as_bool()).unwrap_or(false),
            message_id: msg
                .get("message_id")
                .and_then(|v| v.as_str())
                .map(String::from),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_engine() -> NudgeEngine {
        NudgeEngine::new(NudgeConfig::default())
    }

    fn unread_entry(id: &str) -> InboxEntry {
        InboxEntry {
            read: false,
            message_id: Some(id.to_string()),
        }
    }

    fn read_entry(id: &str) -> InboxEntry {
        InboxEntry {
            read: true,
            message_id: Some(id.to_string()),
        }
    }

    // ── Disabled engine ───────────────────────────────────────────────────

    #[test]
    fn test_nudge_skipped_when_disabled() {
        let config = NudgeConfig {
            enabled: false,
            cooldown_secs: 30,
            text_template: "You have {count} messages.".to_string(),
        };
        let engine = NudgeEngine::new(config);
        let entries = vec![unread_entry("msg-1")];
        let decision = engine.should_nudge("arch-ctm", AgentState::Idle, &entries);
        assert_eq!(decision, NudgeDecision::SkippedDisabled);
    }

    // ── State gating ──────────────────────────────────────────────────────

    #[test]
    fn test_nudge_skipped_when_busy() {
        let engine = make_engine();
        let entries = vec![unread_entry("msg-1")];
        let decision = engine.should_nudge("arch-ctm", AgentState::Busy, &entries);
        assert_eq!(decision, NudgeDecision::SkippedNotIdle);
    }

    #[test]
    fn test_nudge_skipped_when_launching() {
        let engine = make_engine();
        let entries = vec![unread_entry("msg-1")];
        let decision = engine.should_nudge("arch-ctm", AgentState::Launching, &entries);
        assert_eq!(decision, NudgeDecision::SkippedNotIdle);
    }

    #[test]
    fn test_nudge_skipped_when_killed() {
        let engine = make_engine();
        let entries = vec![unread_entry("msg-1")];
        let decision = engine.should_nudge("arch-ctm", AgentState::Killed, &entries);
        assert_eq!(decision, NudgeDecision::SkippedNotIdle);
    }

    // ── No unread messages ────────────────────────────────────────────────

    #[test]
    fn test_nudge_skipped_when_no_unread() {
        let engine = make_engine();
        let entries = vec![read_entry("msg-1"), read_entry("msg-2")];
        let decision = engine.should_nudge("arch-ctm", AgentState::Idle, &entries);
        assert_eq!(decision, NudgeDecision::SkippedNoUnread);
    }

    #[test]
    fn test_nudge_skipped_when_inbox_empty() {
        let engine = make_engine();
        let decision = engine.should_nudge("arch-ctm", AgentState::Idle, &[]);
        assert_eq!(decision, NudgeDecision::SkippedNoUnread);
    }

    // ── Happy path ────────────────────────────────────────────────────────

    #[test]
    fn test_nudge_fires_when_idle_with_unread() {
        let engine = make_engine();
        let entries = vec![unread_entry("msg-1"), unread_entry("msg-2")];
        let decision = engine.should_nudge("arch-ctm", AgentState::Idle, &entries);

        match decision {
            NudgeDecision::Nudge {
                text,
                unread_count,
                newest_message_id,
            } => {
                assert_eq!(unread_count, 2);
                assert!(text.contains("2"), "text should contain the count");
                assert_eq!(newest_message_id, "msg-2");
            }
            other => panic!("Expected Nudge, got {other:?}"),
        }
    }

    #[test]
    fn test_nudge_text_formatting() {
        let config = NudgeConfig {
            enabled: true,
            cooldown_secs: 30,
            text_template: "Hey! {count} messages waiting.".to_string(),
        };
        let engine = NudgeEngine::new(config);
        let text = engine.format_nudge_text(5);
        assert_eq!(text, "Hey! 5 messages waiting.");
    }

    #[test]
    fn test_nudge_text_default_template() {
        let engine = make_engine();
        let text = engine.format_nudge_text(3);
        assert!(
            text.contains("3"),
            "formatted text should contain the count"
        );
        assert!(text.contains("ATM"), "should mention ATM");
    }

    // ── Cooldown enforcement ──────────────────────────────────────────────

    #[test]
    fn test_nudge_cooldown_enforced() {
        let config = NudgeConfig {
            enabled: true,
            cooldown_secs: 9999, // very long cooldown
            text_template: "{count}".to_string(),
        };
        let mut engine = NudgeEngine::new(config);

        // Record a nudge now
        engine.record_nudge("arch-ctm", "msg-1".to_string());

        // Immediately try again — should be blocked by cooldown
        let entries = vec![unread_entry("msg-2")];
        let decision = engine.should_nudge("arch-ctm", AgentState::Idle, &entries);
        assert_eq!(decision, NudgeDecision::SkippedCooldown);
    }

    #[test]
    fn test_nudge_cooldown_per_agent() {
        let config = NudgeConfig {
            enabled: true,
            cooldown_secs: 9999,
            text_template: "{count}".to_string(),
        };
        let mut engine = NudgeEngine::new(config);

        // Record a nudge for arch-ctm
        engine.record_nudge("arch-ctm", "msg-1".to_string());

        // Different agent should NOT be blocked
        let entries = vec![unread_entry("msg-1")];
        let decision = engine.should_nudge("other-agent", AgentState::Idle, &entries);
        // Should fire, not be blocked by arch-ctm's cooldown
        assert!(
            matches!(decision, NudgeDecision::Nudge { .. }),
            "other agent should not be on cooldown"
        );
    }

    // ── Watermark tracking ────────────────────────────────────────────────

    #[test]
    fn test_nudge_watermark_prevents_repeat() {
        let mut engine = make_engine();

        // Record that we nudged about msg-1
        engine.record_nudge("arch-ctm", "msg-1".to_string());

        // Force cooldown to zero so we can test watermark separately.
        // Insert a past timestamp by temporarily setting a short cooldown.
        let config = NudgeConfig {
            enabled: true,
            cooldown_secs: 0, // no cooldown
            text_template: "{count}".to_string(),
        };
        let mut engine2 = NudgeEngine::new(config);
        engine2.record_nudge("arch-ctm", "msg-1".to_string());

        // The newest unread message is still msg-1
        let entries = vec![unread_entry("msg-1")];
        let decision = engine2.should_nudge("arch-ctm", AgentState::Idle, &entries);
        assert_eq!(decision, NudgeDecision::SkippedWatermark);
    }

    #[test]
    fn test_nudge_watermark_clears_on_new_message() {
        let config = NudgeConfig {
            enabled: true,
            cooldown_secs: 0,
            text_template: "{count}".to_string(),
        };
        let mut engine = NudgeEngine::new(config);
        engine.record_nudge("arch-ctm", "msg-1".to_string());

        // New message (msg-2) is now the newest unread
        let entries = vec![unread_entry("msg-1"), unread_entry("msg-2")];
        let decision = engine.should_nudge("arch-ctm", AgentState::Idle, &entries);
        // Watermark is msg-1, newest is msg-2 → should nudge
        assert!(
            matches!(decision, NudgeDecision::Nudge { .. }),
            "should nudge when there is a newer unread message"
        );
    }

    // ── Inbox loading ─────────────────────────────────────────────────────

    #[test]
    fn test_load_inbox_entries_missing_file() {
        let path = std::path::Path::new("/nonexistent/path/inbox.json");
        let entries = load_inbox_entries(path);
        assert!(entries.is_empty());
    }

    #[test]
    fn test_load_inbox_entries_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.json");
        let content = r#"[
            {"from":"sender","text":"hello","timestamp":"2026-02-17T00:00:00Z","read":false,"message_id":"m1"},
            {"from":"sender","text":"world","timestamp":"2026-02-17T00:01:00Z","read":true,"message_id":"m2"}
        ]"#;
        std::fs::write(&path, content).unwrap();

        let entries = load_inbox_entries(&path);
        assert_eq!(entries.len(), 2);
        assert!(!entries[0].read);
        assert_eq!(entries[0].message_id.as_deref(), Some("m1"));
        assert!(entries[1].read);
    }

    #[test]
    fn test_load_inbox_entries_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbox.json");
        std::fs::write(&path, b"not json").unwrap();

        let entries = load_inbox_entries(&path);
        assert!(entries.is_empty());
    }
}
