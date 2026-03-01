//! Application state machine for the ATM TUI.
//!
//! [`App`] is the single source of mutable state for the TUI. All panels read
//! from it; the refresh loop writes to it. No I/O is performed in this module.

use agent_team_mail_core::daemon_client::AgentSummary;
use agent_team_mail_core::schema::InboxMessage;
use std::path::PathBuf;

use crate::config::TuiConfig;

/// A single row shown in the Dashboard panel.
#[derive(Debug, Clone)]
pub struct MemberRow {
    /// Agent identifier (e.g. `"arch-ctm"`).
    pub agent: String,
    /// Current state string (e.g. `"idle"`, `"busy"`).
    pub state: String,
    /// Number of messages currently in the agent's inbox file.
    pub inbox_count: usize,
}

/// Which panel currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusPanel {
    /// Left panel — member list.
    #[default]
    Dashboard,
    /// Right panel — agent terminal stream.
    AgentTerminal,
    /// Right panel — structured log viewer (toggled with `L`).
    LogViewer,
}

/// A pending control action to dispatch on the next main-loop iteration.
///
/// Set by the event handler; consumed and cleared by the control dispatch block
/// in `run_app`.
#[derive(Debug, Clone)]
pub enum PendingControl {
    /// Inject text into the selected agent's stdin.
    Stdin(String),
    /// Send an interrupt signal to the selected agent.
    Interrupt,
    /// Send an elicitation/approval decision via correlated proxy routing.
    ElicitationResponse {
        elicitation_id: String,
        decision: String,
        text: Option<String>,
    },
    /// Mark an inbox message as read for the selected agent.
    MarkInboxRead {
        agent: String,
        message_id: Option<String>,
        from: String,
        timestamp: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPromptKind {
    Exec,
    Patch,
    UserInput,
    Review,
}

#[derive(Debug, Clone)]
pub struct ApprovalPrompt {
    pub id: String,
    pub kind: ApprovalPromptKind,
    pub prompt: String,
}

/// Top-level application state.
///
/// Owned by the main event loop. The UI renders this state; the refresh ticker
/// updates it. Access is single-threaded (no interior mutability required).
pub struct App {
    /// Team name being monitored.
    pub team: String,
    /// Member rows shown in the dashboard left panel.
    pub members: Vec<MemberRow>,
    /// Recent inbox message previews for the selected agent.
    pub inbox_preview: Vec<String>,
    /// Recent inbox messages for the selected agent (newest first).
    pub inbox_messages: Vec<InboxMessage>,
    /// Index into [`inbox_messages`](Self::inbox_messages).
    pub selected_message_index: usize,
    /// Whether the inbox detail view is open for the selected message.
    pub inbox_detail_open: bool,
    /// Index into [`members`](Self::members) of the currently selected agent.
    pub selected_index: usize,
    /// Raw agent list returned by the daemon `list-agents` command.
    pub agent_list: Vec<AgentSummary>,
    /// Log lines collected from the selected agent's session log (bounded to 1000).
    pub stream_lines: Vec<String>,
    /// File-read byte position for incremental log tailing.
    pub stream_pos: u64,
    /// Path to the selected agent's session log file, if resolved.
    pub session_log_path: Option<PathBuf>,
    /// Set to `true` to exit the event loop on the next iteration.
    pub should_quit: bool,
    /// Which panel currently holds keyboard focus.
    pub focus: FocusPanel,
    /// Name of the agent whose session is currently being streamed.
    pub streaming_agent: Option<String>,
    /// Current text in the Agent Terminal control input field.
    pub control_input: String,
    /// Whether the control input field currently has focus for text entry.
    ///
    /// Reserved for future use — the D.2 implementation does not yet differentiate
    /// between panel focus and explicit input activation within the Agent Terminal.
    #[expect(
        dead_code,
        reason = "Reserved for D.3 input-activation UX; not yet wired to render"
    )]
    pub control_input_active: bool,
    /// Message shown in the status bar (replaced on the next control result).
    pub status_message: Option<String>,
    /// Pending control action to execute on the next loop iteration.
    pub pending_control: Option<PendingControl>,
    /// Set when the stream source is unavailable (log file missing or unreadable
    /// after streaming had started). Cleared on the next successful read.
    ///
    /// The UI renders a `[FROZEN]` indicator in the stream pane when this is set.
    pub stream_source_error: Option<String>,
    /// Whether the log viewer panel is currently shown (toggled with `L`).
    pub log_viewer_visible: bool,
    /// Structured log events loaded into the log viewer buffer (bounded to 500).
    pub log_events: Vec<agent_team_mail_core::logging_event::LogEventV1>,
    /// Log viewer scroll offset.
    pub log_scroll_offset: usize,
    /// Whether log viewer auto-follows new events.
    pub log_follow_mode: bool,
    /// Active log level filter (`None` = all levels).
    pub log_level_filter: Option<String>,
    /// Byte position for incremental log file reading.
    pub log_viewer_pos: u64,
    /// Active agent filter for the log viewer (`None` = all agents).
    pub log_agent_filter: Option<String>,
    /// User preferences loaded from `~/.config/atm/tui.toml` at startup.
    pub config: TuiConfig,
    /// When `true`, a `Ctrl-I` was pressed while [`InterruptPolicy::Confirm`] is
    /// active. The status bar shows `"Send interrupt? [y/N]"` and the next
    /// `y`/`Enter` dispatches the interrupt; `n`/`Esc` cancels.
    pub confirm_interrupt_pending: bool,
    /// Whether the stream pane auto-scrolls to the latest line on each append.
    ///
    /// Toggled at runtime with `F`. Initialized from
    /// [`TuiConfig::follow_mode_default`].
    pub follow_mode: bool,
    /// Current scroll offset for the stream pane, counted from the top of
    /// [`stream_lines`](Self::stream_lines).
    ///
    /// When [`follow_mode`](Self::follow_mode) is `true` this is updated
    /// automatically on every [`append_stream_lines`](Self::append_stream_lines)
    /// call so the view stays pinned to the bottom. When follow mode is off the
    /// value is preserved, allowing the user to read earlier output.
    pub stream_scroll_offset: usize,
    /// Turn state for the currently streaming agent, sourced from the daemon's
    /// normalised stream event pipeline.
    ///
    /// Updated during the 2-second daemon refresh cycle via the
    /// `"agent-stream-state"` socket command. `None` when the daemon has no
    /// stream state recorded for the agent.
    pub daemon_turn_state: Option<agent_team_mail_core::daemon_stream::AgentStreamState>,
    /// Path to direct watch-stream feed emitted by `atm-agent-mcp`.
    pub watch_stream_path: Option<PathBuf>,
    /// File-read byte position for incremental watch-stream tailing.
    pub watch_stream_pos: u64,
    /// Last session/thread identifier seen in watch frames.
    pub watch_session_id: Option<String>,
    /// Last model identifier seen in watch frames.
    pub watch_model: Option<String>,
    /// Last context window usage percent seen in watch frames.
    pub watch_context_window_pct: Option<f64>,
    /// Last transport observed from live daemon stream events.
    pub watch_transport: Option<String>,
    /// Last turn id observed from live daemon stream events.
    pub watch_turn_id: Option<String>,
    /// Count of `turn_started` events observed in this watch session.
    pub watch_turn_started: u64,
    /// Count of `turn_completed(status=completed)` events observed.
    pub watch_turn_completed: u64,
    /// Count of `turn_completed(status=interrupted)` events observed.
    pub watch_turn_interrupted: u64,
    /// Count of `turn_completed(status=failed)` events observed.
    pub watch_turn_failed: u64,
    /// Latest dropped-event counter value from stream telemetry.
    pub watch_dropped: u64,
    /// Latest unknown-event counter value from stream telemetry.
    pub watch_unknown: u64,
    /// Active approval/elicitation prompt detected from stream events.
    pub approval_prompt: Option<ApprovalPrompt>,
    /// Optional user-entered text for approval/elicitation response.
    pub approval_input: String,
}

impl App {
    /// Create a new [`App`] for the given team with the provided user config.
    ///
    /// `follow_mode` is initialised from [`TuiConfig::follow_mode_default`].
    pub fn new(team: String, config: TuiConfig) -> Self {
        let follow_mode = config.follow_mode_default;
        Self {
            team,
            members: Vec::new(),
            inbox_preview: Vec::new(),
            inbox_messages: Vec::new(),
            selected_message_index: 0,
            inbox_detail_open: false,
            selected_index: 0,
            agent_list: Vec::new(),
            stream_lines: Vec::new(),
            stream_pos: 0,
            session_log_path: None,
            should_quit: false,
            focus: FocusPanel::default(),
            streaming_agent: None,
            control_input: String::new(),
            control_input_active: false,
            status_message: None,
            pending_control: None,
            stream_source_error: None,
            config,
            confirm_interrupt_pending: false,
            follow_mode,
            stream_scroll_offset: 0,
            daemon_turn_state: None,
            watch_stream_path: None,
            watch_stream_pos: 0,
            watch_session_id: None,
            watch_model: None,
            watch_context_window_pct: None,
            watch_transport: None,
            watch_turn_id: None,
            watch_turn_started: 0,
            watch_turn_completed: 0,
            watch_turn_interrupted: 0,
            watch_turn_failed: 0,
            watch_dropped: 0,
            watch_unknown: 0,
            approval_prompt: None,
            approval_input: String::new(),
            log_viewer_visible: false,
            log_events: Vec::new(),
            log_scroll_offset: 0,
            log_follow_mode: true,
            log_level_filter: None,
            log_viewer_pos: 0,
            log_agent_filter: None,
        }
    }

    /// Return the agent name at the currently selected index, if any.
    pub fn selected_agent(&self) -> Option<&str> {
        self.members
            .get(self.selected_index)
            .map(|r| r.agent.as_str())
    }

    /// Return the currently selected inbox message, if any.
    pub fn selected_message(&self) -> Option<&InboxMessage> {
        self.inbox_messages.get(self.selected_message_index)
    }

    /// Move selection up one row (wraps).
    pub fn select_previous(&mut self) {
        if self.members.is_empty() {
            return;
        }
        if self.selected_index == 0 {
            self.selected_index = self.members.len() - 1;
        } else {
            self.selected_index -= 1;
        }
    }

    /// Move selection down one row (wraps).
    pub fn select_next(&mut self) {
        if self.members.is_empty() {
            return;
        }
        self.selected_index = (self.selected_index + 1) % self.members.len();
    }

    /// Move selected inbox message down one row (wraps).
    pub fn select_next_message(&mut self) {
        if self.inbox_messages.is_empty() {
            return;
        }
        self.selected_message_index = (self.selected_message_index + 1) % self.inbox_messages.len();
    }

    /// Move selected inbox message up one row (wraps).
    pub fn select_previous_message(&mut self) {
        if self.inbox_messages.is_empty() {
            return;
        }
        if self.selected_message_index == 0 {
            self.selected_message_index = self.inbox_messages.len() - 1;
        } else {
            self.selected_message_index -= 1;
        }
    }

    /// Cycle focus: Dashboard → AgentTerminal → LogViewer → Dashboard.
    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPanel::Dashboard => FocusPanel::AgentTerminal,
            FocusPanel::AgentTerminal => FocusPanel::LogViewer,
            FocusPanel::LogViewer => FocusPanel::Dashboard,
        };
    }

    /// Append new log lines to [`stream_lines`](Self::stream_lines), keeping
    /// the buffer bounded to the last 1000 lines.
    ///
    /// When [`follow_mode`](Self::follow_mode) is `true`,
    /// [`stream_scroll_offset`](Self::stream_scroll_offset) is updated so the
    /// stream pane remains pinned to the bottom of the buffer. When follow mode
    /// is `false` the offset is preserved as-is.
    pub fn append_stream_lines(&mut self, new_lines: Vec<String>) {
        self.stream_lines.extend(new_lines);
        const MAX_LINES: usize = 1000;
        if self.stream_lines.len() > MAX_LINES {
            let drain_count = self.stream_lines.len() - MAX_LINES;
            self.stream_lines.drain(..drain_count);
        }
        if self.follow_mode {
            // The UI renders the last `visible_height` lines; the offset is the
            // index into stream_lines where the visible window starts. Pinning
            // to `len()` here lets the draw function clamp correctly.
            self.stream_scroll_offset = self.stream_lines.len();
        }
    }

    /// Reset stream state when switching to a different agent.
    pub fn reset_stream(&mut self) {
        self.stream_lines.clear();
        self.stream_pos = 0;
        self.session_log_path = None;
        self.stream_source_error = None;
        self.stream_scroll_offset = 0;
        self.watch_stream_path = None;
        self.watch_stream_pos = 0;
        self.watch_session_id = None;
        self.watch_model = None;
        self.watch_context_window_pct = None;
        self.watch_transport = None;
        self.watch_turn_id = None;
        self.watch_turn_started = 0;
        self.watch_turn_completed = 0;
        self.watch_turn_interrupted = 0;
        self.watch_turn_failed = 0;
        self.watch_dropped = 0;
        self.watch_unknown = 0;
    }

    /// Apply a direct watch-stream JSON frame to watch metrics.
    pub fn apply_watch_frame(&mut self, frame: &serde_json::Value) {
        let event = frame.get("event").unwrap_or(frame);
        let kind = event
            .pointer("/params/type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        self.maybe_track_approval_prompt(event, kind);
        if let Some(transport) = first_string(event, &["/params/transport"]) {
            self.watch_transport = Some(transport);
        }
        if let Some(turn_id) = first_string(event, &["/params/turn_id", "/params/turnId"]) {
            self.watch_turn_id = Some(turn_id);
        }
        if let Some(session_id) = first_string(
            event,
            &[
                "/params/_meta/threadId",
                "/params/threadId",
                "/params/thread_id",
                "/params/session_id",
                "/params/sessionId",
            ],
        ) {
            self.watch_session_id = Some(session_id);
        }
        if let Some(model) = first_string(
            event,
            &[
                "/params/model",
                "/params/model_name",
                "/params/modelName",
                "/params/usage/model",
            ],
        ) {
            self.watch_model = Some(model);
        }
        if let Some(pct) = first_f64(
            event,
            &[
                "/params/usage/context_window_pct",
                "/params/usage/contextWindowPct",
                "/params/context_window_pct",
                "/params/contextWindowPct",
            ],
        ) {
            self.watch_context_window_pct = Some(pct);
        }

        match kind {
            "turn_started" => {
                self.watch_turn_started = self.watch_turn_started.saturating_add(1);
            }
            "turn_completed" | "task_complete" | "done" => {
                self.watch_turn_completed = self.watch_turn_completed.saturating_add(1);
            }
            "turn_interrupted" | "interrupt" | "cancelled" | "turn_cancelled" => {
                self.watch_turn_interrupted = self.watch_turn_interrupted.saturating_add(1);
            }
            "stream_error" | "error" => {
                self.watch_turn_failed = self.watch_turn_failed.saturating_add(1);
            }
            _ => {
                self.watch_unknown = self.watch_unknown.saturating_add(1);
            }
        }
    }

    /// Append new structured log events to [`log_events`](Self::log_events), keeping
    /// the buffer bounded to the last 500 events.
    ///
    /// When [`log_follow_mode`](Self::log_follow_mode) is `true`,
    /// [`log_scroll_offset`](Self::log_scroll_offset) is updated so the log viewer
    /// remains pinned to the bottom of the buffer.
    pub fn append_log_events(
        &mut self,
        new_events: Vec<agent_team_mail_core::logging_event::LogEventV1>,
    ) {
        self.log_events.extend(new_events);
        const MAX_EVENTS: usize = 500;
        if self.log_events.len() > MAX_EVENTS {
            let drain_count = self.log_events.len() - MAX_EVENTS;
            self.log_events.drain(..drain_count);
        }
        if self.log_follow_mode {
            self.log_scroll_offset = self.log_events.len();
        }
    }

    /// Reset log viewer state: clear events, reset byte position and offsets.
    ///
    /// Reserved for future log viewer reset UX; currently exercised in tests.
    #[allow(dead_code)]
    pub fn reset_log_viewer(&mut self) {
        self.log_events.clear();
        self.log_viewer_pos = 0;
        self.log_scroll_offset = 0;
    }

    /// Cycle the active log level filter.
    ///
    /// Rotation: `None` → `"error"` → `"warn"` → `"info"` → `"debug"` → `None`.
    pub fn cycle_log_level_filter(&mut self) {
        self.log_level_filter = match self.log_level_filter.as_deref() {
            None => Some("error".to_string()),
            Some("error") => Some("warn".to_string()),
            Some("warn") => Some("info".to_string()),
            Some("info") => Some("debug".to_string()),
            _ => None,
        };
    }

    /// Returns `true` if the selected agent is "live" (control input is available).
    ///
    /// Live states are `"idle"` and `"busy"`. All other states — including
    /// `"launching"`, `"killed"`, `"stale"`, `"closed"`, and any unknown value
    /// — are considered not-live.
    pub fn is_live(&self) -> bool {
        self.members
            .get(self.selected_index)
            .map(|m| matches!(m.state.as_str(), "idle" | "busy"))
            .unwrap_or(false)
    }

    /// Returns a human-readable reason why control input is not available, or
    /// `None` if the agent is live.
    pub fn not_live_reason(&self) -> Option<&'static str> {
        match self
            .members
            .get(self.selected_index)
            .map(|m| m.state.as_str())
        {
            Some("launching") => Some("Launching"),
            Some("killed") => Some("Killed"),
            Some("stale") => Some("Stale"),
            Some("closed") => Some("Closed"),
            Some("idle") | Some("busy") => None,
            _ => Some("Not live"),
        }
    }

    /// Resolve transport for status surfaces with explicit precedence:
    /// direct watch-stream fields are authoritative when present; daemon state
    /// is only a coarse fallback when watch metadata is absent.
    pub fn resolved_watch_transport(&self) -> Option<&str> {
        self.watch_transport.as_deref().or_else(|| {
            self.daemon_turn_state
                .as_ref()
                .and_then(|s| s.transport.as_deref())
        })
    }

    /// Resolve turn id for status surfaces using the same precedence as
    /// [`resolved_watch_transport`](Self::resolved_watch_transport).
    pub fn resolved_watch_turn_id(&self) -> Option<&str> {
        self.watch_turn_id.as_deref().or_else(|| {
            self.daemon_turn_state
                .as_ref()
                .and_then(|s| s.turn_id.as_deref())
        })
    }

    /// Resolve session/thread id for status surfaces using the same precedence
    /// as [`resolved_watch_transport`](Self::resolved_watch_transport).
    pub fn resolved_watch_session_id(&self) -> Option<&str> {
        self.watch_session_id.as_deref().or_else(|| {
            self.daemon_turn_state
                .as_ref()
                .and_then(|s| s.thread_id.as_deref())
        })
    }

    fn maybe_track_approval_prompt(&mut self, event: &serde_json::Value, kind: &str) {
        let id = first_string(
            event,
            &[
                "/params/request_id",
                "/params/requestId",
                "/params/item_id",
                "/params/itemId",
                "/params/id",
            ],
        );
        let text = first_string(
            event,
            &[
                "/params/prompt",
                "/params/message",
                "/params/text",
                "/params/output",
                "/params/delta",
            ],
        )
        .unwrap_or_default();
        match kind {
            "exec_approval_request" | "approval_request" | "approval_prompt" => {
                if let Some(id) = id {
                    self.approval_prompt = Some(ApprovalPrompt {
                        id,
                        kind: ApprovalPromptKind::Exec,
                        prompt: text,
                    });
                }
            }
            "apply_patch_approval_request" => {
                if let Some(id) = id {
                    self.approval_prompt = Some(ApprovalPrompt {
                        id,
                        kind: ApprovalPromptKind::Patch,
                        prompt: text,
                    });
                }
            }
            "request_user_input" | "elicitation_request" => {
                if let Some(id) = id {
                    self.approval_prompt = Some(ApprovalPrompt {
                        id,
                        kind: ApprovalPromptKind::UserInput,
                        prompt: text,
                    });
                }
            }
            "entered_review_mode" | "item/enteredReviewMode" => {
                if let Some(id) = id {
                    self.approval_prompt = Some(ApprovalPrompt {
                        id,
                        kind: ApprovalPromptKind::Review,
                        prompt: text,
                    });
                }
            }
            "approval_rejected"
            | "approval_approved"
            | "exited_review_mode"
            | "item/exitedReviewMode" => {
                self.approval_prompt = None;
                self.approval_input.clear();
            }
            _ => {}
        }
    }
}

fn first_string(value: &serde_json::Value, paths: &[&str]) -> Option<String> {
    paths.iter().find_map(|path| {
        value
            .pointer(path)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    })
}

fn first_f64(value: &serde_json::Value, paths: &[&str]) -> Option<f64> {
    paths
        .iter()
        .find_map(|path| value.pointer(path).and_then(|v| v.as_f64()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TuiConfig;

    fn new_app(team: &str) -> App {
        App::new(team.to_string(), TuiConfig::default())
    }

    #[test]
    fn test_select_next_wraps() {
        let mut app = new_app("atm-dev");
        app.members = vec![
            MemberRow {
                agent: "a".into(),
                state: "idle".into(),
                inbox_count: 0,
            },
            MemberRow {
                agent: "b".into(),
                state: "idle".into(),
                inbox_count: 0,
            },
        ];
        app.selected_index = 1;
        app.select_next();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn test_select_previous_wraps() {
        let mut app = new_app("atm-dev");
        app.members = vec![
            MemberRow {
                agent: "a".into(),
                state: "idle".into(),
                inbox_count: 0,
            },
            MemberRow {
                agent: "b".into(),
                state: "idle".into(),
                inbox_count: 0,
            },
        ];
        app.selected_index = 0;
        app.select_previous();
        assert_eq!(app.selected_index, 1);
    }

    #[test]
    fn test_append_stream_lines_bounded() {
        let mut app = new_app("atm-dev");
        // Fill past the 1000-line limit.
        let lines: Vec<String> = (0..1100).map(|i| format!("line {i}")).collect();
        app.append_stream_lines(lines);
        assert_eq!(app.stream_lines.len(), 1000);
        // Should keep the last 1000.
        assert_eq!(app.stream_lines[0], "line 100");
        assert_eq!(app.stream_lines[999], "line 1099");
    }

    #[test]
    fn test_cycle_focus() {
        let mut app = new_app("atm-dev");
        assert_eq!(app.focus, FocusPanel::Dashboard);
        app.cycle_focus();
        assert_eq!(app.focus, FocusPanel::AgentTerminal);
        app.cycle_focus();
        assert_eq!(app.focus, FocusPanel::LogViewer);
        app.cycle_focus();
        assert_eq!(app.focus, FocusPanel::Dashboard);
    }

    #[test]
    fn test_is_live_idle() {
        let mut app = new_app("test");
        app.members = vec![MemberRow {
            agent: "a".into(),
            state: "idle".into(),
            inbox_count: 0,
        }];
        assert!(app.is_live());
    }

    #[test]
    fn test_is_live_busy() {
        let mut app = new_app("test");
        app.members = vec![MemberRow {
            agent: "a".into(),
            state: "busy".into(),
            inbox_count: 0,
        }];
        assert!(app.is_live());
    }

    #[test]
    fn test_not_live_launching() {
        let mut app = new_app("test");
        app.members = vec![MemberRow {
            agent: "a".into(),
            state: "launching".into(),
            inbox_count: 0,
        }];
        assert!(!app.is_live());
        assert_eq!(app.not_live_reason(), Some("Launching"));
    }

    #[test]
    fn test_not_live_killed() {
        let mut app = new_app("test");
        app.members = vec![MemberRow {
            agent: "a".into(),
            state: "killed".into(),
            inbox_count: 0,
        }];
        assert!(!app.is_live());
        assert_eq!(app.not_live_reason(), Some("Killed"));
    }

    #[test]
    fn test_not_live_stale() {
        let mut app = new_app("test");
        app.members = vec![MemberRow {
            agent: "a".into(),
            state: "stale".into(),
            inbox_count: 0,
        }];
        assert!(!app.is_live());
        assert_eq!(app.not_live_reason(), Some("Stale"));
    }

    #[test]
    fn test_not_live_closed() {
        let mut app = new_app("test");
        app.members = vec![MemberRow {
            agent: "a".into(),
            state: "closed".into(),
            inbox_count: 0,
        }];
        assert!(!app.is_live());
        assert_eq!(app.not_live_reason(), Some("Closed"));
    }

    #[test]
    fn test_not_live_no_members() {
        let app = new_app("test");
        assert!(!app.is_live());
        assert_eq!(app.not_live_reason(), Some("Not live"));
    }

    #[test]
    fn test_not_live_unknown_state() {
        let mut app = new_app("test");
        app.members = vec![MemberRow {
            agent: "a".into(),
            state: "unknown-state".into(),
            inbox_count: 0,
        }];
        assert!(!app.is_live());
        assert_eq!(app.not_live_reason(), Some("Not live"));
    }

    #[test]
    fn test_live_reason_is_none_for_idle() {
        let mut app = new_app("test");
        app.members = vec![MemberRow {
            agent: "a".into(),
            state: "idle".into(),
            inbox_count: 0,
        }];
        assert_eq!(app.not_live_reason(), None);
    }

    #[test]
    fn test_live_reason_is_none_for_busy() {
        let mut app = new_app("test");
        app.members = vec![MemberRow {
            agent: "a".into(),
            state: "busy".into(),
            inbox_count: 0,
        }];
        assert_eq!(app.not_live_reason(), None);
    }

    /// Stale agents must not be considered live — TUI must block control input.
    #[test]
    fn test_is_live_returns_false_for_stale_state() {
        let mut app = new_app("atm-dev");
        app.members = vec![MemberRow {
            agent: "arch-ctm".into(),
            state: "stale".into(),
            inbox_count: 0,
        }];
        app.selected_index = 0;
        assert!(!app.is_live(), "stale agent must not be live");
    }

    /// Closed agents must not be considered live — TUI must block control input.
    #[test]
    fn test_is_live_returns_false_for_closed_state() {
        let mut app = new_app("atm-dev");
        app.members = vec![MemberRow {
            agent: "arch-ctm".into(),
            state: "closed".into(),
            inbox_count: 0,
        }];
        app.selected_index = 0;
        assert!(!app.is_live(), "closed agent must not be live");
    }

    // ── Follow mode ───────────────────────────────────────────────────────────

    #[test]
    fn test_follow_mode_initialized_from_config() {
        let cfg = TuiConfig {
            follow_mode_default: false,
            ..Default::default()
        };
        let app = App::new("test".to_string(), cfg);
        assert!(!app.follow_mode, "follow_mode must reflect config default");
    }

    #[test]
    fn test_follow_mode_updates_scroll_offset_on_append() {
        let mut app = new_app("test");
        app.follow_mode = true;
        let lines: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        app.append_stream_lines(lines);
        assert_eq!(
            app.stream_scroll_offset, 50,
            "scroll offset must equal line count when follow mode is on"
        );
    }

    #[test]
    fn test_follow_mode_off_preserves_scroll_offset() {
        let mut app = new_app("test");
        app.follow_mode = false;
        app.stream_scroll_offset = 7;
        let lines: Vec<String> = (0..10).map(|i| format!("line {i}")).collect();
        app.append_stream_lines(lines);
        assert_eq!(
            app.stream_scroll_offset, 7,
            "scroll offset must not change when follow mode is off"
        );
    }

    #[test]
    fn test_reset_stream_clears_scroll_offset() {
        let mut app = new_app("test");
        app.stream_scroll_offset = 42;
        app.reset_stream();
        assert_eq!(
            app.stream_scroll_offset, 0,
            "reset_stream must clear scroll offset"
        );
    }

    #[test]
    fn test_apply_watch_frame_updates_status_surfaces() {
        let mut app = new_app("test");
        let frame = serde_json::json!({
            "event": {
                "params": {
                    "type": "turn_started",
                    "transport": "app-server",
                    "turnId": "turn-7",
                    "_meta": { "threadId": "thread-123" },
                    "model": "gpt-5-codex",
                    "usage": { "contextWindowPct": 72.4 }
                }
            }
        });
        app.apply_watch_frame(&frame);
        assert_eq!(app.watch_transport.as_deref(), Some("app-server"));
        assert_eq!(app.watch_turn_id.as_deref(), Some("turn-7"));
        assert_eq!(app.watch_session_id.as_deref(), Some("thread-123"));
        assert_eq!(app.watch_model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(app.watch_context_window_pct, Some(72.4));
        assert_eq!(app.watch_turn_started, 1);
    }

    #[test]
    fn test_apply_watch_frame_preserves_transport_when_event_omits_it() {
        let mut app = new_app("test");
        app.watch_transport = Some("mcp".to_string());
        app.watch_turn_id = Some("old-turn".to_string());
        let frame = serde_json::json!({
            "event": { "params": { "type": "item_delta", "delta": "hello" } }
        });
        app.apply_watch_frame(&frame);
        assert_eq!(
            app.watch_transport.as_deref(),
            Some("mcp"),
            "transport must remain stable when omitted by partial events"
        );
        assert_eq!(
            app.watch_turn_id.as_deref(),
            Some("old-turn"),
            "turn id must remain stable when omitted by partial events"
        );
    }

    #[test]
    fn test_reset_stream_clears_watch_status_surfaces() {
        let mut app = new_app("test");
        app.watch_session_id = Some("thread-1".to_string());
        app.watch_model = Some("gpt-5".to_string());
        app.watch_context_window_pct = Some(66.0);
        app.watch_transport = Some("mcp".to_string());
        app.watch_turn_id = Some("turn-1".to_string());
        app.reset_stream();
        assert!(app.watch_session_id.is_none());
        assert!(app.watch_model.is_none());
        assert!(app.watch_context_window_pct.is_none());
        assert!(app.watch_transport.is_none());
        assert!(app.watch_turn_id.is_none());
    }

    #[test]
    fn test_watch_status_precedence_prefers_direct_stream_values() {
        use agent_team_mail_core::daemon_stream::{AgentStreamState, StreamTurnStatus};
        let mut app = new_app("test");
        app.watch_transport = Some("mcp".to_string());
        app.watch_turn_id = Some("turn-watch".to_string());
        app.watch_session_id = Some("thread-watch".to_string());
        app.daemon_turn_state = Some(AgentStreamState {
            transport: Some("app-server".to_string()),
            turn_id: Some("turn-daemon".to_string()),
            thread_id: Some("thread-daemon".to_string()),
            turn_status: StreamTurnStatus::Busy,
        });
        assert_eq!(app.resolved_watch_transport(), Some("mcp"));
        assert_eq!(app.resolved_watch_turn_id(), Some("turn-watch"));
        assert_eq!(app.resolved_watch_session_id(), Some("thread-watch"));
    }

    #[test]
    fn test_watch_status_falls_back_to_daemon_when_watch_missing() {
        use agent_team_mail_core::daemon_stream::{AgentStreamState, StreamTurnStatus};
        let mut app = new_app("test");
        app.daemon_turn_state = Some(AgentStreamState {
            transport: Some("app-server".to_string()),
            turn_id: Some("turn-daemon".to_string()),
            thread_id: Some("thread-daemon".to_string()),
            turn_status: StreamTurnStatus::Busy,
        });
        assert_eq!(app.resolved_watch_transport(), Some("app-server"));
        assert_eq!(app.resolved_watch_turn_id(), Some("turn-daemon"));
        assert_eq!(app.resolved_watch_session_id(), Some("thread-daemon"));
    }

    #[test]
    fn test_reset_and_replay_restore_watch_status_coherently() {
        let mut app = new_app("test");
        let replay_frame = serde_json::json!({
            "event": {
                "params": {
                    "type": "turn_started",
                    "transport": "mcp",
                    "turnId": "turn-attach",
                    "_meta": { "threadId": "thread-attach" },
                    "model": "gpt-5-codex",
                    "usage": { "contextWindowPct": 64.0 }
                }
            }
        });
        app.apply_watch_frame(&replay_frame);
        let before = (
            app.watch_transport.clone(),
            app.watch_turn_id.clone(),
            app.watch_session_id.clone(),
            app.watch_model.clone(),
            app.watch_context_window_pct,
        );

        app.reset_stream();
        app.apply_watch_frame(&replay_frame);
        let after = (
            app.watch_transport.clone(),
            app.watch_turn_id.clone(),
            app.watch_session_id.clone(),
            app.watch_model.clone(),
            app.watch_context_window_pct,
        );

        assert_eq!(
            before, after,
            "re-attach replay must restore watch status surfaces after detach/reset"
        );
    }

    #[test]
    fn test_transport_mode_switch_does_not_leave_stale_transport_after_reset() {
        let mut app = new_app("test");
        app.watch_transport = Some("mcp".to_string());
        app.reset_stream();
        let transportless = serde_json::json!({
            "event": { "params": { "type": "item_delta", "delta": "hello" } }
        });
        app.apply_watch_frame(&transportless);
        assert!(
            app.watch_transport.is_none(),
            "transport-less events after reset must not retain stale transport"
        );
    }

    /// Stress test: append 10,000 lines in rapid succession.
    /// Validates that the 1000-line bound is enforced and no panic occurs.
    /// SLO: append + bound enforcement < 200ms for 10k lines.
    #[test]
    fn test_stress_stream_append_bounded() {
        let mut app = new_app("test");
        let start = std::time::Instant::now();
        let lines: Vec<String> = (0..10_000).map(|i| format!("line {i}")).collect();
        // Simulate batched appends as would happen in production
        for chunk in lines.chunks(100) {
            app.append_stream_lines(chunk.to_vec());
        }
        let elapsed = start.elapsed();
        assert_eq!(
            app.stream_lines.len(),
            1000,
            "buffer must stay bounded at 1000"
        );
        assert!(
            elapsed.as_millis() < 200,
            "10k line stress append must complete in <200ms, took {elapsed:?}"
        );
    }

    // ── G.7 daemon_turn_state tests ──────────────────────────────────────────

    #[test]
    fn test_daemon_turn_state_defaults_to_none() {
        let app = new_app("test");
        assert!(
            app.daemon_turn_state.is_none(),
            "daemon_turn_state should default to None"
        );
    }

    #[test]
    fn test_daemon_turn_state_busy_display() {
        use agent_team_mail_core::daemon_stream::{AgentStreamState, StreamTurnStatus};
        let mut app = new_app("test");
        app.daemon_turn_state = Some(AgentStreamState {
            turn_id: Some("t-1".to_string()),
            thread_id: Some("th-1".to_string()),
            transport: Some("app-server".to_string()),
            turn_status: StreamTurnStatus::Busy,
        });
        let state = app.daemon_turn_state.as_ref().unwrap();
        assert_eq!(state.turn_status, StreamTurnStatus::Busy);
        assert_eq!(format!("{}", state.turn_status), "busy");
    }

    #[test]
    fn test_daemon_turn_state_terminal_display() {
        use agent_team_mail_core::daemon_stream::{AgentStreamState, StreamTurnStatus};
        let mut app = new_app("test");
        app.daemon_turn_state = Some(AgentStreamState {
            turn_id: Some("t-2".to_string()),
            thread_id: None,
            transport: Some("cli-json".to_string()),
            turn_status: StreamTurnStatus::Terminal,
        });
        let state = app.daemon_turn_state.as_ref().unwrap();
        assert_eq!(state.turn_status, StreamTurnStatus::Terminal);
        assert_eq!(format!("{}", state.turn_status), "terminal");
    }

    #[test]
    fn test_daemon_turn_state_idle_display() {
        use agent_team_mail_core::daemon_stream::{AgentStreamState, StreamTurnStatus};
        let mut app = new_app("test");
        app.daemon_turn_state = Some(AgentStreamState {
            turn_status: StreamTurnStatus::Idle,
            ..Default::default()
        });
        let state = app.daemon_turn_state.as_ref().unwrap();
        assert_eq!(state.turn_status, StreamTurnStatus::Idle);
        assert_eq!(format!("{}", state.turn_status), "idle");
    }

    // ── Log viewer state tests ────────────────────────────────────────────────

    #[test]
    fn test_log_viewer_visible_defaults_false() {
        let app = new_app("test");
        assert!(
            !app.log_viewer_visible,
            "log_viewer_visible must default to false"
        );
    }

    #[test]
    fn test_append_log_events_bounded_to_500() {
        use agent_team_mail_core::logging_event::new_log_event;
        let mut app = new_app("test");
        let events: Vec<_> = (0..600)
            .map(|i| new_log_event("atm", &format!("action_{i}"), "atm::test", "info"))
            .collect();
        app.append_log_events(events);
        assert_eq!(
            app.log_events.len(),
            500,
            "log_events must be bounded to 500"
        );
        // Should keep the last 500 (indices 100..=599 → actions 100..=599).
        assert_eq!(app.log_events[0].action, "action_100");
        assert_eq!(app.log_events[499].action, "action_599");
    }

    #[test]
    fn test_cycle_log_level_filter_cycles() {
        let mut app = new_app("test");
        assert!(
            app.log_level_filter.is_none(),
            "initial filter must be None"
        );
        app.cycle_log_level_filter();
        assert_eq!(app.log_level_filter.as_deref(), Some("error"));
        app.cycle_log_level_filter();
        assert_eq!(app.log_level_filter.as_deref(), Some("warn"));
        app.cycle_log_level_filter();
        assert_eq!(app.log_level_filter.as_deref(), Some("info"));
        app.cycle_log_level_filter();
        assert_eq!(app.log_level_filter.as_deref(), Some("debug"));
        app.cycle_log_level_filter();
        assert!(
            app.log_level_filter.is_none(),
            "filter must wrap back to None"
        );
    }

    #[test]
    fn test_log_follow_mode_updates_scroll_offset() {
        use agent_team_mail_core::logging_event::new_log_event;
        let mut app = new_app("test");
        app.log_follow_mode = true;
        let events: Vec<_> = (0..30)
            .map(|i| new_log_event("atm", &format!("act_{i}"), "atm::test", "info"))
            .collect();
        app.append_log_events(events);
        assert_eq!(
            app.log_scroll_offset, 30,
            "scroll offset must equal event count when log_follow_mode is on"
        );
    }

    #[test]
    fn test_log_follow_mode_off_preserves_offset() {
        use agent_team_mail_core::logging_event::new_log_event;
        let mut app = new_app("test");
        app.log_follow_mode = false;
        app.log_scroll_offset = 5;
        let events: Vec<_> = (0..10)
            .map(|i| new_log_event("atm", &format!("act_{i}"), "atm::test", "info"))
            .collect();
        app.append_log_events(events);
        assert_eq!(
            app.log_scroll_offset, 5,
            "scroll offset must not change when log_follow_mode is off"
        );
    }

    #[test]
    fn test_reset_log_viewer_clears_state() {
        use agent_team_mail_core::logging_event::new_log_event;
        let mut app = new_app("test");
        app.log_events
            .push(new_log_event("atm", "act", "atm::test", "info"));
        app.log_viewer_pos = 42;
        app.log_scroll_offset = 7;
        app.reset_log_viewer();
        assert!(app.log_events.is_empty(), "reset must clear log_events");
        assert_eq!(app.log_viewer_pos, 0, "reset must clear log_viewer_pos");
        assert_eq!(
            app.log_scroll_offset, 0,
            "reset must clear log_scroll_offset"
        );
    }

    #[test]
    fn test_cycle_focus_includes_log_viewer() {
        let mut app = new_app("test");
        // Full cycle: Dashboard → AgentTerminal → LogViewer → Dashboard
        assert_eq!(app.focus, FocusPanel::Dashboard);
        app.cycle_focus();
        assert_eq!(app.focus, FocusPanel::AgentTerminal);
        app.cycle_focus();
        assert_eq!(app.focus, FocusPanel::LogViewer);
        app.cycle_focus();
        assert_eq!(
            app.focus,
            FocusPanel::Dashboard,
            "must wrap back to Dashboard"
        );
    }

    #[test]
    fn test_apply_watch_frame_tracks_exec_approval_prompt() {
        let mut app = new_app("atm-dev");
        let frame = serde_json::json!({
            "event": {
                "params": {
                    "type": "exec_approval_request",
                    "request_id": "req-42",
                    "message": "allow command?"
                }
            }
        });
        app.apply_watch_frame(&frame);
        let prompt = app.approval_prompt.expect("prompt should be captured");
        assert_eq!(prompt.id, "req-42");
        assert_eq!(prompt.kind, ApprovalPromptKind::Exec);
        assert_eq!(prompt.prompt, "allow command?");
    }

    #[test]
    fn test_apply_watch_frame_clears_prompt_on_resolution() {
        let mut app = new_app("atm-dev");
        app.approval_prompt = Some(ApprovalPrompt {
            id: "req-9".to_string(),
            kind: ApprovalPromptKind::Review,
            prompt: "review".to_string(),
        });
        app.approval_input = "notes".to_string();
        let frame = serde_json::json!({
            "event": {
                "params": { "type": "approval_approved" }
            }
        });
        app.apply_watch_frame(&frame);
        assert!(app.approval_prompt.is_none());
        assert!(app.approval_input.is_empty());
    }
}
