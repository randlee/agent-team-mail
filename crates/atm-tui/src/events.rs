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
//! | `F` | Toggle follow mode (uppercase) |
//!
//! ## Agent Terminal panel (when selected agent is live)
//!
//! | Key | Action |
//! |-----|--------|
//! | _printable char_ | Append to control input |
//! | `Enter` | Submit stdin text (non-empty) |
//! | `Backspace` | Delete last character |
//! | `Ctrl-I` | Send interrupt (subject to [`InterruptPolicy`]) |
//! | `Esc` | Clear control input / cancel pending interrupt confirmation |
//!
//! ### Interrupt confirmation dialog (`interrupt_policy = "confirm"`)
//!
//! When [`InterruptPolicy::Confirm`] is active, `Ctrl-I` sets
//! `confirm_interrupt_pending = true` and shows `"Send interrupt? [y/N]"` in
//! the status bar. While the dialog is open:
//!
//! | Key | Action |
//! |-----|--------|
//! | `y` / `Y` / `Enter` | Confirm — dispatch interrupt |
//! | `n` / `N` / `Esc` | Cancel — dismiss dialog |
//! | _other_ | Ignored |
//!
//! Dashboard panel ignores character input — it is mail-only.
//!
//! [`InterruptPolicy`]: crate::config::InterruptPolicy

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, FocusPanel, PendingControl};
use crate::config::InterruptPolicy;

/// Process a single terminal input event and update [`App`] state accordingly.
///
/// Returns `true` if the application should quit after this event.
pub fn handle_event(event: &Event, app: &mut App) -> bool {
    if let Event::Key(KeyEvent { code, modifiers, .. }) = event {
        // ── Interrupt confirmation dialog (higher priority) ────────────────────
        // When a confirmation is pending, only accept y/Y/Enter (confirm) or
        // n/N/Esc (cancel). All other keys are silently discarded so the user
        // does not accidentally trigger other bindings while the dialog is open.
        if app.confirm_interrupt_pending {
            return handle_confirm_interrupt(code, app);
        }

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
            // 'F' (uppercase) toggles follow mode globally regardless of panel.
            (KeyCode::Char('F'), m) if !m.contains(KeyModifiers::CONTROL) => {
                app.follow_mode = !app.follow_mode;
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

/// Handle the `y/N` interrupt confirmation dialog.
///
/// Called when `app.confirm_interrupt_pending` is `true`. Clears the pending
/// flag in all cases; dispatches the interrupt only on confirmation.
///
/// Returns `true` only if the application should quit (never for this dialog).
fn handle_confirm_interrupt(code: &KeyCode, app: &mut App) -> bool {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            app.confirm_interrupt_pending = false;
            app.status_message = None;
            app.pending_control = Some(PendingControl::Interrupt);
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.confirm_interrupt_pending = false;
            app.status_message = None;
        }
        // Any other key is silently ignored while the dialog is open.
        _ => {}
    }
    false
}

/// Handle keys while the Agent Terminal panel is focused.
fn handle_agent_terminal_key(
    code: &KeyCode,
    modifiers: &KeyModifiers,
    app: &mut App,
) -> bool {
    // Ctrl-I — interrupt, gated by InterruptPolicy.
    if matches!(code, KeyCode::Char('i')) && modifiers.contains(KeyModifiers::CONTROL) {
        if app.is_live() {
            match app.config.interrupt_policy {
                InterruptPolicy::Always => {
                    app.pending_control = Some(PendingControl::Interrupt);
                }
                InterruptPolicy::Never => {
                    // Silently discard.
                }
                InterruptPolicy::Confirm => {
                    app.confirm_interrupt_pending = true;
                    app.status_message =
                        Some("Send interrupt? [y/N]".to_string());
                }
            }
        }
        return false;
    }

    // Esc → clear control input (interrupt confirmation is handled above)
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
    use crate::config::{InterruptPolicy, TuiConfig};
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key_event(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    fn new_app() -> App {
        App::new("atm-dev".to_string(), TuiConfig::default())
    }

    fn app_with_members() -> App {
        let mut app = new_app();
        app.members = vec![
            MemberRow { agent: "a".into(), state: "idle".into(), inbox_count: 0 },
            MemberRow { agent: "b".into(), state: "busy".into(), inbox_count: 1 },
            MemberRow { agent: "c".into(), state: "idle".into(), inbox_count: 2 },
        ];
        app
    }

    fn app_with_policy(policy: InterruptPolicy) -> App {
        let mut cfg = TuiConfig::default();
        cfg.interrupt_policy = policy;
        let mut app = App::new("atm-dev".to_string(), cfg);
        app.members = vec![
            MemberRow { agent: "a".into(), state: "busy".into(), inbox_count: 0 },
        ];
        app.focus = FocusPanel::AgentTerminal;
        app.selected_index = 0;
        app
    }

    // ── Global bindings ───────────────────────────────────────────────────────

    #[test]
    fn test_q_quits_on_dashboard() {
        let mut app = new_app();
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
        let mut app = new_app();
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
        let mut app = new_app();
        assert_eq!(app.focus, FocusPanel::Dashboard);
        handle_event(&key_event(KeyCode::Tab, KeyModifiers::NONE), &mut app);
        assert_eq!(app.focus, FocusPanel::AgentTerminal);
        handle_event(&key_event(KeyCode::Tab, KeyModifiers::NONE), &mut app);
        assert_eq!(app.focus, FocusPanel::Dashboard);
    }

    #[test]
    fn test_other_key_ignored_on_dashboard() {
        let mut app = new_app();
        let quit = handle_event(&key_event(KeyCode::Char('x'), KeyModifiers::NONE), &mut app);
        assert!(!quit);
        assert!(!app.should_quit);
    }

    // ── Follow mode toggle ────────────────────────────────────────────────────

    #[test]
    fn test_uppercase_f_toggles_follow_mode_on() {
        let mut app = new_app();
        app.follow_mode = false;
        handle_event(&key_event(KeyCode::Char('F'), KeyModifiers::NONE), &mut app);
        assert!(app.follow_mode, "F must enable follow mode when it was off");
    }

    #[test]
    fn test_uppercase_f_toggles_follow_mode_off() {
        let mut app = new_app();
        app.follow_mode = true;
        handle_event(&key_event(KeyCode::Char('F'), KeyModifiers::NONE), &mut app);
        assert!(!app.follow_mode, "F must disable follow mode when it was on");
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

    // ── Interrupt policy: Always ──────────────────────────────────────────────

    #[test]
    fn test_ctrl_i_always_policy_dispatches_immediately() {
        let mut app = app_with_policy(InterruptPolicy::Always);
        handle_event(&key_event(KeyCode::Char('i'), KeyModifiers::CONTROL), &mut app);
        assert!(
            matches!(app.pending_control, Some(PendingControl::Interrupt)),
            "Always policy must dispatch interrupt immediately"
        );
        assert!(!app.confirm_interrupt_pending);
    }

    // ── Interrupt policy: Never ───────────────────────────────────────────────

    #[test]
    fn test_ctrl_i_never_policy_discards_silently() {
        let mut app = app_with_policy(InterruptPolicy::Never);
        handle_event(&key_event(KeyCode::Char('i'), KeyModifiers::CONTROL), &mut app);
        assert!(app.pending_control.is_none(), "Never policy must discard interrupt");
        assert!(!app.confirm_interrupt_pending);
    }

    // ── Interrupt policy: Confirm ─────────────────────────────────────────────

    #[test]
    fn test_ctrl_i_confirm_policy_sets_pending_dialog() {
        let mut app = app_with_policy(InterruptPolicy::Confirm);
        handle_event(&key_event(KeyCode::Char('i'), KeyModifiers::CONTROL), &mut app);
        assert!(app.confirm_interrupt_pending, "Confirm policy must open dialog");
        assert_eq!(app.status_message.as_deref(), Some("Send interrupt? [y/N]"));
        assert!(app.pending_control.is_none(), "Control must not be dispatched yet");
    }

    #[test]
    fn test_confirm_dialog_y_dispatches_interrupt() {
        let mut app = app_with_policy(InterruptPolicy::Confirm);
        app.confirm_interrupt_pending = true;
        app.status_message = Some("Send interrupt? [y/N]".to_string());
        handle_event(&key_event(KeyCode::Char('y'), KeyModifiers::NONE), &mut app);
        assert!(
            matches!(app.pending_control, Some(PendingControl::Interrupt)),
            "y must dispatch interrupt"
        );
        assert!(!app.confirm_interrupt_pending, "dialog must be cleared");
        assert!(app.status_message.is_none(), "status message must be cleared");
    }

    #[test]
    fn test_confirm_dialog_uppercase_y_dispatches_interrupt() {
        let mut app = app_with_policy(InterruptPolicy::Confirm);
        app.confirm_interrupt_pending = true;
        handle_event(&key_event(KeyCode::Char('Y'), KeyModifiers::NONE), &mut app);
        assert!(matches!(app.pending_control, Some(PendingControl::Interrupt)));
        assert!(!app.confirm_interrupt_pending);
    }

    #[test]
    fn test_confirm_dialog_enter_dispatches_interrupt() {
        let mut app = app_with_policy(InterruptPolicy::Confirm);
        app.confirm_interrupt_pending = true;
        handle_event(&key_event(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        assert!(matches!(app.pending_control, Some(PendingControl::Interrupt)));
        assert!(!app.confirm_interrupt_pending);
    }

    #[test]
    fn test_confirm_dialog_n_cancels() {
        let mut app = app_with_policy(InterruptPolicy::Confirm);
        app.confirm_interrupt_pending = true;
        app.status_message = Some("Send interrupt? [y/N]".to_string());
        handle_event(&key_event(KeyCode::Char('n'), KeyModifiers::NONE), &mut app);
        assert!(app.pending_control.is_none(), "n must cancel interrupt");
        assert!(!app.confirm_interrupt_pending, "dialog must be cleared");
        assert!(app.status_message.is_none(), "status message must be cleared");
    }

    #[test]
    fn test_confirm_dialog_uppercase_n_cancels() {
        let mut app = app_with_policy(InterruptPolicy::Confirm);
        app.confirm_interrupt_pending = true;
        handle_event(&key_event(KeyCode::Char('N'), KeyModifiers::NONE), &mut app);
        assert!(app.pending_control.is_none());
        assert!(!app.confirm_interrupt_pending);
    }

    #[test]
    fn test_confirm_dialog_esc_cancels() {
        let mut app = app_with_policy(InterruptPolicy::Confirm);
        app.confirm_interrupt_pending = true;
        handle_event(&key_event(KeyCode::Esc, KeyModifiers::NONE), &mut app);
        assert!(app.pending_control.is_none(), "Esc must cancel interrupt confirmation");
        assert!(!app.confirm_interrupt_pending);
    }

    #[test]
    fn test_confirm_dialog_other_key_ignored() {
        let mut app = app_with_policy(InterruptPolicy::Confirm);
        app.confirm_interrupt_pending = true;
        handle_event(&key_event(KeyCode::Char('x'), KeyModifiers::NONE), &mut app);
        // Dialog stays open; no control dispatched.
        assert!(app.confirm_interrupt_pending, "unrecognised key must leave dialog open");
        assert!(app.pending_control.is_none());
    }

    // ── Legacy interrupt tests ────────────────────────────────────────────────

    #[test]
    fn test_ctrl_i_sets_pending_interrupt_default_policy() {
        // Default policy is Confirm — Ctrl-I should open dialog, not dispatch.
        let mut app = app_with_members();
        app.focus = FocusPanel::AgentTerminal;
        app.selected_index = 1; // "b" is "busy"
        handle_event(&key_event(KeyCode::Char('i'), KeyModifiers::CONTROL), &mut app);
        // With Confirm policy, dialog opens rather than dispatching directly.
        assert!(app.confirm_interrupt_pending, "default Confirm policy must open dialog");
    }

    #[test]
    fn test_ctrl_i_not_sent_when_not_live() {
        let mut app = app_with_members();
        app.focus = FocusPanel::AgentTerminal;
        app.members[0].state = "killed".to_string();
        app.selected_index = 0;
        handle_event(&key_event(KeyCode::Char('i'), KeyModifiers::CONTROL), &mut app);
        assert!(app.pending_control.is_none(), "Interrupt should not be set for non-live agent");
        assert!(!app.confirm_interrupt_pending, "dialog must not open for non-live agent");
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
