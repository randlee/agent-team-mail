//! Application state machine for the ATM TUI.
//!
//! [`App`] is the single source of mutable state for the TUI. All panels read
//! from it; the refresh loop writes to it. No I/O is performed in this module.

use std::path::PathBuf;

use agent_team_mail_core::daemon_client::AgentSummary;

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
}

impl App {
    /// Create a new [`App`] for the given team.
    pub fn new(team: String) -> Self {
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
    pub fn append_stream_lines(&mut self, new_lines: Vec<String>) {
        self.stream_lines.extend(new_lines);
        const MAX_LINES: usize = 1000;
        if self.stream_lines.len() > MAX_LINES {
            let drain_count = self.stream_lines.len() - MAX_LINES;
            self.stream_lines.drain(..drain_count);
        }
    }

    /// Reset stream state when switching to a different agent.
    pub fn reset_stream(&mut self) {
        self.stream_lines.clear();
        self.stream_pos = 0;
        self.session_log_path = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_next_wraps() {
        let mut app = App::new("atm-dev".to_string());
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
        let mut app = App::new("atm-dev".to_string());
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
        let mut app = App::new("atm-dev".to_string());
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
        let mut app = App::new("atm-dev".to_string());
        assert_eq!(app.focus, FocusPanel::Dashboard);
        app.cycle_focus();
        assert_eq!(app.focus, FocusPanel::AgentTerminal);
        app.cycle_focus();
        assert_eq!(app.focus, FocusPanel::Dashboard);
    }
}
