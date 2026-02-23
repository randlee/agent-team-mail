//! Codex watch-pane rendering helpers adapted for ATM TUI.
//!
//! This module provides a light-weight rendering layer for stream lines and
//! daemon turn events so the watch pane aligns with Codex CLI-style transcript
//! and status presentation.

use agent_team_mail_core::daemon_stream::{DaemonStreamEvent, TurnStatusWire};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Convert a daemon stream event into a compact, Codex-style transcript line.
pub fn format_daemon_event_line(event: &DaemonStreamEvent) -> String {
    match event {
        DaemonStreamEvent::TurnStarted {
            turn_id, transport, ..
        } => {
            format!("turn.started transport={transport} id={turn_id}")
        }
        DaemonStreamEvent::TurnCompleted {
            turn_id,
            transport,
            status,
            ..
        } => {
            let status_s = match status {
                TurnStatusWire::Completed => "completed",
                TurnStatusWire::Interrupted => "interrupted",
                TurnStatusWire::Failed => "failed",
            };
            format!("turn.completed status={status_s} transport={transport} id={turn_id}")
        }
        DaemonStreamEvent::TurnIdle {
            turn_id, transport, ..
        } => {
            format!("turn.idle transport={transport} id={turn_id}")
        }
        DaemonStreamEvent::StreamError {
            session_id,
            error_summary,
            ..
        } => format!("stream.error session={session_id} message={error_summary}"),
        DaemonStreamEvent::DroppedCounters {
            dropped, unknown, ..
        } => format!("stream.counters dropped={dropped} unknown={unknown}"),
    }
}

/// Render one watch-stream line with simple semantic highlighting.
///
/// This intentionally keeps formatting deterministic and dependency-light.
pub fn render_stream_line(raw_line: &str) -> Line<'static> {
    let trimmed = raw_line.trim();

    if let Some(rest) = trimmed.strip_prefix("turn.started ") {
        return Line::from(vec![
            Span::styled(
                "▶ ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Turn started ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(rest.to_string()),
        ]);
    }

    if let Some(rest) = trimmed.strip_prefix("turn.completed ") {
        let color = if rest.contains("status=failed") {
            Color::Red
        } else if rest.contains("status=interrupted") {
            Color::Yellow
        } else {
            Color::Green
        };
        return Line::from(vec![
            Span::styled(
                "✓ ",
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Turn completed ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(rest.to_string()),
        ]);
    }

    if let Some(rest) = trimmed.strip_prefix("turn.idle ") {
        return Line::from(vec![
            Span::styled("• ", Style::default().fg(Color::Blue)),
            Span::styled("Turn idle ", Style::default().fg(Color::Blue)),
            Span::raw(rest.to_string()),
        ]);
    }

    if let Some(rest) = trimmed.strip_prefix("stream.error ") {
        return Line::from(vec![
            Span::styled(
                "! ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Stream error ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(rest.to_string()),
        ]);
    }

    if let Some(rest) = trimmed.strip_prefix("stream.counters ") {
        return Line::from(vec![
            Span::styled(
                "≈ ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Counters ", Style::default().fg(Color::Yellow)),
            Span::raw(rest.to_string()),
        ]);
    }

    if trimmed.starts_with("```") {
        return Line::from(Span::styled(
            trimmed.to_string(),
            Style::default().fg(Color::Cyan),
        ));
    }

    if trimmed.starts_with('#') {
        return Line::from(Span::styled(
            trimmed.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }

    if trimmed.starts_with("- ") {
        let body = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        return Line::from(vec![
            Span::styled("• ", Style::default().fg(Color::Yellow)),
            Span::raw(body.to_string()),
        ]);
    }

    Line::from(Span::raw(trimmed.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_turn_started_event() {
        let event = DaemonStreamEvent::TurnStarted {
            agent: "arch-ctm".to_string(),
            thread_id: "th-1".to_string(),
            turn_id: "turn-1".to_string(),
            transport: "app-server".to_string(),
        };
        let line = format_daemon_event_line(&event);
        assert!(line.starts_with("turn.started"));
        assert!(line.contains("transport=app-server"));
    }

    #[test]
    fn renders_turn_completed_prefix() {
        let line = render_stream_line("turn.completed status=completed transport=mcp id=t1");
        let rendered: String = line
            .spans
            .into_iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(rendered.contains("Turn completed"));
    }

    #[test]
    fn renders_markdown_heading_bold() {
        let line = render_stream_line("## Heading");
        assert_eq!(line.spans.len(), 1);
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }
}
