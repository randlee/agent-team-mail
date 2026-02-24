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
//! │ q: quit  ↑↓: select  Tab: panel  Ctrl-I: interrupt              │ status
//! └──────────────────────────────────────────────────────────────────┘
//! ```

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::agent_terminal::expand_keys;
use crate::app::{App, FocusPanel};
use crate::codex_watch::render_stream_line;

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
        Span::raw(format!(
            " v{}  Team: {}",
            env!("CARGO_PKG_VERSION"),
            app.team
        )),
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
    if app.log_viewer_visible {
        draw_log_viewer(frame, columns[1], app);
    } else {
        draw_agent_terminal(frame, columns[1], app);
    }
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

    let left_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);

    // Column header row
    let header = ListItem::new(Line::from(vec![Span::styled(
        format!("{:<20} {:<8} {}", "AGENT", "STATE", "INBOX"),
        Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    )]));

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
            Span::styled(format!("{:<20}", truncate_str(&member.agent, 20)), style),
            Span::styled(
                format!(" {:<8}", truncate_str(&member.state, 8)),
                Style::default().fg(if selected { Color::Black } else { state_color }),
            ),
            Span::styled(format!(" {}", member.inbox_count), style),
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

    frame.render_stateful_widget(List::new(items).block(block), left_rows[0], &mut list_state);

    let inbox_title = app
        .selected_agent()
        .map(|a| format!(" Inbox Preview ({a}) "))
        .unwrap_or_else(|| " Inbox Preview ".to_string());
    let inbox_block = Block::default()
        .title(inbox_title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style);

    if app.inbox_preview.is_empty() {
        frame.render_widget(
            Paragraph::new("No messages")
                .block(inbox_block)
                .style(Style::default().fg(Color::DarkGray)),
            left_rows[1],
        );
    } else {
        let lines: Vec<Line> = app
            .inbox_preview
            .iter()
            .map(|s| Line::from(Span::raw(s.clone())))
            .collect();
        frame.render_widget(
            Paragraph::new(lines)
                .block(inbox_block)
                .wrap(Wrap { trim: false }),
            left_rows[1],
        );
    }
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

fn draw_stream_pane(frame: &mut Frame, area: Rect, app: &App, border_style: Style, focused: bool) {
    let agent_label = app.streaming_agent.as_deref().unwrap_or("(none selected)");

    // Choose stream badge from daemon-derived stream state rather than
    // filesystem inference.
    let source_badge = if app.stream_source_error.is_some() {
        Span::styled(
            "[FROZEN] ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else if app.daemon_turn_state.as_ref().is_some_and(|s| {
        s.turn_status != agent_team_mail_core::daemon_stream::StreamTurnStatus::Terminal
    }) {
        Span::styled(
            "[LIVE] ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else if !app.stream_lines.is_empty() {
        Span::styled(
            "[REPLAY] ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("[WAITING] ", Style::default().fg(Color::DarkGray))
    };

    // Add daemon turn state badge if available.
    let turn_badge = match &app.daemon_turn_state {
        Some(state) => {
            use agent_team_mail_core::daemon_stream::StreamTurnStatus;
            let (text, color) = match state.turn_status {
                StreamTurnStatus::Busy => ("[BUSY] ", Color::Yellow),
                StreamTurnStatus::Terminal => ("[DONE] ", Color::Cyan),
                StreamTurnStatus::Idle => ("[IDLE] ", Color::Blue),
            };
            Span::styled(
                text,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        }
        None => Span::raw(""),
    };

    let title_line = Line::from(vec![source_badge, turn_badge, Span::raw(agent_label)]);

    let block = Block::default()
        .title(title_line)
        .borders(Borders::ALL)
        .border_type(if focused {
            BorderType::Rounded
        } else {
            BorderType::Plain
        })
        .border_style(border_style);

    frame.render_widget(block.clone(), area);
    let inner = block.inner(area);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // status rows
            Constraint::Length(1), // progress row
            Constraint::Min(0),    // transcript
        ])
        .split(inner);

    let turn_status = app
        .daemon_turn_state
        .as_ref()
        .map(|s| s.turn_status.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    // Status precedence rule (M.5): direct watch stream is authoritative.
    // Daemon values are fallback only when watch metadata is absent.
    let transport = app.resolved_watch_transport().unwrap_or("n/a");
    let turn_id = app.resolved_watch_turn_id().unwrap_or("n/a");
    let session_id = app.resolved_watch_session_id().unwrap_or("n/a");
    let model = app.watch_model.as_deref().unwrap_or("n/a");
    let context = app
        .watch_context_window_pct
        .map(|pct| format!("{pct:.0}%"))
        .unwrap_or_else(|| "n/a".to_string());
    let completed_total =
        app.watch_turn_completed + app.watch_turn_interrupted + app.watch_turn_failed;
    let summary_lines = vec![
        Line::from(vec![
            Span::styled("status ", Style::default().fg(Color::Blue)),
            Span::styled(turn_status, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("transport ", Style::default().fg(Color::Blue)),
            Span::raw(transport),
            Span::raw("  "),
            Span::styled("turn ", Style::default().fg(Color::Blue)),
            Span::raw(turn_id),
        ]),
        Line::from(vec![
            Span::styled("session ", Style::default().fg(Color::Blue)),
            Span::raw(session_id),
            Span::raw("  "),
            Span::styled("model ", Style::default().fg(Color::Blue)),
            Span::raw(model),
            Span::raw("  "),
            Span::styled("context ", Style::default().fg(Color::Blue)),
            Span::raw(context),
        ]),
        Line::from(vec![
            Span::styled("events ", Style::default().fg(Color::Blue)),
            Span::raw(format!(
                "started={} completed={} interrupted={} failed={}  dropped={} unknown={}",
                app.watch_turn_started,
                app.watch_turn_completed,
                app.watch_turn_interrupted,
                app.watch_turn_failed,
                app.watch_dropped,
                app.watch_unknown
            )),
        ]),
    ];
    frame.render_widget(Paragraph::new(summary_lines), sections[0]);

    let ratio = if app.watch_turn_started == 0 {
        0.0
    } else {
        (completed_total as f64 / app.watch_turn_started as f64).clamp(0.0, 1.0)
    };
    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(Color::Cyan))
            .ratio(ratio)
            .label(format!("turn completion {:.0}%", ratio * 100.0)),
        sections[1],
    );

    // Compute visible log lines based on scroll offset and transcript viewport.
    let inner_height = sections[2].height as usize;

    // Prepend a freeze indicator line when the stream is frozen.
    let freeze_line: Option<Line> = app.stream_source_error.as_ref().map(|msg| {
        Line::from(Span::styled(
            format!("[{msg}]"),
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ))
    });

    // When follow mode is on, stream_scroll_offset is updated by append_stream_lines
    // to be >= stream_lines.len(). The start index is clamped so that exactly
    // `inner_height` lines (or fewer) are rendered, always pinned to the bottom.
    // When follow mode is off, the offset is the user's chosen scroll position.
    let bottom = app.stream_scroll_offset.min(app.stream_lines.len());
    let start = bottom.saturating_sub(inner_height.max(1));
    let mut visible: Vec<Line> = app.stream_lines[start..bottom]
        .iter()
        .map(|line| {
            let expanded = expand_keys(line);
            render_stream_line(&expanded)
        })
        .collect();

    // Insert freeze indicator at the top if present.
    if let Some(fl) = freeze_line {
        visible.insert(0, fl);
    }

    if visible.is_empty() {
        let placeholder = if app.streaming_agent.is_none() {
            "Select an agent with ↑↓ to stream its session log."
        } else {
            "Waiting for session log data..."
        };
        frame.render_widget(
            Paragraph::new(placeholder)
                .style(Style::default().fg(Color::DarkGray))
                .wrap(Wrap { trim: false }),
            sections[2],
        );
    } else {
        frame.render_widget(
            Paragraph::new(visible).wrap(Wrap { trim: false }),
            sections[2],
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
                "Type to send stdin... (Enter: send  Ctrl-I: interrupt  Esc: clear)",
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
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ),
            Span::styled(
                format!("control input not available: {reason}"),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(content).block(block), area);
    }
}

// ── Log Viewer panel ──────────────────────────────────────────────────────────

/// Format a single [`LogEventV1`] into a display line and the color to render it with.
///
/// Format: `{ts}  {LEVEL:<5}  [{source_binary}{/agent}] {action}{suffix}`
/// where suffix is `: {error}` or ` ({outcome})` when those fields are present.
fn format_log_event_line(
    event: &agent_team_mail_core::logging_event::LogEventV1,
) -> (String, ratatui::style::Color) {
    // Choose a color based on the log level.
    let color = match event.level.to_lowercase().as_str() {
        "error" => Color::Red,
        "warn" => Color::Yellow,
        "info" => Color::Green,
        "debug" | "trace" => Color::DarkGray,
        _ => Color::White,
    };

    // Build the source label: `source_binary` or `source_binary/agent`.
    let source_label = match event.agent.as_deref() {
        Some(agent) => format!("{}/{}", event.source_binary, agent),
        None => event.source_binary.clone(),
    };

    // Build optional suffix.
    let suffix = if let Some(ref err) = event.error {
        format!(": {err}")
    } else if let Some(ref outcome) = event.outcome {
        format!(" ({outcome})")
    } else {
        String::new()
    };

    let line = format!(
        "{}  {:<5}  [{}] {}{}",
        event.ts,
        event.level.to_uppercase(),
        source_label,
        event.action,
        suffix,
    );

    (line, color)
}

/// Render the Log Viewer panel.
fn draw_log_viewer(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == FocusPanel::LogViewer;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    // Build the title with optional level filter and follow badge.
    let level_label = match app.log_level_filter.as_deref() {
        Some(l) => format!(" Log Viewer [level: {l}]"),
        None => " Log Viewer [level: all]".to_string(),
    };
    let title_text = if app.log_follow_mode {
        format!("{level_label} [FOLLOW] ")
    } else {
        format!("{level_label} ")
    };

    let block = Block::default()
        .title(title_text)
        .borders(Borders::ALL)
        .border_type(if focused {
            BorderType::Rounded
        } else {
            BorderType::Plain
        })
        .border_style(border_style);

    // Account for top/bottom borders.
    let inner_height = area.height.saturating_sub(2) as usize;

    if app.log_events.is_empty() {
        frame.render_widget(
            Paragraph::new("No log events — start atm-daemon to see logs")
                .block(block)
                .style(Style::default().fg(Color::DarkGray))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    // Filter events by active level filter before windowing.
    let filtered: Vec<&agent_team_mail_core::logging_event::LogEventV1> = app
        .log_events
        .iter()
        .filter(|e| {
            app.log_level_filter
                .as_deref()
                .is_none_or(|f| e.level.eq_ignore_ascii_case(f))
        })
        .collect();

    let bottom = app.log_scroll_offset.min(filtered.len());
    let start = bottom.saturating_sub(inner_height.max(1));
    let visible: Vec<Line> = filtered[start..bottom]
        .iter()
        .map(|event| {
            let (text, color) = format_log_event_line(event);
            Line::from(Span::styled(text, Style::default().fg(color)))
        })
        .collect();

    frame.render_widget(
        Paragraph::new(visible)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

// ── Status bar ────────────────────────────────────────────────────────────────

fn draw_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let text = if app.confirm_interrupt_pending {
        // Interrupt confirmation dialog takes highest priority in the status bar.
        Line::from(vec![
            Span::styled(
                " Send interrupt? ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "[y",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("/"),
            Span::styled(
                "N]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
        ])
    } else if let Some(ref msg) = app.status_message {
        Line::from(vec![
            Span::styled(" ✓ ", Style::default().fg(Color::Green)),
            Span::raw(msg.as_str()),
        ])
    } else if let Some(ref err) = app.stream_source_error {
        Line::from(vec![
            Span::styled(
                " [FROZEN] ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(err.as_str(), Style::default().fg(Color::Yellow)),
        ])
    } else {
        let follow_label = if app.follow_mode {
            "follow:ON"
        } else {
            "follow:OFF"
        };
        Line::from(vec![
            Span::styled(
                " q",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": quit  "),
            Span::styled(
                "↑↓",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": select  "),
            Span::styled(
                "Tab",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": panel  "),
            Span::styled(
                "Ctrl-I",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": interrupt  "),
            Span::styled(
                "F",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(": {follow_label}  ")),
            Span::styled(
                "L",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": log  "),
            Span::styled(
                "G",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": filter"),
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
