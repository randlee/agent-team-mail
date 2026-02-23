//! Codex watch-pane rendering helpers adapted for ATM TUI.
//!
//! This module provides a light-weight rendering layer for stream lines and
//! daemon turn events so the watch pane aligns with Codex CLI-style transcript
//! and status presentation.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::codex_vendor::text_formatting::format_json_compact;

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

/// Format a direct watch-stream JSON frame to a transcript line with source
/// attribution badges (`kind/actor/channel`).
pub fn format_watch_frame_line(frame: &serde_json::Value) -> String {
    let source_kind = frame
        .pointer("/source/kind")
        .and_then(|v| v.as_str())
        .unwrap_or("client_prompt");
    let source_actor = frame
        .pointer("/source/actor")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let source_channel = frame
        .pointer("/source/channel")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let event = frame.get("event").unwrap_or(frame);
    let kind = event
        .pointer("/params/type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let text = event
        .pointer("/params/delta")
        .and_then(|v| v.as_str())
        .or_else(|| event.pointer("/params/text").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/output").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/message").and_then(|v| v.as_str()))
        .unwrap_or("");
    let text = format_json_compact(text).unwrap_or_else(|| text.to_string());

    let source_badge = format!("[{source_kind}|{source_actor}|{source_channel}]");
    match kind {
        "turn_started" | "turn_completed" | "turn_idle" | "item_started" | "item_completed" => {
            format!("{source_badge} {kind}")
        }
        "agent_message_delta" | "agent_message" | "agent_message_chunk" => {
            format!("{source_badge} assistant: {text}")
        }
        "exec_command_output_delta" | "exec_command_completed" | "exec_command_error" => {
            format!("{source_badge} cmd: {text}")
        }
        "reasoning_content_delta" | "agent_reasoning_delta" | "reasoning_content" => {
            format!("{source_badge} reasoning: {text}")
        }
        "stream_error" | "error" => format!("{source_badge} stream.error {text}"),
        _ => {
            if text.is_empty() {
                format!("{source_badge} {kind}")
            } else {
                format!("{source_badge} {kind}: {text}")
            }
        }
    }
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

    #[test]
    fn formats_watch_frame_with_source_badge() {
        let frame = serde_json::json!({
            "source": {"kind":"atm_mail","actor":"arch-atm@atm-dev","channel":"mail_injector"},
            "event": {"params":{"type":"turn_started"}}
        });
        let line = format_watch_frame_line(&frame);
        assert!(line.contains("[atm_mail|arch-atm@atm-dev|mail_injector]"));
        assert!(line.contains("turn_started"));
    }
}
