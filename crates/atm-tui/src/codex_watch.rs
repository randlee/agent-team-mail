//! Codex watch-pane rendering helpers adapted for ATM TUI.
//!
//! This module provides a light-weight rendering layer for stream lines and
//! daemon turn events so the watch pane aligns with Codex CLI-style transcript
//! and status presentation.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

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
