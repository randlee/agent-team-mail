//! Per-thread command queue and lifecycle state machine.
//!
//! Enforces FR-17.10/FR-17.11 close > cancel > Claude > auto-mail precedence.
//!
//! [`ThreadCommandQueue`] is a pure synchronous data structure — no async needed.
//! The async dispatch layer (A.7) will wrap it in a `Mutex` and poll it from
//! the proxy event loop.

use tokio::sync::oneshot;

/// Error returned by [`ThreadCommandQueue::push_claude_reply`] when close has been requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueClosedError;

impl std::fmt::Display for QueueClosedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "thread queue is closed — no further commands accepted")
    }
}

impl std::error::Error for QueueClosedError {}

/// Result returned via the close oneshot channel when a thread is closed.
#[derive(Debug, PartialEq, Eq)]
pub enum CloseResult {
    /// Thread was idle at close time; closed immediately.
    ClosedIdle,
    /// Thread was busy; a summary was collected before close.
    ClosedWithSummary,
    /// Thread was busy; timed out waiting for summary — interrupted.
    Interrupted,
}

/// A command that can be dispatched to a Codex thread.
///
/// Commands are prioritised: `Close` jumps to the front of the queue;
/// `ClaudeReply` precedes `AutoMailInject`.
pub enum ThreadCommand {
    /// Claude-initiated codex-reply turn (highest priority after close).
    ClaudeReply {
        /// The JSON-RPC request id from the upstream call.
        request_id: serde_json::Value,
        /// The tool arguments forwarded to the child.
        args: serde_json::Value,
        /// Channel to deliver the child's response back to the upstream caller.
        ///
        /// When a `ClaudeReply` is dispatched from the queue, the dispatcher
        /// registers this sender in the pending map so the child's response
        /// completes the original upstream request.
        respond_tx: oneshot::Sender<serde_json::Value>,
    },
    /// Auto-mail injection turn (lowest priority, FR-17.11).
    AutoMailInject {
        /// The mail content to inject as a new turn.
        content: String,
    },
    /// Close the thread (highest overall priority).
    ///
    /// The sender awaits the [`CloseResult`] via the oneshot channel.
    Close {
        /// Channel to report the close outcome back to the caller.
        respond_tx: oneshot::Sender<CloseResult>,
    },
}

impl std::fmt::Debug for ThreadCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClaudeReply { request_id, .. } => {
                write!(f, "ClaudeReply {{ request_id: {request_id} }}")
            }
            Self::AutoMailInject { .. } => write!(f, "AutoMailInject"),
            Self::Close { .. } => write!(f, "Close"),
        }
    }
}

/// Priority command queue for a single Codex thread.
///
/// Enforces the precedence rule: `Close` > `ClaudeReply` > `AutoMailInject`.
/// Once a close is requested, no further commands are accepted.
///
/// This struct is intentionally not `Send` — wrap in `Arc<tokio::sync::Mutex<…>>`
/// at the call site when sharing across tasks.
///
/// # Examples
///
/// ```
/// use atm_agent_mcp::lifecycle::{ThreadCommandQueue, CloseResult};
/// use tokio::sync::oneshot;
///
/// let mut q = ThreadCommandQueue::new("codex:test-agent".to_string());
/// // Push a Claude reply with a respond_tx for upstream delivery
/// let (tx, _rx) = oneshot::channel();
/// assert!(q.push_claude_reply(serde_json::json!(1), serde_json::json!({}), tx).is_ok());
/// // Pop it back
/// assert!(q.pop_next().is_some());
/// ```
#[derive(Debug)]
pub struct ThreadCommandQueue {
    /// The agent this queue belongs to.
    agent_id: String,
    /// Pending commands in priority order.
    queue: std::collections::VecDeque<ThreadCommand>,
    /// Whether a close has been requested (for idempotency, FR-17.9).
    close_requested: bool,
}

impl ThreadCommandQueue {
    /// Create a new, empty command queue for the given agent.
    pub fn new(agent_id: String) -> Self {
        Self {
            agent_id,
            queue: std::collections::VecDeque::new(),
            close_requested: false,
        }
    }

    /// The agent_id this queue is associated with.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Whether a close has been requested on this queue.
    pub fn is_close_requested(&self) -> bool {
        self.close_requested
    }

    /// Enqueue a Claude-initiated reply turn.
    ///
    /// The `respond_tx` oneshot is stored with the command so that when the
    /// dispatcher pops this entry, it can register the sender in the pending
    /// map and the child's eventual response will complete the original
    /// upstream request.
    ///
    /// Returns `Err(QueueClosedError)` when a close has already been requested (FR-17.9).
    /// The caller should return `ERR_SESSION_CLOSED` to upstream when this fails.
    pub fn push_claude_reply(
        &mut self,
        request_id: serde_json::Value,
        args: serde_json::Value,
        respond_tx: oneshot::Sender<serde_json::Value>,
    ) -> Result<(), QueueClosedError> {
        if self.close_requested {
            return Err(QueueClosedError);
        }
        self.queue.push_back(ThreadCommand::ClaudeReply {
            request_id,
            args,
            respond_tx,
        });
        Ok(())
    }

    /// Enqueue an auto-mail injection turn (lowest priority).
    ///
    /// Silently dropped when:
    /// - A close has been requested, or
    /// - A `ClaudeReply` is already pending (FR-8.10 / FR-17.11).
    ///
    /// Returns `true` if the command was queued, `false` if it was dropped.
    pub fn push_auto_mail(&mut self, content: String) -> bool {
        if self.close_requested {
            return false;
        }
        // Reject if any ClaudeReply is already pending
        let has_pending_reply = self
            .queue
            .iter()
            .any(|c| matches!(c, ThreadCommand::ClaudeReply { .. }));
        if has_pending_reply {
            return false;
        }
        self.queue
            .push_back(ThreadCommand::AutoMailInject { content });
        true
    }

    /// Enqueue a close command at the front of the queue (highest priority).
    ///
    /// Returns `true` if the close was accepted (first time), `false` if a
    /// close was already requested (idempotent — FR-17.9).
    ///
    /// On `false`, the caller should drop `respond_tx` or send a duplicate
    /// result themselves.
    pub fn push_close(&mut self, respond_tx: oneshot::Sender<CloseResult>) -> bool {
        if self.close_requested {
            return false;
        }
        self.close_requested = true;
        // Close always jumps to the front of the queue
        self.queue.push_front(ThreadCommand::Close { respond_tx });
        true
    }

    /// Pop the next command from the queue.
    ///
    /// Returns `None` when the queue is empty.
    pub fn pop_next(&mut self) -> Option<ThreadCommand> {
        self.queue.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    fn make_queue() -> ThreadCommandQueue {
        ThreadCommandQueue::new("codex:test-agent".to_string())
    }

    // ─── Idempotent close ────────────────────────────────────────────────────

    #[test]
    fn close_is_idempotent_second_push_returns_false() {
        let mut q = make_queue();
        let (tx1, _rx1) = oneshot::channel::<CloseResult>();
        let (tx2, _rx2) = oneshot::channel::<CloseResult>();

        assert!(q.push_close(tx1), "first close should be accepted");
        assert!(
            !q.push_close(tx2),
            "second close must return false (idempotent)"
        );
        // Only one Close command in the queue
        assert!(q.is_close_requested());
    }

    // ─── Claude reply rejected when close pending ─────────────────────────────

    #[test]
    fn claude_reply_rejected_after_close() {
        let mut q = make_queue();
        let (tx, _rx) = oneshot::channel::<CloseResult>();
        q.push_close(tx);

        let (reply_tx, _reply_rx) = oneshot::channel();
        let result = q.push_claude_reply(serde_json::json!(1), serde_json::json!({}), reply_tx);
        assert!(
            result.is_err(),
            "ClaudeReply must be rejected when close is pending"
        );
    }

    // ─── Auto mail rejected when close pending ────────────────────────────────

    #[test]
    fn auto_mail_rejected_after_close() {
        let mut q = make_queue();
        let (tx, _rx) = oneshot::channel::<CloseResult>();
        q.push_close(tx);

        let queued = q.push_auto_mail("inject me".to_string());
        assert!(
            !queued,
            "AutoMailInject must be rejected when close is pending"
        );
    }

    // ─── Auto mail rejected when Claude reply queued ──────────────────────────

    #[test]
    fn auto_mail_rejected_when_claude_reply_queued() {
        let mut q = make_queue();
        let (reply_tx, _reply_rx) = oneshot::channel();
        q.push_claude_reply(serde_json::json!(1), serde_json::json!({}), reply_tx)
            .unwrap();

        let queued = q.push_auto_mail("inject me".to_string());
        assert!(
            !queued,
            "AutoMailInject must be rejected when a ClaudeReply is pending (FR-8.10)"
        );
    }

    // ─── Close jumps to front ─────────────────────────────────────────────────

    #[test]
    fn close_jumps_to_front_of_non_empty_queue() {
        let mut q = make_queue();
        // Push a ClaudeReply first
        let (reply_tx, _reply_rx) = oneshot::channel();
        q.push_claude_reply(
            serde_json::json!(42),
            serde_json::json!({"prompt": "hello"}),
            reply_tx,
        )
        .unwrap();

        // Now push close — it should jump ahead
        let (tx, _rx) = oneshot::channel::<CloseResult>();
        assert!(q.push_close(tx));

        // First pop must be the Close, not the ClaudeReply
        let first = q.pop_next().expect("queue must not be empty");
        assert!(
            matches!(first, ThreadCommand::Close { .. }),
            "Close must be the first command popped"
        );

        // Second pop is the ClaudeReply
        let second = q.pop_next().expect("ClaudeReply must still be present");
        assert!(
            matches!(second, ThreadCommand::ClaudeReply { .. }),
            "ClaudeReply must follow Close"
        );
    }

    // ─── pop_next on empty queue ──────────────────────────────────────────────

    #[test]
    fn pop_next_on_empty_queue_returns_none() {
        let mut q = make_queue();
        assert!(q.pop_next().is_none());
    }

    // ─── Basic round-trip ─────────────────────────────────────────────────────

    #[test]
    fn push_and_pop_claude_reply_round_trip() {
        let mut q = make_queue();
        let (reply_tx, _reply_rx) = oneshot::channel();
        q.push_claude_reply(serde_json::json!(99), serde_json::json!({"x": 1}), reply_tx)
            .unwrap();

        let cmd = q.pop_next().unwrap();
        match cmd {
            ThreadCommand::ClaudeReply {
                request_id, args, ..
            } => {
                assert_eq!(request_id, serde_json::json!(99));
                assert_eq!(args["x"], 1);
            }
            _ => panic!("expected ClaudeReply"),
        }
        assert!(q.pop_next().is_none());
    }

    #[test]
    fn push_and_pop_auto_mail_round_trip() {
        let mut q = make_queue();
        let queued = q.push_auto_mail("hello world".to_string());
        assert!(queued);

        let cmd = q.pop_next().unwrap();
        match cmd {
            ThreadCommand::AutoMailInject { content } => {
                assert_eq!(content, "hello world");
            }
            _ => panic!("expected AutoMailInject"),
        }
    }

    // ─── Queue ordering: Close > ClaudeReply > AutoMailInject ─────────────────
    //
    // These tests validate the priority invariant (FR-17.10/FR-17.11) holds
    // regardless of the insertion order of commands.  They are transport-
    // independent: the queue is a pure data structure; no async I/O needed.

    /// Insertion order: AutoMail → ClaudeReply → Close.
    ///
    /// AutoMail is accepted because it is pushed before any ClaudeReply.
    /// ClaudeReply is appended next.  Close jumps to the front.
    ///
    /// Expected pop order: Close → AutoMail (FIFO) → ClaudeReply (FIFO).
    ///
    /// Note: the queue does NOT retroactively reorder items already enqueued.
    /// The priority rule applies at *insertion* time:
    /// - AutoMailInject is dropped only when a ClaudeReply is *already in the
    ///   queue* at push time.  Here AutoMail was pushed first, so it stays.
    /// - Close jumps to the front at push time regardless of what is queued.
    #[test]
    fn ordering_auto_mail_then_claude_reply_then_close_close_is_first() {
        let mut q = make_queue();

        // AutoMailInject first (accepted — no ClaudeReply pending yet).
        let mail_queued = q.push_auto_mail("mail-1".to_string());
        assert!(mail_queued, "AutoMail accepted before ClaudeReply");

        // ClaudeReply second (appended after AutoMail).
        let (reply_tx, _reply_rx) = oneshot::channel();
        q.push_claude_reply(serde_json::json!(1), serde_json::json!({}), reply_tx)
            .unwrap();

        // Close last — must jump to the front.
        let (close_tx, _close_rx) = oneshot::channel::<CloseResult>();
        assert!(q.push_close(close_tx));

        // Pop 1: must be Close (always front).
        let first = q.pop_next().unwrap();
        assert!(
            matches!(first, ThreadCommand::Close { .. }),
            "first pop must be Close, got: {first:?}"
        );

        // Pop 2: AutoMail (was inserted before ClaudeReply — FIFO order retained).
        let second = q.pop_next().unwrap();
        assert!(
            matches!(second, ThreadCommand::AutoMailInject { .. }),
            "second pop must be AutoMailInject (FIFO after Close), got: {second:?}"
        );

        // Pop 3: ClaudeReply (was inserted after AutoMail).
        let third = q.pop_next().unwrap();
        assert!(
            matches!(third, ThreadCommand::ClaudeReply { .. }),
            "third pop must be ClaudeReply, got: {third:?}"
        );

        assert!(q.pop_next().is_none(), "queue must be empty");
    }

    /// Insertion order: ClaudeReply → Close.
    /// AutoMailInject attempted AFTER ClaudeReply is already pending — must be
    /// dropped (FR-8.10).  Expected pop order: Close → ClaudeReply.
    #[test]
    fn ordering_claude_reply_first_auto_mail_dropped_then_close_is_first() {
        let mut q = make_queue();

        // ClaudeReply first.
        let (reply_tx, _reply_rx) = oneshot::channel();
        q.push_claude_reply(serde_json::json!(2), serde_json::json!({}), reply_tx)
            .unwrap();

        // AutoMail — rejected because ClaudeReply is pending (FR-8.10).
        let mail_queued = q.push_auto_mail("should-be-dropped".to_string());
        assert!(
            !mail_queued,
            "AutoMail must be dropped when ClaudeReply is pending"
        );

        // Close — must jump to front.
        let (close_tx, _close_rx) = oneshot::channel::<CloseResult>();
        assert!(q.push_close(close_tx));

        // Pop 1: Close.
        let first = q.pop_next().unwrap();
        assert!(matches!(first, ThreadCommand::Close { .. }));

        // Pop 2: ClaudeReply.
        let second = q.pop_next().unwrap();
        assert!(matches!(second, ThreadCommand::ClaudeReply { .. }));

        // Queue empty — dropped AutoMail is never present.
        assert!(q.pop_next().is_none());
    }

    /// Insertion order: Close only.
    /// AutoMail and ClaudeReply pushed after close must both be rejected.
    /// Pop order: Close only.
    #[test]
    fn ordering_close_only_subsequent_commands_rejected() {
        let mut q = make_queue();

        let (close_tx, _close_rx) = oneshot::channel::<CloseResult>();
        assert!(q.push_close(close_tx));

        // Both subsequent pushes must be rejected.
        let mail_queued = q.push_auto_mail("rejected".to_string());
        assert!(!mail_queued, "AutoMail must be rejected post-close");

        let (reply_tx, _reply_rx) = oneshot::channel();
        let reply_result =
            q.push_claude_reply(serde_json::json!(3), serde_json::json!({}), reply_tx);
        assert!(
            reply_result.is_err(),
            "ClaudeReply must be rejected post-close"
        );

        // Only the Close command is present.
        assert!(matches!(q.pop_next().unwrap(), ThreadCommand::Close { .. }));
        assert!(q.pop_next().is_none());
    }

    /// Insert multiple ClaudeReplies — they should drain FIFO (Close > each
    /// ClaudeReply in order), AutoMail never enters after first ClaudeReply.
    #[test]
    fn ordering_multiple_claude_replies_drain_fifo_before_auto_mail() {
        let mut q = make_queue();

        // Two ClaudeReplies.
        let (tx1, _rx1) = oneshot::channel();
        let (tx2, _rx2) = oneshot::channel();
        q.push_claude_reply(serde_json::json!(10), serde_json::json!({}), tx1)
            .unwrap();
        q.push_claude_reply(serde_json::json!(11), serde_json::json!({}), tx2)
            .unwrap();

        // AutoMail dropped (ClaudeReply pending).
        assert!(!q.push_auto_mail("nope".to_string()));

        // No close — just verify FIFO ordering of ClaudeReplies.
        let first = q.pop_next().unwrap();
        match first {
            ThreadCommand::ClaudeReply { request_id, .. } => {
                assert_eq!(request_id, serde_json::json!(10));
            }
            _ => panic!("expected first ClaudeReply with id=10"),
        }

        let second = q.pop_next().unwrap();
        match second {
            ThreadCommand::ClaudeReply { request_id, .. } => {
                assert_eq!(request_id, serde_json::json!(11));
            }
            _ => panic!("expected second ClaudeReply with id=11"),
        }

        assert!(q.pop_next().is_none());
    }
}
