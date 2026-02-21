//! Keyboard input event handling for the ATM TUI.
//!
//! Events are consumed in the main loop. The handler mutates [`App`] state
//! directly; rendering happens separately. All unrecognised keys are silently
//! ignored (input is disabled in D.1).

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;

/// Process a single terminal input event and update [`App`] state accordingly.
///
/// Returns `true` if the application should quit after this event.
///
/// # Key bindings
///
/// | Key | Action |
/// |-----|--------|
/// | `q` | Quit |
/// | `Ctrl-C` | Quit |
/// | `↑` | Move selection up |
/// | `↓` | Move selection down |
/// | `Tab` | Cycle panel focus |
/// | _other_ | Ignored (input disabled) |
pub fn handle_event(event: &Event, app: &mut App) -> bool {
    if let Event::Key(KeyEvent { code, modifiers, .. }) = event {
        match (code, modifiers) {
            (KeyCode::Char('q'), _) => {
                app.should_quit = true;
                return true;
            }
            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                app.should_quit = true;
                return true;
            }
            (KeyCode::Up, _) => {
                app.select_previous();
            }
            (KeyCode::Down, _) => {
                app.select_next();
            }
            (KeyCode::Tab, _) => {
                app.cycle_focus();
            }
            _ => {}
        }
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

    #[test]
    fn test_q_quits() {
        let mut app = App::new("atm-dev".to_string());
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
    fn test_other_key_ignored() {
        let mut app = App::new("atm-dev".to_string());
        let quit = handle_event(&key_event(KeyCode::Char('x'), KeyModifiers::NONE), &mut app);
        assert!(!quit);
        assert!(!app.should_quit);
    }
}
