//! Application state machine for the ATM TUI.
//!
//! [`App`] is the single source of mutable state for the TUI. All panels read
//! from it; the refresh loop writes to it. No I/O is performed in this module.

use std::path::PathBuf;

use agent_team_mail_core::daemon_client::AgentSummary;

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
    #[expect(dead_code, reason = "Reserved for D.3 input-activation UX; not yet wired to render")]
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
        }
    }

    /// Return the agent name at the currently selected index, if any.
    pub fn selected_agent(&self) -> Option<&str> {
        self.members.get(self.selected_index).map(|r| r.agent.as_str())
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

    /// Cycle focus between Dashboard and AgentTerminal panels.
    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPanel::Dashboard => FocusPanel::AgentTerminal,
            FocusPanel::AgentTerminal => FocusPanel::Dashboard,
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
            MemberRow { agent: "a".into(), state: "idle".into(), inbox_count: 0 },
            MemberRow { agent: "b".into(), state: "idle".into(), inbox_count: 0 },
        ];
        app.selected_index = 1;
        app.select_next();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn test_select_previous_wraps() {
        let mut app = new_app("atm-dev");
        app.members = vec![
            MemberRow { agent: "a".into(), state: "idle".into(), inbox_count: 0 },
            MemberRow { agent: "b".into(), state: "idle".into(), inbox_count: 0 },
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
        assert_eq!(app.focus, FocusPanel::Dashboard);
    }

    #[test]
    fn test_is_live_idle() {
        let mut app = new_app("test");
        app.members =
            vec![MemberRow { agent: "a".into(), state: "idle".into(), inbox_count: 0 }];
        assert!(app.is_live());
    }

    #[test]
    fn test_is_live_busy() {
        let mut app = new_app("test");
        app.members =
            vec![MemberRow { agent: "a".into(), state: "busy".into(), inbox_count: 0 }];
        assert!(app.is_live());
    }

    #[test]
    fn test_not_live_launching() {
        let mut app = new_app("test");
        app.members =
            vec![MemberRow { agent: "a".into(), state: "launching".into(), inbox_count: 0 }];
        assert!(!app.is_live());
        assert_eq!(app.not_live_reason(), Some("Launching"));
    }

    #[test]
    fn test_not_live_killed() {
        let mut app = new_app("test");
        app.members =
            vec![MemberRow { agent: "a".into(), state: "killed".into(), inbox_count: 0 }];
        assert!(!app.is_live());
        assert_eq!(app.not_live_reason(), Some("Killed"));
    }

    #[test]
    fn test_not_live_stale() {
        let mut app = new_app("test");
        app.members =
            vec![MemberRow { agent: "a".into(), state: "stale".into(), inbox_count: 0 }];
        assert!(!app.is_live());
        assert_eq!(app.not_live_reason(), Some("Stale"));
    }

    #[test]
    fn test_not_live_closed() {
        let mut app = new_app("test");
        app.members =
            vec![MemberRow { agent: "a".into(), state: "closed".into(), inbox_count: 0 }];
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
        app.members =
            vec![MemberRow { agent: "a".into(), state: "unknown-state".into(), inbox_count: 0 }];
        assert!(!app.is_live());
        assert_eq!(app.not_live_reason(), Some("Not live"));
    }

    #[test]
    fn test_live_reason_is_none_for_idle() {
        let mut app = new_app("test");
        app.members =
            vec![MemberRow { agent: "a".into(), state: "idle".into(), inbox_count: 0 }];
        assert_eq!(app.not_live_reason(), None);
    }

    #[test]
    fn test_live_reason_is_none_for_busy() {
        let mut app = new_app("test");
        app.members =
            vec![MemberRow { agent: "a".into(), state: "busy".into(), inbox_count: 0 }];
        assert_eq!(app.not_live_reason(), None);
    }

    /// Stale agents must not be considered live — TUI must block control input.
    #[test]
    fn test_is_live_returns_false_for_stale_state() {
        let mut app = new_app("atm-dev");
        app.members = vec![
            MemberRow { agent: "arch-ctm".into(), state: "stale".into(), inbox_count: 0 },
        ];
        app.selected_index = 0;
        assert!(!app.is_live(), "stale agent must not be live");
    }

    /// Closed agents must not be considered live — TUI must block control input.
    #[test]
    fn test_is_live_returns_false_for_closed_state() {
        let mut app = new_app("atm-dev");
        app.members = vec![
            MemberRow { agent: "arch-ctm".into(), state: "closed".into(), inbox_count: 0 },
        ];
        app.selected_index = 0;
        assert!(!app.is_live(), "closed agent must not be live");
    }

    // ── Follow mode ───────────────────────────────────────────────────────────

    #[test]
    fn test_follow_mode_initialized_from_config() {
        let cfg = TuiConfig { follow_mode_default: false, ..Default::default() };
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
        assert_eq!(app.stream_scroll_offset, 0, "reset_stream must clear scroll offset");
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
        assert_eq!(app.stream_lines.len(), 1000, "buffer must stay bounded at 1000");
        assert!(
            elapsed.as_millis() < 200,
            "10k line stress append must complete in <200ms, took {elapsed:?}"
        );
    }
}
