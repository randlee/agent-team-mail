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

    if let Some(rest) = trimmed.strip_prefix("turn.interrupted ") {
        return Line::from(vec![
            Span::styled(
                "⏹ ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Turn interrupted ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(rest.to_string()),
        ]);
    }

    if let Some(rest) = trimmed.strip_prefix("approval.request ") {
        return Line::from(vec![
            Span::styled(
                "? ",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Approval requested ",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(rest.to_string()),
        ]);
    }

    if let Some(rest) = trimmed.strip_prefix("approval.rejected ") {
        return Line::from(vec![
            Span::styled(
                "✗ ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Approval rejected ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(rest.to_string()),
        ]);
    }

    if let Some(rest) = trimmed.strip_prefix("approval.resolved ") {
        return Line::from(vec![
            Span::styled(
                "✓ ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Approval resolved ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
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
    use std::fs;
    use std::path::PathBuf;

    fn rendered_text(line: Line<'static>) -> String {
        line.spans
            .into_iter()
            .map(|s| s.content.to_string())
            .collect::<String>()
    }

    fn renderer_fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parity/renderer")
    }

    #[test]
    fn renders_turn_completed_prefix() {
        let line = render_stream_line("turn.completed status=completed transport=mcp id=t1");
        let rendered = rendered_text(line);
        assert!(rendered.contains("Turn completed"));
    }

    #[test]
    fn renders_markdown_heading_bold() {
        let line = render_stream_line("## Heading");
        assert_eq!(line.spans.len(), 1);
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn renders_approval_prefix() {
        let line = render_stream_line("approval.request allow command");
        let rendered = rendered_text(line);
        assert!(rendered.contains("Approval requested"));
    }

    #[test]
    fn parity_render_fixture_combined_flow() {
        let scenario = renderer_fixture_dir().join("combined-flow");
        let raw_events =
            fs::read_to_string(scenario.join("normalized.events.jsonl")).expect("events fixture");
        let actual_lines: Vec<String> = raw_events
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("valid JSON line"))
            .map(|v| {
                let raw = v
                    .get("line")
                    .and_then(|v| v.as_str())
                    .expect("line field in fixture");
                rendered_text(render_stream_line(raw))
            })
            .collect();
        let actual = format!("{}\n", actual_lines.join("\n"));

        let expected_120 = fs::read_to_string(scenario.join("viewport-120x36.snap"))
            .expect("120x36 snapshot")
            .replace("\r\n", "\n");
        let expected_80 = fs::read_to_string(scenario.join("viewport-80x24.snap"))
            .expect("80x24 snapshot")
            .replace("\r\n", "\n");

        // Current parity baseline uses viewport-independent line snapshots.
        // Keep both fixtures explicit until full frame-buffer snapshots land.
        assert_eq!(actual, expected_120, "renderer mismatch for 120x36 snapshot");
        assert_eq!(actual, expected_80, "renderer mismatch for 80x24 snapshot");
    }
}
