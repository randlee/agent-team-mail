//! Codex watch-pane rendering helpers adapted for ATM TUI.
//!
//! This module provides a structured rendering foundation for stream lines:
//! - classify raw normalized lines into render classes,
//! - apply class-specific icon/label/styling,
//! - wrap body content using terminal-width-aware layout constraints.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderClass {
    TurnStarted,
    TurnCompleted,
    TurnIdle,
    TurnInterrupted,
    ApprovalExecRequest,
    ApprovalPatchRequest,
    ApprovalReviewEntered,
    ApprovalReviewExited,
    ApprovalReviewRejected,
    ApprovalReviewResolved,
    ApprovalRequestedLegacy,
    ApprovalRejectedLegacy,
    ApprovalResolvedLegacy,
    ToolExec,
    FileEdit,
    CmdBegin,
    CmdOutput,
    CmdCompleted,
    CmdError,
    ToolMcpBegin,
    ToolMcpEnd,
    ToolWebSearchBegin,
    ToolWebSearchEnd,
    SessionConfigured,
    SessionTokenCount,
    PlanUpdate,
    PlanDelta,
    Unsupported,
    StreamError,
    InputClient,
    InputUserSteer,
    InputAtmMail,
    ElicitationRequest,
    StreamCounters,
    MarkdownFence,
    MarkdownHeading,
    MarkdownBullet,
    Plain,
}

#[derive(Debug, Clone)]
struct RenderSpec {
    icon: &'static str,
    icon_style: Style,
    label: &'static str,
    label_style: Style,
    body_style: Style,
}

#[derive(Debug, Clone)]
struct ParsedLine {
    class: RenderClass,
    body: String,
}

/// Test helper for rendering one normalized stream line as a single line.
///
/// Runtime rendering should use `render_stream_lines_with_width`.
#[cfg(test)]
pub fn render_stream_line(raw_line: &str) -> Line<'static> {
    render_stream_lines_with_width(raw_line, usize::MAX)
        .into_iter()
        .next()
        .unwrap_or_else(|| Line::from(Span::raw(String::new())))
}

/// Render one normalized stream line into one or more terminal-width-aware lines.
pub fn render_stream_lines_with_width(raw_line: &str, max_width: usize) -> Vec<Line<'static>> {
    let parsed = parse_stream_line(raw_line);
    render_parsed_line(&parsed, max_width.max(1))
}

fn parse_stream_line(raw_line: &str) -> ParsedLine {
    let trimmed = raw_line.trim();

    if let Some(rest) = trimmed.strip_prefix("turn.started ") {
        return ParsedLine {
            class: RenderClass::TurnStarted,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("turn.completed ") {
        return ParsedLine {
            class: RenderClass::TurnCompleted,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("turn.idle ") {
        return ParsedLine {
            class: RenderClass::TurnIdle,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("turn.interrupted ") {
        return ParsedLine {
            class: RenderClass::TurnInterrupted,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("approval.exec.request ") {
        return ParsedLine {
            class: RenderClass::ApprovalExecRequest,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("approval.patch.request ") {
        return ParsedLine {
            class: RenderClass::ApprovalPatchRequest,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("approval.review.entered ") {
        return ParsedLine {
            class: RenderClass::ApprovalReviewEntered,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("approval.review.exited ") {
        return ParsedLine {
            class: RenderClass::ApprovalReviewExited,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("approval.review.rejected ") {
        return ParsedLine {
            class: RenderClass::ApprovalReviewRejected,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("approval.review.resolved ") {
        return ParsedLine {
            class: RenderClass::ApprovalReviewResolved,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("file.edit ") {
        return ParsedLine {
            class: RenderClass::FileEdit,
            body: rest.to_string(),
        };
    }
    // Legacy tokens retained for older fixtures/transcripts.
    if let Some(rest) = trimmed.strip_prefix("cmd ") {
        return ParsedLine {
            class: RenderClass::ToolExec,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("approval.request ") {
        return ParsedLine {
            class: RenderClass::ApprovalRequestedLegacy,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("approval.rejected ") {
        return ParsedLine {
            class: RenderClass::ApprovalRejectedLegacy,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("approval.resolved ") {
        return ParsedLine {
            class: RenderClass::ApprovalResolvedLegacy,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("cmd.begin ") {
        return ParsedLine {
            class: RenderClass::CmdBegin,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("cmd.output ") {
        return ParsedLine {
            class: RenderClass::CmdOutput,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("cmd.completed ") {
        return ParsedLine {
            class: RenderClass::CmdCompleted,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("cmd.error ") {
        return ParsedLine {
            class: RenderClass::CmdError,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("tool.mcp.begin ") {
        return ParsedLine {
            class: RenderClass::ToolMcpBegin,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("tool.mcp.end ") {
        return ParsedLine {
            class: RenderClass::ToolMcpEnd,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("tool.web_search.begin ") {
        return ParsedLine {
            class: RenderClass::ToolWebSearchBegin,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("tool.web_search.end ") {
        return ParsedLine {
            class: RenderClass::ToolWebSearchEnd,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("session.configured ") {
        return ParsedLine {
            class: RenderClass::SessionConfigured,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("session.token_count ") {
        return ParsedLine {
            class: RenderClass::SessionTokenCount,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("plan.update ") {
        return ParsedLine {
            class: RenderClass::PlanUpdate,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("plan.delta ") {
        return ParsedLine {
            class: RenderClass::PlanDelta,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("unsupported.") {
        return ParsedLine {
            class: RenderClass::Unsupported,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("unknown.") {
        return ParsedLine {
            class: RenderClass::Unsupported,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("stream.error ") {
        return ParsedLine {
            class: RenderClass::StreamError,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("input.client ") {
        return ParsedLine {
            class: RenderClass::InputClient,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("input.user_steer ") {
        return ParsedLine {
            class: RenderClass::InputUserSteer,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("input.atm_mail ") {
        return ParsedLine {
            class: RenderClass::InputAtmMail,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("elicitation.request ") {
        return ParsedLine {
            class: RenderClass::ElicitationRequest,
            body: rest.to_string(),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("stream.counters ") {
        return ParsedLine {
            class: RenderClass::StreamCounters,
            body: rest.to_string(),
        };
    }
    if trimmed.starts_with("```") {
        return ParsedLine {
            class: RenderClass::MarkdownFence,
            body: trimmed.to_string(),
        };
    }
    if trimmed.starts_with('#') {
        return ParsedLine {
            class: RenderClass::MarkdownHeading,
            body: trimmed.to_string(),
        };
    }
    if let Some(body) = trimmed.strip_prefix("- ") {
        return ParsedLine {
            class: RenderClass::MarkdownBullet,
            body: body.to_string(),
        };
    }

    ParsedLine {
        class: RenderClass::Plain,
        body: trimmed.to_string(),
    }
}

fn render_parsed_line(parsed: &ParsedLine, max_width: usize) -> Vec<Line<'static>> {
    match parsed.class {
        RenderClass::Plain => wrap_plain_line(&parsed.body, max_width, Style::default()),
        RenderClass::MarkdownFence => {
            wrap_plain_line(&parsed.body, max_width, Style::default().fg(Color::Cyan))
        }
        RenderClass::MarkdownHeading => wrap_plain_line(
            &parsed.body,
            max_width,
            Style::default().add_modifier(Modifier::BOLD),
        ),
        RenderClass::MarkdownBullet => render_bullet_line(&parsed.body, max_width),
        class => render_class_line(class, &parsed.body, max_width),
    }
}

fn render_class_line(class: RenderClass, body: &str, max_width: usize) -> Vec<Line<'static>> {
    let spec = render_spec(class, body);
    let icon_width = spec.icon.chars().count();
    let label_width = spec.label.chars().count();

    let chunks = split_widths(icon_width, label_width, max_width);
    let body_width = chunks.2.max(1);
    let wrapped_body = wrap_text(body, body_width);

    let mut out = Vec::new();
    for (idx, chunk) in wrapped_body.iter().enumerate() {
        if idx == 0 {
            out.push(Line::from(vec![
                Span::styled(spec.icon.to_string(), spec.icon_style),
                Span::styled(spec.label.to_string(), spec.label_style),
                Span::styled(chunk.to_string(), spec.body_style),
            ]));
        } else {
            let indent = " ".repeat(icon_width + label_width);
            out.push(Line::from(vec![
                Span::raw(indent),
                Span::styled(chunk.to_string(), spec.body_style),
            ]));
        }
    }
    if out.is_empty() {
        out.push(Line::from(vec![
            Span::styled(spec.icon.to_string(), spec.icon_style),
            Span::styled(spec.label.to_string(), spec.label_style),
        ]));
    }
    out
}

fn render_bullet_line(body: &str, max_width: usize) -> Vec<Line<'static>> {
    let icon = "• ";
    let icon_width = icon.chars().count();
    let body_width = max_width.saturating_sub(icon_width).max(1);
    let wrapped = wrap_text(body, body_width);
    let mut out = Vec::new();
    for (idx, chunk) in wrapped.iter().enumerate() {
        if idx == 0 {
            out.push(Line::from(vec![
                Span::styled(icon.to_string(), Style::default().fg(Color::Yellow)),
                Span::raw(chunk.to_string()),
            ]));
        } else {
            out.push(Line::from(vec![
                Span::raw(" ".repeat(icon_width)),
                Span::raw(chunk.to_string()),
            ]));
        }
    }
    if out.is_empty() {
        out.push(Line::from(vec![
            Span::styled(icon.to_string(), Style::default().fg(Color::Yellow)),
            Span::raw(String::new()),
        ]));
    }
    out
}

fn render_spec(class: RenderClass, body: &str) -> RenderSpec {
    match class {
        RenderClass::TurnStarted => RenderSpec {
            icon: "▶ ",
            icon_style: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            label: "Turn started ",
            label_style: Style::default().add_modifier(Modifier::BOLD),
            body_style: Style::default(),
        },
        RenderClass::TurnCompleted => {
            let color = if body.contains("status=failed") {
                Color::Red
            } else if body.contains("status=interrupted") {
                Color::Yellow
            } else {
                Color::Green
            };
            RenderSpec {
                icon: "✓ ",
                icon_style: Style::default().fg(color).add_modifier(Modifier::BOLD),
                label: "Turn completed ",
                label_style: Style::default().add_modifier(Modifier::BOLD),
                body_style: Style::default(),
            }
        }
        RenderClass::TurnIdle => RenderSpec {
            icon: "• ",
            icon_style: Style::default().fg(Color::Blue),
            label: "Turn idle ",
            label_style: Style::default().fg(Color::Blue),
            body_style: Style::default(),
        },
        RenderClass::TurnInterrupted => RenderSpec {
            icon: "⏹ ",
            icon_style: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            label: "Turn interrupted ",
            label_style: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            body_style: Style::default(),
        },
        RenderClass::ApprovalExecRequest => RenderSpec {
            icon: "? ",
            icon_style: Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            label: "Exec approval ",
            label_style: Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            body_style: Style::default(),
        },
        RenderClass::ApprovalPatchRequest => RenderSpec {
            icon: "? ",
            icon_style: Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            label: "Patch approval ",
            label_style: Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            body_style: Style::default(),
        },
        RenderClass::ApprovalReviewEntered => RenderSpec {
            icon: "⎔ ",
            icon_style: Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            label: "Review entered ",
            label_style: Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            body_style: Style::default(),
        },
        RenderClass::ApprovalReviewExited => RenderSpec {
            icon: "⎔ ",
            icon_style: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            label: "Review exited ",
            label_style: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            body_style: Style::default(),
        },
        RenderClass::ApprovalReviewRejected => RenderSpec {
            icon: "✗ ",
            icon_style: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            label: "Review rejected ",
            label_style: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            body_style: Style::default(),
        },
        RenderClass::ApprovalReviewResolved => RenderSpec {
            icon: "✓ ",
            icon_style: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            label: "Review resolved ",
            label_style: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            body_style: Style::default(),
        },
        RenderClass::ToolExec => RenderSpec {
            icon: "› ",
            icon_style: Style::default().fg(Color::Cyan),
            label: "Command ",
            label_style: Style::default().fg(Color::Cyan),
            body_style: Style::default(),
        },
        RenderClass::FileEdit => RenderSpec {
            icon: "Δ ",
            icon_style: Style::default().fg(Color::Yellow),
            label: "File edit ",
            label_style: Style::default().fg(Color::Yellow),
            body_style: Style::default(),
        },
        RenderClass::ApprovalRequestedLegacy => RenderSpec {
            icon: "? ",
            icon_style: Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            label: "Approval requested ",
            label_style: Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            body_style: Style::default(),
        },
        RenderClass::ApprovalRejectedLegacy => RenderSpec {
            icon: "✗ ",
            icon_style: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            label: "Approval rejected ",
            label_style: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            body_style: Style::default(),
        },
        RenderClass::ApprovalResolvedLegacy => RenderSpec {
            icon: "✓ ",
            icon_style: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            label: "Approval resolved ",
            label_style: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            body_style: Style::default(),
        },
        RenderClass::CmdBegin => RenderSpec {
            icon: "» ",
            icon_style: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            label: "Command begin ",
            label_style: Style::default().fg(Color::Cyan),
            body_style: Style::default(),
        },
        RenderClass::CmdOutput => RenderSpec {
            icon: "› ",
            icon_style: Style::default().fg(Color::Cyan),
            label: "Command output ",
            label_style: Style::default().fg(Color::Cyan),
            body_style: Style::default(),
        },
        RenderClass::CmdCompleted => RenderSpec {
            icon: "✓ ",
            icon_style: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            label: "Command done ",
            label_style: Style::default().fg(Color::Green),
            body_style: Style::default(),
        },
        RenderClass::CmdError => RenderSpec {
            icon: "✗ ",
            icon_style: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            label: "Command error ",
            label_style: Style::default().fg(Color::Red),
            body_style: Style::default(),
        },
        RenderClass::ToolMcpBegin => RenderSpec {
            icon: "⋯ ",
            icon_style: Style::default().fg(Color::Yellow),
            label: "MCP tool begin ",
            label_style: Style::default().fg(Color::Yellow),
            body_style: Style::default(),
        },
        RenderClass::ToolMcpEnd => RenderSpec {
            icon: "⋯ ",
            icon_style: Style::default().fg(Color::Yellow),
            label: "MCP tool end ",
            label_style: Style::default().fg(Color::Yellow),
            body_style: Style::default(),
        },
        RenderClass::ToolWebSearchBegin => RenderSpec {
            icon: "⋯ ",
            icon_style: Style::default().fg(Color::Yellow),
            label: "Search begin ",
            label_style: Style::default().fg(Color::Yellow),
            body_style: Style::default(),
        },
        RenderClass::ToolWebSearchEnd => RenderSpec {
            icon: "⋯ ",
            icon_style: Style::default().fg(Color::Yellow),
            label: "Search end ",
            label_style: Style::default().fg(Color::Yellow),
            body_style: Style::default(),
        },
        RenderClass::SessionConfigured => RenderSpec {
            icon: "◉ ",
            icon_style: Style::default().fg(Color::Blue),
            label: "Session configured ",
            label_style: Style::default().fg(Color::Blue),
            body_style: Style::default(),
        },
        RenderClass::SessionTokenCount => RenderSpec {
            icon: "◉ ",
            icon_style: Style::default().fg(Color::Blue),
            label: "Token count ",
            label_style: Style::default().fg(Color::Blue),
            body_style: Style::default(),
        },
        RenderClass::PlanUpdate => RenderSpec {
            icon: "☰ ",
            icon_style: Style::default().fg(Color::Yellow),
            label: "Plan update ",
            label_style: Style::default().fg(Color::Yellow),
            body_style: Style::default(),
        },
        RenderClass::PlanDelta => RenderSpec {
            icon: "☰ ",
            icon_style: Style::default().fg(Color::Yellow),
            label: "Plan delta ",
            label_style: Style::default().fg(Color::Yellow),
            body_style: Style::default(),
        },
        RenderClass::Unsupported => RenderSpec {
            icon: "? ",
            icon_style: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            label: "Unsupported ",
            label_style: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            body_style: Style::default(),
        },
        RenderClass::StreamError => RenderSpec {
            icon: "! ",
            icon_style: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            label: "Stream error ",
            label_style: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            body_style: Style::default(),
        },
        RenderClass::InputClient => RenderSpec {
            icon: "→ ",
            icon_style: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            label: "Client input ",
            label_style: Style::default().fg(Color::Cyan),
            body_style: Style::default(),
        },
        RenderClass::InputUserSteer => RenderSpec {
            icon: "↪ ",
            icon_style: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            label: "Steer input ",
            label_style: Style::default().fg(Color::Yellow),
            body_style: Style::default(),
        },
        RenderClass::InputAtmMail => RenderSpec {
            icon: "✉ ",
            icon_style: Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            label: "ATM mail ",
            label_style: Style::default().fg(Color::Magenta),
            body_style: Style::default(),
        },
        RenderClass::ElicitationRequest => RenderSpec {
            icon: "? ",
            icon_style: Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            label: "Input requested ",
            label_style: Style::default().fg(Color::Magenta),
            body_style: Style::default(),
        },
        RenderClass::StreamCounters => RenderSpec {
            icon: "≈ ",
            icon_style: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            label: "Counters ",
            label_style: Style::default().fg(Color::Yellow),
            body_style: Style::default(),
        },
        RenderClass::MarkdownFence
        | RenderClass::MarkdownHeading
        | RenderClass::MarkdownBullet
        | RenderClass::Plain => RenderSpec {
            icon: "",
            icon_style: Style::default(),
            label: "",
            label_style: Style::default(),
            body_style: Style::default(),
        },
    }
}

fn split_widths(
    icon_width: usize,
    label_width: usize,
    total_width: usize,
) -> (usize, usize, usize) {
    let rect = Rect {
        x: 0,
        y: 0,
        width: total_width.min(u16::MAX as usize) as u16,
        height: 1,
    };
    let chunks = Layout::horizontal([
        Constraint::Length(icon_width.min(u16::MAX as usize) as u16),
        Constraint::Length(label_width.min(u16::MAX as usize) as u16),
        Constraint::Min(0),
    ])
    .split(rect);
    (
        chunks[0].width as usize,
        chunks[1].width as usize,
        chunks[2].width as usize,
    )
}

fn wrap_plain_line(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let wrapped = wrap_text(text, width.max(1));
    if wrapped.is_empty() {
        return vec![Line::from(Span::styled(String::new(), style))];
    }
    wrapped
        .into_iter()
        .map(|chunk| Line::from(Span::styled(chunk, style)))
        .collect()
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                push_word_chunks(word, width, &mut current, &mut out);
                continue;
            }
            if current.chars().count() + 1 + word.chars().count() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                out.push(std::mem::take(&mut current));
                push_word_chunks(word, width, &mut current, &mut out);
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn push_word_chunks(word: &str, width: usize, current: &mut String, out: &mut Vec<String>) {
    if word.chars().count() <= width {
        current.push_str(word);
        return;
    }
    let mut buf = String::new();
    for ch in word.chars() {
        if buf.chars().count() >= width {
            out.push(std::mem::take(&mut buf));
        }
        buf.push(ch);
    }
    if !buf.is_empty() {
        if current.is_empty() {
            current.push_str(&buf);
        } else {
            out.push(std::mem::take(current));
            current.push_str(&buf);
        }
    }
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
        let line = render_stream_line("approval.exec.request allow command");
        let rendered = rendered_text(line);
        assert!(rendered.contains("Exec approval"));
    }

    #[test]
    fn renders_tool_exec_and_file_edit_prefixes() {
        let cmd = rendered_text(render_stream_line("cmd ls -la"));
        assert!(cmd.contains("Command"));
        let file_edit = rendered_text(render_stream_line("file.edit patch begin"));
        assert!(file_edit.contains("File edit"));
    }

    #[test]
    fn wraps_structured_line_by_width() {
        let lines = render_stream_lines_with_width(
            "approval.exec.request allow command with many words",
            24,
        );
        assert!(lines.len() >= 2, "expected wrapped lines");
        let first = rendered_text(lines[0].clone());
        assert!(first.contains("Exec approval"));
        let second = rendered_text(lines[1].clone());
        assert!(second.starts_with(" ".repeat("".len() + "Exec approval ".len()).as_str()));
    }

    #[test]
    fn renders_new_event_family_prefixes() {
        let samples = [
            ("approval.exec.request allow", "Exec approval"),
            ("approval.patch.request allow", "Patch approval"),
            ("approval.review.entered waiting", "Review entered"),
            ("cmd.begin run", "Command begin"),
            ("cmd.output out", "Command output"),
            ("tool.mcp.begin read_file", "MCP tool begin"),
            ("tool.mcp.end read_file", "MCP tool end"),
            ("tool.web_search.begin q", "Search begin"),
            ("tool.web_search.end q", "Search end"),
            ("session.configured s1", "Session configured"),
            ("session.token_count 321", "Token count"),
            ("plan.update added-step", "Plan update"),
            ("plan.delta +item", "Plan delta"),
            ("unsupported.future_event", "Unsupported"),
        ];
        for (raw, expected) in samples {
            let rendered = rendered_text(render_stream_line(raw));
            assert!(
                rendered.contains(expected),
                "expected '{expected}' in rendered line for '{raw}', got '{rendered}'"
            );
        }
    }

    #[test]
    fn wraps_plain_text_by_width() {
        let lines = render_stream_lines_with_width("a b c d e f g", 5);
        assert!(lines.len() >= 2, "plain lines should wrap");
    }

    #[test]
    fn parity_render_fixture_scenarios() {
        let scenarios = [
            "combined-flow",
            "multi-item",
            "fatal-error",
            "unknown-event",
            "atm-mail",
            "user-steer",
            "session-attach",
            "detach-reattach",
            "cross-transport",
            "degraded-events",
        ];

        for scenario_name in scenarios {
            let scenario = renderer_fixture_dir().join(scenario_name);
            let raw_events = fs::read_to_string(scenario.join("normalized.events.jsonl"))
                .expect("events fixture");
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
            assert_eq!(
                actual, expected_120,
                "renderer mismatch for 120x36 snapshot in scenario {scenario_name}"
            );
            assert_eq!(
                actual, expected_80,
                "renderer mismatch for 80x24 snapshot in scenario {scenario_name}"
            );
        }
    }
}
