//! Ratatui layout and widget rendering for the ATM TUI.
//!
//! The layout is a two-column split with a header bar and a status bar:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────┐
//! │ ATM TUI  │  Team: <team>                                         │ header
//! ├────────────────────┬─────────────────────────────────────────────┤
//! │ Dashboard          │ Agent Terminal                              │
//! │                    │                                             │ body
//! │ AGENT   STATE INB  │ [LIVE] arch-ctm                            │
//! │ arch-ctm idle   3  │ {"Timestamp":"...","Level":"info",...}      │
//! │ ...                │ ...                                         │
//! │                    ├─────────────────────────────────────────────┤
//! │                    │ Type to send stdin...  (or [disabled])      │ input
//! ├────────────────────┴─────────────────────────────────────────────┤
//! │ q: quit  ↑↓: select  Tab: panel  Ctrl-K: interrupt              │ status
//! └──────────────────────────────────────────────────────────────────┘
//! ```

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::agent_terminal::expand_keys;
use crate::app::{App, FocusPanel};

/// Render the full TUI frame from current [`App`] state.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // ── Outer vertical split: header / body / status ─────────────────────────
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(0),    // body
            Constraint::Length(1), // status bar
        ])
        .split(area);

    draw_header(frame, outer[0], app);
    draw_body(frame, outer[1], app);
    draw_status_bar(frame, outer[2], app);
}

// ── Header ────────────────────────────────────────────────────────────────────

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let text = Line::from(vec![
        Span::styled(
            " ATM TUI  ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" Team: {}", app.team)),
    ]);
    frame.render_widget(Paragraph::new(text), area);
}

// ── Body (left + right) ───────────────────────────────────────────────────────

fn draw_body(frame: &mut Frame, area: Rect, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    draw_dashboard(frame, columns[0], app);
    draw_agent_terminal(frame, columns[1], app);
}

// ── Dashboard panel ───────────────────────────────────────────────────────────

fn draw_dashboard(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == FocusPanel::Dashboard;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" Dashboard ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style);

    // Column header row
    let header = ListItem::new(Line::from(vec![
        Span::styled(
            format!("{:<20} {:<8} {}", "AGENT", "STATE", "INBOX"),
            Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ),
    ]));

    let mut items: Vec<ListItem> = vec![header];

    for (idx, member) in app.members.iter().enumerate() {
        let selected = idx == app.selected_index;
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let state_color = match member.state.as_str() {
            "busy" => Color::Yellow,
            "launching" => Color::Blue,
            "killed" | "stale" | "closed" => Color::Red,
            _ => Color::Green, // idle, unknown
        };

        let row = Line::from(vec![
            Span::styled(
                format!("{:<20}", truncate_str(&member.agent, 20)),
                style,
            ),
            Span::styled(
                format!(" {:<8}", truncate_str(&member.state, 8)),
                Style::default().fg(if selected { Color::Black } else { state_color }),
            ),
            Span::styled(
                format!(" {}", member.inbox_count),
                style,
            ),
        ]);

        items.push(ListItem::new(row));
    }

    if app.members.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            " (no members — daemon may be offline)",
            Style::default().fg(Color::DarkGray),
        ))));
    }

    let mut list_state = ListState::default();
    // +1 because the header occupies index 0 in the item list
    list_state.select(Some(app.selected_index + 1));

    frame.render_stateful_widget(
        List::new(items).block(block),
        area,
        &mut list_state,
    );
}

// ── Agent Terminal panel ──────────────────────────────────────────────────────

fn draw_agent_terminal(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == FocusPanel::AgentTerminal;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    // Split right panel: stream area + control input bar
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);

    draw_stream_pane(frame, rows[0], app, border_style, focused);
    draw_control_input(frame, rows[1], app, border_style);
}

fn draw_stream_pane(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    border_style: Style,
    focused: bool,
) {
    let agent_label = app
        .streaming_agent
        .as_deref()
        .unwrap_or("(none selected)");

    let source_badge = if app.session_log_path.as_ref().is_some_and(|p| p.exists()) {
        Span::styled("[LIVE] ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
    } else {
        Span::styled("[WAITING] ", Style::default().fg(Color::DarkGray))
    };

    let title_line = Line::from(vec![
        source_badge,
        Span::raw(agent_label),
    ]);

    let block = Block::default()
        .title(title_line)
        .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
        .border_type(if focused { BorderType::Rounded } else { BorderType::Plain })
        .border_style(border_style);

    // Show the last N lines that fit in the viewport
    let inner_height = area.height.saturating_sub(2) as usize; // subtract top border + potential title
    let start = app.stream_lines.len().saturating_sub(inner_height.max(1));
    let visible: Vec<Line> = app.stream_lines[start..]
        .iter()
        .map(|line| {
            let expanded = expand_keys(line);
            Line::from(Span::raw(expanded))
        })
        .collect();

    if visible.is_empty() {
        let placeholder = if app.streaming_agent.is_none() {
            "Select an agent with ↑↓ to stream its session log."
        } else {
            "Waiting for session log data..."
        };
        frame.render_widget(
            Paragraph::new(placeholder)
                .block(block)
                .style(Style::default().fg(Color::DarkGray))
                .wrap(Wrap { trim: false }),
            area,
        );
    } else {
        frame.render_widget(
            Paragraph::new(visible)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}

/// Render the Agent Terminal control input field.
///
/// Shows an active text cursor and hint when the selected agent is live;
/// shows a disabled placeholder with reason otherwise.
fn draw_control_input(frame: &mut Frame, area: Rect, app: &App, border_style: Style) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(border_style);

    if app.is_live() {
        let content = if app.control_input.is_empty() {
            Line::from(Span::styled(
                "Type to send stdin... (Enter: send  Ctrl-K: interrupt  Esc: clear)",
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            Line::from(vec![
                Span::raw(app.control_input.as_str()),
                Span::styled("█", Style::default().fg(Color::Cyan)),
            ])
        };
        frame.render_widget(Paragraph::new(content).block(block), area);
    } else {
        let reason = app.not_live_reason().unwrap_or("Not live");
        let content = Line::from(vec![
            Span::styled(
                "[disabled] ",
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            ),
            Span::styled(
                format!("control input not available: {reason}"),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(content).block(block), area);
    }
}

// ── Status bar ────────────────────────────────────────────────────────────────

fn draw_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let text = if let Some(ref msg) = app.status_message {
        Line::from(vec![
            Span::styled(" ✓ ", Style::default().fg(Color::Green)),
            Span::raw(msg.as_str()),
        ])
    } else {
        Line::from(vec![
            Span::styled(" q", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(": quit  "),
            Span::styled("↑↓", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(": select  "),
            Span::styled("Tab", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(": panel  "),
            Span::styled(
                "Ctrl-K",
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::raw(": interrupt"),
        ])
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

// ── Utilities ─────────────────────────────────────────────────────────────────

/// Truncate a string to `max_chars` characters, appending `…` when truncated.
fn truncate_str(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        let end = max_chars.saturating_sub(1);
        format!("{}…", chars[..end].iter().collect::<String>())
    }
}
