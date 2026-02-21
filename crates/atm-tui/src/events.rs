//! Keyboard input event handling for the ATM TUI.
//!
//! Events are consumed in the main loop. The handler mutates [`App`] state
//! directly; rendering happens separately.
//!
//! # Key Bindings
//!
//! ## Global (any focus)
//!
//! | Key | Action |
//! |-----|--------|
//! | `q` | Quit |
//! | `Ctrl-C` | Quit |
//! | `↑` | Move selection up |
//! | `↓` | Move selection down |
//! | `Tab` | Cycle panel focus |
//!
//! ## Agent Terminal panel (when selected agent is live)
//!
//! | Key | Action |
//! |-----|--------|
//! | _printable char_ | Append to control input |
//! | `Enter` | Submit stdin text (non-empty) |
//! | `Backspace` | Delete last character |
//! | `Ctrl-K` | Send interrupt |
//! | `Esc` | Clear control input |
//!
//! Dashboard panel ignores character input — it is mail-only.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, FocusPanel, PendingControl};

/// Process a single terminal input event and update [`App`] state accordingly.
///
/// Returns `true` if the application should quit after this event.
pub fn handle_event(event: &Event, app: &mut App) -> bool {
    if let Event::Key(KeyEvent { code, modifiers, .. }) = event {
        // ── Global bindings ───────────────────────────────────────────────────
        match (code, modifiers) {
            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                app.should_quit = true;
                return true;
            }
            (KeyCode::Up, _) => {
                app.select_previous();
                return false;
            }
            (KeyCode::Down, _) => {
                app.select_next();
                return false;
            }
            (KeyCode::Tab, _) => {
                app.cycle_focus();
                return false;
            }
            _ => {}
        }

        // ── Panel-specific bindings ───────────────────────────────────────────
        return match app.focus {
            FocusPanel::AgentTerminal => handle_agent_terminal_key(code, modifiers, app),
            FocusPanel::Dashboard => handle_dashboard_key(code, app),
        };
    }
    false
}

/// Handle keys while the Agent Terminal panel is focused.
fn handle_agent_terminal_key(
    code: &KeyCode,
    modifiers: &KeyModifiers,
    app: &mut App,
) -> bool {
    // Ctrl-K sends interrupt (preferred to avoid Ctrl-I/Tab collision).
    // Ctrl-I is accepted as a legacy fallback when distinguishable.
    // When not live, the interrupt is dropped client-side to avoid sending
    // to a session that cannot receive input.
    if (matches!(code, KeyCode::Char('k')) || matches!(code, KeyCode::Char('i')))
        && modifiers.contains(KeyModifiers::CONTROL)
    {
        if app.is_live() {
            app.pending_control = Some(PendingControl::Interrupt);
        }
        return false;
    }

    // Esc → clear control input
    if matches!(code, KeyCode::Esc) {
        app.control_input.clear();
        return false;
    }

    // Backspace → delete last character
    if matches!(code, KeyCode::Backspace) {
        app.control_input.pop();
        return false;
    }

    // Enter → submit if non-empty and agent is live
    if matches!(code, KeyCode::Enter) {
        let text = app.control_input.trim().to_string();
        if !text.is_empty() && app.is_live() {
            app.pending_control = Some(PendingControl::Stdin(text));
            app.control_input.clear();
        }
        return false;
    }

    // Printable character → quit shortcut or append to input
    if let KeyCode::Char(c) = code {
        // 'q' in the agent terminal still quits when no modifiers are held
        // and control_input is empty (consistent UX: once typing starts, 'q'
        // is just a character).
        if *c == 'q' && modifiers.is_empty() && app.control_input.is_empty() {
            app.should_quit = true;
            return true;
        }
        // Append character — only when agent is live (or user is typing for
        // an agent that became not-live mid-edit; input is allowed to accumulate
        // but Enter will be rejected).
        if app.is_live() {
            app.control_input.push(*c);
        }
    }

    false
}

/// Handle keys while the Dashboard panel is focused.
///
/// The Dashboard is mail-only: character input is ignored here. Navigation
/// keys are handled globally before this function is reached.
fn handle_dashboard_key(code: &KeyCode, app: &mut App) -> bool {
    if let KeyCode::Char('q') = code {
        app.should_quit = true;
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{FocusPanel, MemberRow};
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key_event(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    fn app_with_members() -> App {
        let mut app = App::new("atm-dev".to_string());
        app.members = vec![
            MemberRow { agent: "a".into(), state: "idle".into(), inbox_count: 0 },
            MemberRow { agent: "b".into(), state: "busy".into(), inbox_count: 1 },
            MemberRow { agent: "c".into(), state: "idle".into(), inbox_count: 2 },
        ];
        app
    }

    // ── Global bindings ───────────────────────────────────────────────────────

    #[test]
    fn test_q_quits_on_dashboard() {
        let mut app = App::new("atm-dev".to_string());
        assert_eq!(app.focus, FocusPanel::Dashboard);
        let quit = handle_event(&key_event(KeyCode::Char('q'), KeyModifiers::NONE), &mut app);
        assert!(quit);
        assert!(app.should_quit);
    }

    #[test]
    fn test_q_quits_on_agent_terminal_when_input_empty() {
        let mut app = app_with_members();
        app.focus = FocusPanel::AgentTerminal;
        // With empty input, 'q' should quit.
        let quit = handle_event(&key_event(KeyCode::Char('q'), KeyModifiers::NONE), &mut app);
        assert!(quit);
        assert!(app.should_quit);
    }

    #[test]
    fn test_ctrl_c_quits() {
        let mut app = App::new("atm-dev".to_string());
        let quit = handle_event(&key_event(KeyCode::Char('c'), KeyModifiers::CONTROL), &mut app);
        assert!(quit);
        assert!(app.should_quit);
    }

    #[test]
    fn test_arrow_down_moves_selection() {
        let mut app = app_with_members();
        assert_eq!(app.selected_index, 0);
        handle_event(&key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(app.selected_index, 1);
    }

    #[test]
    fn test_arrow_up_moves_selection() {
        let mut app = app_with_members();
        app.selected_index = 2;
        handle_event(&key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert_eq!(app.selected_index, 1);
    }

    #[test]
    fn test_tab_cycles_focus() {
        let mut app = App::new("atm-dev".to_string());
        assert_eq!(app.focus, FocusPanel::Dashboard);
        handle_event(&key_event(KeyCode::Tab, KeyModifiers::NONE), &mut app);
        assert_eq!(app.focus, FocusPanel::AgentTerminal);
        handle_event(&key_event(KeyCode::Tab, KeyModifiers::NONE), &mut app);
        assert_eq!(app.focus, FocusPanel::Dashboard);
    }

    #[test]
    fn test_other_key_ignored_on_dashboard() {
        let mut app = App::new("atm-dev".to_string());
        let quit = handle_event(&key_event(KeyCode::Char('x'), KeyModifiers::NONE), &mut app);
        assert!(!quit);
        assert!(!app.should_quit);
    }

    // ── Agent Terminal input bindings ─────────────────────────────────────────

    #[test]
    fn test_char_input_in_agent_terminal_appends() {
        let mut app = app_with_members();
        app.focus = FocusPanel::AgentTerminal;
        app.selected_index = 0; // "a" is "idle"
        handle_event(&key_event(KeyCode::Char('h'), KeyModifiers::NONE), &mut app);
        assert_eq!(app.control_input, "h");
    }

    #[test]
    fn test_char_input_on_dashboard_ignored() {
        let mut app = app_with_members();
        app.focus = FocusPanel::Dashboard;
        handle_event(&key_event(KeyCode::Char('h'), KeyModifiers::NONE), &mut app);
        assert!(app.control_input.is_empty(), "Dashboard input must be ignored");
    }

    #[test]
    fn test_backspace_removes_last_char() {
        let mut app = app_with_members();
        app.focus = FocusPanel::AgentTerminal;
        app.selected_index = 0; // idle
        app.control_input = "hello".to_string();
        handle_event(&key_event(KeyCode::Backspace, KeyModifiers::NONE), &mut app);
        assert_eq!(app.control_input, "hell");
    }

    #[test]
    fn test_esc_clears_input() {
        let mut app = app_with_members();
        app.focus = FocusPanel::AgentTerminal;
        app.control_input = "something".to_string();
        handle_event(&key_event(KeyCode::Esc, KeyModifiers::NONE), &mut app);
        assert!(app.control_input.is_empty());
    }

    #[test]
    fn test_enter_with_text_sets_pending_stdin() {
        let mut app = app_with_members();
        app.focus = FocusPanel::AgentTerminal;
        app.selected_index = 0; // "a" is "idle"
        app.control_input = "hello world".to_string();
        handle_event(&key_event(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        assert!(
            matches!(&app.pending_control, Some(PendingControl::Stdin(s)) if s == "hello world"),
            "Expected Stdin(\"hello world\"), got {:?}",
            app.pending_control.as_ref().map(|p| format!("{p:?}"))
        );
        assert!(app.control_input.is_empty());
    }

    #[test]
    fn test_enter_with_empty_input_no_pending() {
        let mut app = app_with_members();
        app.focus = FocusPanel::AgentTerminal;
        app.selected_index = 0; // idle
        app.control_input = "   ".to_string(); // whitespace only
        handle_event(&key_event(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        assert!(app.pending_control.is_none(), "Whitespace-only input should not set pending");
    }

    #[test]
    fn test_ctrl_k_sets_pending_interrupt() {
        let mut app = app_with_members();
        app.focus = FocusPanel::AgentTerminal;
        app.selected_index = 1; // "b" is "busy"
        handle_event(&key_event(KeyCode::Char('k'), KeyModifiers::CONTROL), &mut app);
        assert!(
            matches!(app.pending_control, Some(PendingControl::Interrupt)),
            "Expected Interrupt pending"
        );
    }

    #[test]
    fn test_ctrl_k_not_sent_when_not_live() {
        let mut app = app_with_members();
        app.focus = FocusPanel::AgentTerminal;
        app.members[0].state = "killed".to_string();
        app.selected_index = 0;
        handle_event(&key_event(KeyCode::Char('k'), KeyModifiers::CONTROL), &mut app);
        assert!(app.pending_control.is_none(), "Interrupt should not be set for non-live agent");
    }

    #[test]
    fn test_char_not_appended_when_not_live() {
        let mut app = app_with_members();
        app.focus = FocusPanel::AgentTerminal;
        app.members[0].state = "launching".to_string();
        app.selected_index = 0;
        handle_event(&key_event(KeyCode::Char('x'), KeyModifiers::NONE), &mut app);
        assert!(
            app.control_input.is_empty(),
            "Char input should not append when agent is not live"
        );
    }

    #[test]
    fn test_q_does_not_quit_when_input_has_text() {
        // When the user has typed something, 'q' should append to input, not quit.
        let mut app = app_with_members();
        app.focus = FocusPanel::AgentTerminal;
        app.selected_index = 0; // idle
        app.control_input = "hel".to_string();
        let quit = handle_event(&key_event(KeyCode::Char('q'), KeyModifiers::NONE), &mut app);
        assert!(!quit, "q should not quit when control_input is non-empty");
        assert_eq!(app.control_input, "helq");
    }
}
