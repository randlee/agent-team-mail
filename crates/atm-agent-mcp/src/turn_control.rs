//! Unified turn control abstraction across all transports.
//!
//! This module provides the [`TurnControl`] trait and its shared implementation,
//! [`TurnTracker`], which tracks the active `turn_id` per thread and emits
//! lifecycle events to the ATM daemon when turns start and complete.
//!
//! # Design
//!
//! All three transports (`McpTransport`, `JsonCodecTransport`,
//! `AppServerTransport`) share the same turn-control interface so that
//! `proxy.rs` and downstream code can call `start_turn`, `steer_turn`,
//! and `on_turn_completed` without knowing which transport is active.
//!
//! # Daemon emission
//!
//! When a turn completes (for any transport), [`TurnTracker::on_turn_completed`]
//! and [`TurnTracker::interrupt_turn`] both call
//! [`crate::lifecycle_emit::emit_lifecycle_event`] with
//! [`crate::lifecycle_emit::EventKind::TeammateIdle`], so the daemon is notified
//! via a single normalised code path regardless of transport.
//!
//! # Stale-turn rejection
//!
//! [`TurnControl::steer_turn`] checks that the caller-supplied
//! `expected_turn_id` matches the currently-active turn for the given thread.
//! A mismatch (or no active turn) returns a [`StaleTurnError`] describing the
//! discrepancy.  The proxy uses this to reject `turn/steer` requests that were
//! issued against an already-completed or replaced turn.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use agent_team_mail_core::daemon_stream::DaemonStreamEvent;

use crate::lifecycle_emit::{EventKind, emit_lifecycle_event};
use crate::stream_norm::TurnStatus;

// ─── StaleTurnError ───────────────────────────────────────────────────────────

/// Error returned by [`TurnControl::validate_steer`] when the caller's
/// `expected_turn_id` does not match the currently active turn for a thread.
///
/// # Fields
///
/// * `expected` — the turn ID the caller expected to be active.
/// * `actual`   — the turn ID that is actually active, or `None` if no turn is
///   in progress on that thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleTurnError {
    /// The turn ID supplied by the caller (i.e. the one they thought was active).
    pub expected: String,
    /// The turn ID currently active on the thread, or `None` if the thread is idle.
    pub actual: Option<String>,
}

impl std::fmt::Display for StaleTurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.actual {
            Some(actual) => write!(
                f,
                "stale turn: expected turn_id={:?} but active turn is {:?}",
                self.expected, actual
            ),
            None => write!(
                f,
                "stale turn: expected turn_id={:?} but no turn is active",
                self.expected
            ),
        }
    }
}

impl std::error::Error for StaleTurnError {}

// ─── TurnControl ──────────────────────────────────────────────────────────────

/// Transport-agnostic turn control operations.
///
/// Implemented by [`TurnTracker`]. `proxy.rs` calls these methods regardless
/// of which transport (`McpTransport`, `JsonCodecTransport`,
/// `AppServerTransport`) is active.
///
/// Daemon lifecycle events are emitted inside the implementation so the caller
/// does not need to know about [`crate::lifecycle_emit`].
#[async_trait]
pub trait TurnControl: Send + Sync {
    /// Record that a new turn has begun on `thread_id`.
    ///
    /// Sets the active turn to `turn_id`. Callers invoke this when a
    /// `turn/started` notification is observed (app-server) or when a turn is
    /// initiated via cli-json or MCP.
    async fn start_turn(&self, thread_id: &str, turn_id: &str);

    /// Validate that `expected_turn_id` matches the active turn for `thread_id`.
    ///
    /// Returns `Ok(())` if the IDs match, or [`Err(StaleTurnError)`] if:
    /// - No turn is active on `thread_id` (`actual = None`), or
    /// - A different turn is active (`actual = Some(other_id)`).
    ///
    /// Used by the proxy to gate `turn/steer` requests before the steer is sent.
    async fn steer_turn(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
    ) -> Result<(), StaleTurnError>;

    /// Record that the active turn on `thread_id` has completed normally or
    /// been interrupted.
    ///
    /// Clears the active turn and emits a
    /// [`EventKind::TeammateIdle`] daemon event.
    async fn on_turn_completed(&self, thread_id: &str, turn_id: &str, status: TurnStatus);

    /// Record that the active turn on `thread_id` was forcibly interrupted.
    ///
    /// Clears the active turn and emits a
    /// [`EventKind::TeammateIdle`] daemon event.
    async fn interrupt_turn(&self, thread_id: &str);

    /// Return the currently active `turn_id` for `thread_id`, or `None` if
    /// the thread is idle.
    async fn active_turn_id(&self, thread_id: &str) -> Option<String>;
}

// ─── TurnTracker ──────────────────────────────────────────────────────────────

/// Session identity context required for daemon lifecycle emission.
///
/// Constructed once per session and supplied to [`TurnTracker::new`].
/// The tracker clones these strings into each daemon event it emits.
#[derive(Debug, Clone)]
pub struct SessionContext {
    /// ATM identity of the agent (e.g. `"arch-ctm"`).
    pub identity: String,
    /// ATM team name (e.g. `"atm-dev"`).
    pub team: String,
    /// ATM session ID in `"codex:<uuid>"` format.
    pub session_id: String,
}

impl SessionContext {
    /// Create a new [`SessionContext`].
    pub fn new(
        identity: impl Into<String>,
        team: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            identity: identity.into(),
            team: team.into(),
            session_id: session_id.into(),
        }
    }
}

/// Shared, `Clone`-able turn tracker that implements [`TurnControl`].
///
/// Holds an `Arc<Mutex<HashMap<thread_id, turn_id>>>` so the same
/// [`TurnTracker`] can be shared across transport task boundaries without
/// additional wrapping.
///
/// Supports deferred [`SessionContext`] binding via [`TurnTracker::new_deferred`]
/// and [`TurnTracker::set_session_context`]. When no context is set, daemon
/// emission is a no-op. This allows transports to hold a `TurnTracker` before
/// session information is available.
///
/// The `transport` field identifies which transport owns this tracker (e.g.
/// `"mcp"`, `"cli-json"`, `"app-server"`). It is included in every daemon
/// stream event so that TUI consumers can distinguish turn events by transport.
///
/// # Thread safety
///
/// All mutations go through the inner `Mutex`; the outer `Arc` allows
/// cheap cloning across tasks.
#[derive(Debug, Clone)]
pub struct TurnTracker {
    /// Map from `thread_id` to the currently active `turn_id`.
    ///
    /// `None` value means the thread is idle (no active turn).
    active_turns: Arc<Mutex<HashMap<String, Option<String>>>>,
    /// Session context for daemon lifecycle emission.
    ///
    /// `None` when created via [`TurnTracker::new_deferred`] and
    /// [`TurnTracker::set_session_context`] has not yet been called.
    /// Daemon emission is a no-op when `None`.
    ctx: Arc<Mutex<Option<SessionContext>>>,
    /// Transport identifier included in every daemon stream event.
    ///
    /// Set at construction via [`TurnTracker::new`] or
    /// [`TurnTracker::new_deferred`]. Defaults to `"unknown"` for deferred
    /// trackers so that events emitted before the transport is identified are
    /// still distinguishable from silence.
    transport: Arc<String>,
}

impl TurnTracker {
    /// Create a new, empty [`TurnTracker`] bound to the given session and transport.
    ///
    /// `transport` identifies which transport owns this tracker (e.g. `"mcp"`,
    /// `"cli-json"`, `"app-server"`). It is included in every daemon stream event.
    pub fn new(ctx: SessionContext, transport: impl Into<String>) -> Self {
        Self {
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            ctx: Arc::new(Mutex::new(Some(ctx))),
            transport: Arc::new(transport.into()),
        }
    }

    /// Create a new, empty [`TurnTracker`] with no session context.
    ///
    /// `transport` identifies which transport owns this tracker (e.g. `"mcp"`,
    /// `"cli-json"`, `"app-server"`). Defaults to `"unknown"` if not known at
    /// construction time — pass the correct transport name when available.
    ///
    /// Daemon lifecycle emission is a no-op until [`Self::set_session_context`]
    /// is called. This constructor is used when transports are created before
    /// session information (identity, team, session_id) is available.
    pub fn new_deferred(transport: impl Into<String>) -> Self {
        Self {
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            ctx: Arc::new(Mutex::new(None)),
            transport: Arc::new(transport.into()),
        }
    }

    /// Bind a [`SessionContext`] to this tracker, enabling daemon emission.
    ///
    /// May be called at any time after construction. Subsequent calls to
    /// turn-control methods will emit daemon lifecycle events using the
    /// provided context. Calling this more than once replaces the previous
    /// context.
    pub async fn set_session_context(&self, ctx: SessionContext) {
        *self.ctx.lock().await = Some(ctx);
    }

    /// Emit a [`EventKind::TeammateIdle`] event to the daemon (best-effort).
    ///
    /// Also emits a [`DaemonStreamEvent::TurnIdle`] stream event so that the
    /// TUI receives push notification of the transition.
    ///
    /// No-op if no [`SessionContext`] has been set.
    async fn emit_idle(&self) {
        let guard = self.ctx.lock().await;
        if let Some(ref ctx) = *guard {
            emit_lifecycle_event(
                EventKind::TeammateIdle,
                &ctx.identity,
                &ctx.team,
                &ctx.session_id,
                None,
            )
            .await;
            crate::stream_emit::emit_stream_event(&DaemonStreamEvent::TurnIdle {
                agent: ctx.identity.clone(),
                turn_id: String::new(),
                transport: (*self.transport).clone(),
            })
            .await;
        }
    }
}

#[async_trait]
impl TurnControl for TurnTracker {
    async fn start_turn(&self, thread_id: &str, turn_id: &str) {
        {
            let mut guard = self.active_turns.lock().await;
            guard.insert(thread_id.to_string(), Some(turn_id.to_string()));
        }
        tracing::debug!(
            thread_id = %thread_id,
            turn_id = %turn_id,
            "turn_control: turn started"
        );
        // Emit stream event (best-effort, fire-and-forget).
        let guard = self.ctx.lock().await;
        if let Some(ref ctx) = *guard {
            crate::stream_emit::emit_stream_event(&DaemonStreamEvent::TurnStarted {
                agent: ctx.identity.clone(),
                thread_id: thread_id.to_string(),
                turn_id: turn_id.to_string(),
                transport: (*self.transport).clone(),
            })
            .await;
        }
    }

    async fn steer_turn(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
    ) -> Result<(), StaleTurnError> {
        let guard = self.active_turns.lock().await;
        let actual = guard.get(thread_id).and_then(|opt| opt.as_deref());
        match actual {
            Some(active) if active == expected_turn_id => Ok(()),
            Some(active) => Err(StaleTurnError {
                expected: expected_turn_id.to_string(),
                actual: Some(active.to_string()),
            }),
            None => Err(StaleTurnError {
                expected: expected_turn_id.to_string(),
                actual: None,
            }),
        }
    }

    async fn on_turn_completed(&self, thread_id: &str, turn_id: &str, status: TurnStatus) {
        {
            let mut guard = self.active_turns.lock().await;
            guard.insert(thread_id.to_string(), None);
        }
        tracing::debug!(
            thread_id = %thread_id,
            turn_id = %turn_id,
            status = ?status,
            "turn_control: turn completed → idle"
        );
        // Emit TurnCompleted stream event before the idle transition.
        {
            let guard = self.ctx.lock().await;
            if let Some(ref ctx) = *guard {
                crate::stream_emit::emit_stream_event(&DaemonStreamEvent::TurnCompleted {
                    agent: ctx.identity.clone(),
                    thread_id: thread_id.to_string(),
                    turn_id: turn_id.to_string(),
                    status: agent_team_mail_core::daemon_stream::TurnStatusWire::from(status),
                    transport: (*self.transport).clone(),
                })
                .await;
            }
        }
        self.emit_idle().await;
    }

    async fn interrupt_turn(&self, thread_id: &str) {
        {
            let mut guard = self.active_turns.lock().await;
            guard.insert(thread_id.to_string(), None);
        }
        tracing::debug!(
            thread_id = %thread_id,
            "turn_control: turn interrupted → idle"
        );
        self.emit_idle().await;
    }

    async fn active_turn_id(&self, thread_id: &str) -> Option<String> {
        let guard = self.active_turns.lock().await;
        guard.get(thread_id).and_then(|opt| opt.clone())
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::tempdir;

    fn make_tracker() -> TurnTracker {
        TurnTracker::new(
            SessionContext::new("arch-ctm", "atm-dev", "codex:test-1234"),
            "mcp",
        )
    }

    // ── StaleTurnError display ────────────────────────────────────────────────

    #[test]
    fn stale_turn_error_display_with_active_turn() {
        let err = StaleTurnError {
            expected: "old-turn".to_string(),
            actual: Some("new-turn".to_string()),
        };
        let msg = err.to_string();
        assert!(msg.contains("old-turn"), "should mention expected: {msg}");
        assert!(msg.contains("new-turn"), "should mention actual: {msg}");
    }

    #[test]
    fn stale_turn_error_display_no_active_turn() {
        let err = StaleTurnError {
            expected: "gone-turn".to_string(),
            actual: None,
        };
        let msg = err.to_string();
        assert!(msg.contains("gone-turn"), "should mention expected: {msg}");
        assert!(
            msg.contains("no turn is active"),
            "should mention idle state: {msg}"
        );
    }

    // ── active_turn_id: initial state ─────────────────────────────────────────

    #[tokio::test]
    async fn active_turn_id_returns_none_for_unknown_thread() {
        let tracker = make_tracker();
        assert!(tracker.active_turn_id("thread-x").await.is_none());
    }

    // ── TurnTracker::new_deferred creates tracker without context ─────────────

    #[tokio::test]
    async fn new_deferred_creates_tracker_without_context() {
        let tracker = TurnTracker::new_deferred("mcp");
        // Should work without panic — no context means no daemon emission.
        tracker.start_turn("t1", "turn-def").await;
        assert_eq!(
            tracker.active_turn_id("t1").await,
            Some("turn-def".to_string())
        );
    }

    // ── set_session_context enables deferred tracker ──────────────────────────

    #[tokio::test]
    #[serial]
    async fn set_session_context_enables_daemon_emission() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        let tracker = TurnTracker::new_deferred("mcp");
        tracker
            .set_session_context(SessionContext::new("arch-ctm", "atm-dev", "codex:test"))
            .await;
        // After setting context, turn tracking should still work.
        tracker.start_turn("t1", "turn-ctx").await;
        assert_eq!(
            tracker.active_turn_id("t1").await,
            Some("turn-ctx".to_string())
        );

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }

    // ── start_turn sets the active turn ───────────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn start_turn_sets_active_turn_id() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        let tracker = make_tracker();
        tracker.start_turn("t1", "turn-abc").await;
        assert_eq!(
            tracker.active_turn_id("t1").await,
            Some("turn-abc".to_string())
        );

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }

    // ── steer_turn: correct id succeeds ───────────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn steer_turn_correct_turn_id_succeeds() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        let tracker = make_tracker();
        tracker.start_turn("t1", "turn-abc").await;
        assert!(tracker.steer_turn("t1", "turn-abc").await.is_ok());

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }

    // ── steer_turn: mismatched id returns StaleTurnError ──────────────────────

    #[tokio::test]
    #[serial]
    async fn steer_turn_mismatched_turn_id_returns_error() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        let tracker = make_tracker();
        tracker.start_turn("t1", "current-turn").await;

        let err = tracker
            .steer_turn("t1", "stale-turn")
            .await
            .expect_err("mismatched id must return StaleTurnError");
        assert_eq!(err.expected, "stale-turn");
        assert_eq!(err.actual, Some("current-turn".to_string()));

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }

    // ── steer_turn: no active turn returns StaleTurnError(actual=None) ────────

    #[tokio::test]
    async fn steer_turn_no_active_turn_returns_error_with_none_actual() {
        let tracker = make_tracker();
        let err = tracker
            .steer_turn("t1", "any-turn")
            .await
            .expect_err("no active turn must return StaleTurnError");
        assert_eq!(err.expected, "any-turn");
        assert!(err.actual.is_none(), "actual must be None when idle");
    }

    // ── on_turn_completed clears the active turn ──────────────────────────────

    #[tokio::test]
    #[serial]
    async fn on_turn_completed_clears_active_turn() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        let tracker = make_tracker();
        tracker.start_turn("t1", "turn-xyz").await;
        tracker
            .on_turn_completed("t1", "turn-xyz", TurnStatus::Completed)
            .await;
        assert!(
            tracker.active_turn_id("t1").await.is_none(),
            "active_turn_id must be None after completion"
        );

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }

    // ── steer_turn after completion returns StaleTurnError(actual=None) ───────

    #[tokio::test]
    #[serial]
    async fn steer_turn_after_completion_returns_stale_error() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        let tracker = make_tracker();
        tracker.start_turn("t1", "turn-done").await;
        tracker
            .on_turn_completed("t1", "turn-done", TurnStatus::Completed)
            .await;

        let err = tracker
            .steer_turn("t1", "turn-done")
            .await
            .expect_err("post-completion steer must be rejected");
        assert_eq!(err.expected, "turn-done");
        assert!(
            err.actual.is_none(),
            "no turn should be active after completion"
        );

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }

    // ── interrupt_turn clears the active turn ─────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn interrupt_turn_clears_active_turn() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        let tracker = make_tracker();
        tracker.start_turn("t1", "turn-int").await;
        tracker.interrupt_turn("t1").await;
        assert!(
            tracker.active_turn_id("t1").await.is_none(),
            "active_turn_id must be None after interrupt"
        );

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }

    // ── Full round-trip ───────────────────────────────────────────────────────

    /// start_turn → active_turn_id matches → steer_turn succeeds
    /// → on_turn_completed → active_turn_id is None → steer_turn fails
    #[tokio::test]
    #[serial]
    async fn full_round_trip_start_steer_complete() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        let tracker = make_tracker();

        // Start a turn.
        tracker.start_turn("thread-1", "turn-rt-1").await;

        // Active turn ID matches.
        assert_eq!(
            tracker.active_turn_id("thread-1").await,
            Some("turn-rt-1".to_string())
        );

        // Steer with correct turn_id succeeds.
        tracker
            .steer_turn("thread-1", "turn-rt-1")
            .await
            .expect("steer with correct id must succeed");

        // Complete the turn.
        tracker
            .on_turn_completed("thread-1", "turn-rt-1", TurnStatus::Completed)
            .await;

        // Active turn ID is now None.
        assert!(tracker.active_turn_id("thread-1").await.is_none());

        // Steer after completion is rejected.
        let err = tracker
            .steer_turn("thread-1", "turn-rt-1")
            .await
            .expect_err("steer after completion must fail");
        assert_eq!(err.expected, "turn-rt-1");
        assert!(err.actual.is_none());

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }

    // ── Multiple threads are tracked independently ────────────────────────────

    #[tokio::test]
    #[serial]
    async fn multiple_threads_tracked_independently() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        let tracker = make_tracker();

        tracker.start_turn("thread-a", "turn-a1").await;
        tracker.start_turn("thread-b", "turn-b1").await;

        // Each thread has its own turn.
        assert_eq!(
            tracker.active_turn_id("thread-a").await,
            Some("turn-a1".to_string())
        );
        assert_eq!(
            tracker.active_turn_id("thread-b").await,
            Some("turn-b1".to_string())
        );

        // Completing thread-a does not affect thread-b.
        tracker
            .on_turn_completed("thread-a", "turn-a1", TurnStatus::Completed)
            .await;
        assert!(tracker.active_turn_id("thread-a").await.is_none());
        assert_eq!(
            tracker.active_turn_id("thread-b").await,
            Some("turn-b1".to_string())
        );

        // Steer on thread-b with correct id still succeeds.
        tracker
            .steer_turn("thread-b", "turn-b1")
            .await
            .expect("thread-b steer must still succeed");

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }

    // ── TurnStatus variants are all handled ───────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn on_turn_completed_with_interrupted_status_clears_turn() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        let tracker = make_tracker();
        tracker.start_turn("t1", "turn-i").await;
        tracker
            .on_turn_completed("t1", "turn-i", TurnStatus::Interrupted)
            .await;
        assert!(tracker.active_turn_id("t1").await.is_none());

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }

    #[tokio::test]
    #[serial]
    async fn on_turn_completed_with_failed_status_clears_turn() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        let tracker = make_tracker();
        tracker.start_turn("t1", "turn-f").await;
        tracker
            .on_turn_completed("t1", "turn-f", TurnStatus::Failed)
            .await;
        assert!(tracker.active_turn_id("t1").await.is_none());

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }

    // ── SessionContext ────────────────────────────────────────────────────────

    #[test]
    fn session_context_stores_fields() {
        let ctx = SessionContext::new("agent-1", "team-x", "codex:abc");
        assert_eq!(ctx.identity, "agent-1");
        assert_eq!(ctx.team, "team-x");
        assert_eq!(ctx.session_id, "codex:abc");
    }

    // ── Stream event emission: no-panic with absent daemon ───────────────────

    /// Verify that `start_turn` emits a stream event without panicking when no
    /// daemon socket is present (best-effort noop).
    #[tokio::test]
    #[serial]
    async fn start_turn_emits_stream_event_with_no_daemon() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        let tracker = make_tracker();
        // Must not panic even with no daemon running.
        tracker.start_turn("t1", "turn-stream-1").await;
        assert_eq!(
            tracker.active_turn_id("t1").await,
            Some("turn-stream-1".to_string())
        );

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }

    /// Verify that `on_turn_completed` emits a stream event without panicking
    /// when no daemon socket is present (best-effort noop).
    #[tokio::test]
    #[serial]
    async fn on_turn_completed_emits_stream_event_with_no_daemon() {
        let dir = tempdir().unwrap();
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        let tracker = make_tracker();
        tracker.start_turn("t1", "turn-stream-2").await;
        // Must not panic even with no daemon running.
        tracker
            .on_turn_completed("t1", "turn-stream-2", TurnStatus::Completed)
            .await;
        assert!(
            tracker.active_turn_id("t1").await.is_none(),
            "active_turn_id must be None after completion"
        );

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }
}
