//! `attach` subcommand — interactive attached mode for a live session.
//!
//! This command binds to one `agent_id`, tails the existing watch-stream feed
//! for read-path continuity, and routes user controls via daemon `control`
//! requests for write-path parity.

use crate::cli::AttachArgs;
use agent_team_mail_core::control::{
    CONTROL_SCHEMA_VERSION, ControlAck, ControlAction, ControlRequest,
};
use agent_team_mail_core::daemon_client::{query_agent_state, send_control};
use serde::Serialize;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};
use tokio::time::interval;

const WATCH_ATTACH_REPLAY_MAX_FRAMES: usize = 50;
const WATCH_ATTACH_REPLAY_SCAN_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlVerb {
    Help,
    Interrupt,
    Detach,
    Approve,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AttachInput {
    AgentText(String),
    Control {
        verb: ControlVerb,
        arg: Option<String>,
    },
    Ignore,
}

#[derive(Debug, Clone, Serialize)]
struct AttachedRenderEnvelope {
    v: u8,
    mode: &'static str,
    agent_id: String,
    class: String,
    applicability: String,
    source_kind: String,
    source_actor: String,
    source_channel: String,
    event_type: String,
    text: String,
    is_turn_boundary: bool,
    raw: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventApplicability {
    Required,
    Degraded,
    OutOfScope,
}

impl EventApplicability {
    fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Degraded => "degraded",
            Self::OutOfScope => "out_of_scope",
        }
    }
}

pub async fn run(args: AttachArgs) -> anyhow::Result<()> {
    let team = resolved_team(args.team.as_deref());
    let poll_ms = args.poll_ms.max(50);

    let state = query_agent_state(&args.agent_id, &team)?;
    if state.is_none() {
        eprintln!(
            "warning: agent '{}' is not currently reported by daemon list/state; attach will still attempt stream/control binding",
            args.agent_id
        );
    }

    let watch_path = watch_feed_path(&args.agent_id).ok_or_else(|| {
        anyhow::anyhow!("failed to resolve watch feed path (ATM_HOME/HOME not available)")
    })?;

    print_attach_banner(&args.agent_id, &team, &watch_path);
    print_input_contract();

    let mut stream_pos: u64 = 0;
    let mut ticker = interval(Duration::from_millis(poll_ms));
    let mut stdin_lines = BufReader::new(tokio::io::stdin()).lines();

    // Initial attach replay (bounded).
    if let Ok((replay, new_pos)) = tail_watch_stream_file(&watch_path, 0, &args.agent_id).await {
        stream_pos = new_pos;
        for frame in replay {
            print_frame(&args.agent_id, frame, args.json)?;
        }
    }

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Ok((frames, new_pos)) = tail_watch_stream_file(&watch_path, stream_pos, &args.agent_id).await {
                    stream_pos = new_pos;
                    for frame in frames {
                        print_frame(&args.agent_id, frame, args.json)?;
                    }
                }
            }
            maybe_line = stdin_lines.next_line() => {
                let Some(line) = maybe_line? else {
                    break;
                };
                match parse_attach_input(&line) {
                    AttachInput::Ignore => {}
                    AttachInput::AgentText(text) => {
                        // Default route is agent input; control verbs must be prefixed with ':'.
                        let ack = send_stdin_control(&team, &args.agent_id, &text)?;
                        println!("ack {}", format_ack(&ack));
                    }
                    AttachInput::Control { verb, arg } => {
                        match verb {
                            ControlVerb::Help => print_input_contract(),
                            ControlVerb::Interrupt => {
                                let ack = send_interrupt_control(&team, &args.agent_id)?;
                                println!("ack {}", format_ack(&ack));
                            }
                            ControlVerb::Detach => break,
                            ControlVerb::Approve => {
                                let payload = arg.unwrap_or_else(|| "approve".to_string());
                                let ack = send_stdin_control(&team, &args.agent_id, &payload)?;
                                println!("ack {}", format_ack(&ack));
                            }
                            ControlVerb::Reject => {
                                let payload = arg.unwrap_or_else(|| "reject".to_string());
                                let ack = send_stdin_control(&team, &args.agent_id, &payload)?;
                                println!("ack {}", format_ack(&ack));
                            }
                        }
                    }
                }
            }
        }
    }

    println!("detached from {}", args.agent_id);
    Ok(())
}

fn print_attach_banner(agent_id: &str, team: &str, watch_path: &Path) {
    println!("attach mode: agent_id={agent_id} team={team}");
    println!("watch feed: {}", watch_path.display());
}

fn print_input_contract() {
    println!("input routing:");
    println!("  plain text      -> agent input (stdin control)");
    println!("  :interrupt      -> interrupt control request");
    println!("  :approve [text] -> approval response via stdin");
    println!("  :reject [text]  -> rejection response via stdin");
    println!("  :help           -> show routing contract");
    println!("  :detach         -> detach and exit");
}

fn resolved_team(arg: Option<&str>) -> String {
    if let Some(team) = arg
        && !team.trim().is_empty()
    {
        return team.trim().to_string();
    }
    if let Ok(team) = std::env::var("ATM_TEAM")
        && !team.trim().is_empty()
    {
        return team.trim().to_string();
    }
    "atm-dev".to_string()
}

fn parse_attach_input(line: &str) -> AttachInput {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return AttachInput::Ignore;
    }

    if !trimmed.starts_with(':') {
        return AttachInput::AgentText(trimmed.to_string());
    }

    let command = trimmed.trim_start_matches(':').trim();
    if command.is_empty() {
        return AttachInput::Ignore;
    }

    let mut parts = command.splitn(2, ' ');
    let verb = parts.next().unwrap_or_default().to_ascii_lowercase();
    let arg = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    match verb.as_str() {
        "help" | "h" | "?" => AttachInput::Control {
            verb: ControlVerb::Help,
            arg: None,
        },
        "interrupt" | "i" => AttachInput::Control {
            verb: ControlVerb::Interrupt,
            arg: None,
        },
        "detach" | "quit" | "exit" => AttachInput::Control {
            verb: ControlVerb::Detach,
            arg: None,
        },
        "approve" => AttachInput::Control {
            verb: ControlVerb::Approve,
            arg,
        },
        "reject" => AttachInput::Control {
            verb: ControlVerb::Reject,
            arg,
        },
        _ => AttachInput::Control {
            verb: ControlVerb::Help,
            arg: None,
        },
    }
}

fn send_stdin_control(team: &str, agent_id: &str, text: &str) -> anyhow::Result<ControlAck> {
    let req = ControlRequest {
        v: CONTROL_SCHEMA_VERSION,
        request_id: uuid::Uuid::new_v4().to_string(),
        msg_type: "control.stdin.request".to_string(),
        signal: None,
        sent_at: chrono::Utc::now().to_rfc3339(),
        team: team.to_string(),
        session_id: String::new(),
        agent_id: agent_id.to_string(),
        sender: "attach_cli".to_string(),
        action: ControlAction::Stdin,
        payload: Some(text.to_string()),
        content_ref: None,
    };
    send_control(&req)
}

fn send_interrupt_control(team: &str, agent_id: &str) -> anyhow::Result<ControlAck> {
    let req = ControlRequest {
        v: CONTROL_SCHEMA_VERSION,
        request_id: uuid::Uuid::new_v4().to_string(),
        msg_type: "control.interrupt.request".to_string(),
        signal: Some("interrupt".to_string()),
        sent_at: chrono::Utc::now().to_rfc3339(),
        team: team.to_string(),
        session_id: String::new(),
        agent_id: agent_id.to_string(),
        sender: "attach_cli".to_string(),
        action: ControlAction::Interrupt,
        payload: None,
        content_ref: None,
    };
    send_control(&req)
}

fn format_ack(ack: &ControlAck) -> String {
    let detail = ack.detail.as_deref().unwrap_or("");
    format!(
        "request_id={} result={:?} duplicate={} {}",
        ack.request_id, ack.result, ack.duplicate, detail
    )
}

fn watch_feed_path(agent_id: &str) -> Option<PathBuf> {
    let safe_id: Cow<str> = if agent_id.contains('/') || agent_id.contains('\\') {
        Cow::Owned(agent_id.replace(['/', '\\'], "_"))
    } else {
        Cow::Borrowed(agent_id)
    };
    if let Ok(atm_home) = std::env::var("ATM_HOME") {
        let trimmed = atm_home.trim();
        if !trimmed.is_empty() {
            return Some(
                PathBuf::from(trimmed)
                    .join("watch-stream")
                    .join(format!("{safe_id}.jsonl")),
            );
        }
    }
    let home = agent_team_mail_core::home::get_home_dir().ok()?;
    Some(
        home.join(".config/atm/watch-stream")
            .join(format!("{safe_id}.jsonl")),
    )
}

async fn tail_watch_stream_file(
    path: &Path,
    pos: u64,
    agent_id: &str,
) -> anyhow::Result<(Vec<Value>, u64)> {
    use tokio::fs::File;

    if !path.exists() {
        return Ok((Vec::new(), pos));
    }

    let mut file = File::open(path).await?;
    let file_len = file.metadata().await?.len();
    if pos == 0 || file_len < pos {
        return read_watch_replay_for_attach(path, &mut file, file_len, agent_id).await;
    }
    if file_len == pos {
        return Ok((Vec::new(), pos));
    }

    file.seek(std::io::SeekFrom::Start(pos)).await?;
    let read_len = (file_len - pos).min(256 * 1024) as usize;
    let mut buf = vec![0u8; read_len];
    let n = file.read(&mut buf).await?;
    buf.truncate(n);

    let mut out = Vec::new();
    for line in String::from_utf8_lossy(&buf)
        .lines()
        .filter(|l| !l.trim().is_empty())
    {
        if let Some(frame) = extract_frame(line)
            && frame
                .get("agent_id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id == agent_id)
        {
            out.push(frame);
        }
    }
    Ok((out, pos + n as u64))
}

async fn read_watch_replay_for_attach(
    path: &Path,
    file: &mut tokio::fs::File,
    file_len: u64,
    agent_id: &str,
) -> anyhow::Result<(Vec<Value>, u64)> {
    if !path.exists() || file_len == 0 {
        return Ok((Vec::new(), 0));
    }

    let start = file_len.saturating_sub(WATCH_ATTACH_REPLAY_SCAN_BYTES);
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut buf = vec![0u8; (file_len - start) as usize];
    let n = file.read(&mut buf).await?;
    buf.truncate(n);

    let chunk = String::from_utf8_lossy(&buf);
    let mut lines = chunk.lines();
    if start > 0 {
        let _ = lines.next();
    }

    let mut replay: VecDeque<Value> = VecDeque::with_capacity(WATCH_ATTACH_REPLAY_MAX_FRAMES);
    for line in lines.filter(|l| !l.trim().is_empty()) {
        if let Some(frame) = extract_frame(line)
            && frame
                .get("agent_id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id == agent_id)
        {
            if replay.len() >= WATCH_ATTACH_REPLAY_MAX_FRAMES {
                let _ = replay.pop_front();
            }
            replay.push_back(frame);
        }
    }
    Ok((replay.into_iter().collect(), file_len))
}

fn extract_frame(line: &str) -> Option<Value> {
    let parsed: Value = serde_json::from_str(line).ok()?;
    if let Some(frame) = parsed.get("frame") {
        return Some(frame.clone());
    }
    Some(parsed)
}

fn print_frame(agent_id: &str, frame: Value, as_json: bool) -> anyhow::Result<()> {
    let env = to_attached_envelope(agent_id, &frame);
    if as_json {
        println!("{}", serde_json::to_string(&env)?);
        return Ok(());
    }

    if env.class == "input.atm_mail" {
        println!(
            "{} <{}>",
            format_mail_actor(&env.source_actor),
            clamp_three_lines(&env.text)
        );
        return Ok(());
    }

    let rendered = render_attached_text(&env);
    println!(
        "[{}][{}|{}] {rendered}",
        env.class, env.source_kind, env.applicability
    );
    Ok(())
}

fn to_attached_envelope(agent_id: &str, frame: &Value) -> AttachedRenderEnvelope {
    let source_kind = frame
        .pointer("/source/kind")
        .and_then(|v| v.as_str())
        .unwrap_or("client_prompt")
        .to_string();
    let source_actor = frame
        .pointer("/source/actor")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let source_channel = frame
        .pointer("/source/channel")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let event = frame.get("event").unwrap_or(frame);
    let event_type = event
        .pointer("/params/type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let text = extract_event_text(event).to_string();
    let (class, applicability) = classify_event_class(&event_type, &source_kind);
    let is_turn_boundary = matches!(
        event_type.as_str(),
        "turn_started"
            | "turn_completed"
            | "task_complete"
            | "done"
            | "turn_idle"
            | "idle"
            | "turn_interrupted"
            | "turn_cancelled"
            | "cancelled"
            | "interrupt"
            | "approval_prompt"
            | "approval_request"
            | "approval_rejected"
            | "approval_approved"
            | "entered_review_mode"
            | "exited_review_mode"
            | "item/enteredReviewMode"
            | "item/exitedReviewMode"
            | "stream_error"
            | "error"
    );

    AttachedRenderEnvelope {
        v: 1,
        mode: "attached",
        agent_id: agent_id.to_string(),
        class: class.to_string(),
        applicability: applicability.as_str().to_string(),
        source_kind,
        source_actor,
        source_channel,
        event_type,
        text,
        is_turn_boundary,
        raw: frame.clone(),
    }
}

fn classify_event_class(event_type: &str, source_kind: &str) -> (&'static str, EventApplicability) {
    if source_kind == "atm_mail" || source_kind == "atm_mcp" {
        return ("input.atm_mail", EventApplicability::Required);
    }
    if source_kind == "user_steer" || source_kind == "tui_user" {
        return ("input.user_steer", EventApplicability::Required);
    }

    let lower = event_type.to_ascii_lowercase();
    let event_type = lower.as_str();
    match event_type {
        "user_message" => ("input.client", EventApplicability::Required),
        "agent_message" | "agent_message_delta" | "agent_message_chunk" | "item_delta" => {
            ("assistant.output", EventApplicability::Required)
        }
        "reasoning_content_delta" | "agent_reasoning_delta" | "reasoning_content" => {
            ("assistant.reasoning", EventApplicability::Required)
        }
        "approval_prompt"
        | "approval_request"
        | "approval_rejected"
        | "approval_approved"
        | "entered_review_mode"
        | "exited_review_mode"
        | "item/enteredreviewmode"
        | "item/exitedreviewmode" => ("approval", EventApplicability::Required),
        "exec_command_started"
        | "exec_command_output_delta"
        | "exec_command_completed"
        | "exec_command_error"
        | "terminal_interaction" => ("tool.exec", EventApplicability::Required),
        "patch_apply_begin" | "patch_apply_end" | "turn_diff" | "file_change" => {
            ("file.edit", EventApplicability::Required)
        }
        "request_user_input" | "elicitation_request" => {
            ("elicitation.request", EventApplicability::Required)
        }
        "turn_started" | "turn_completed" | "task_started" | "task_complete" | "turn_idle"
        | "idle" | "done" | "item_started" | "item_completed" | "turn_interrupted"
        | "turn_cancelled" | "cancelled" | "interrupt" | "stream_error" | "error" => {
            ("turn.lifecycle", EventApplicability::Required)
        }
        "mcp_tool_call_begin"
        | "mcp_tool_call_end"
        | "web_search_begin"
        | "web_search_end"
        | "dynamic_tool_call_request"
        | "view_image_tool_call"
        | "tool_call"
        | "tool_result" => ("tool.lifecycle", EventApplicability::Degraded),
        "session_configured"
        | "thread_name_updated"
        | "token_count"
        | "model_reroute"
        | "context_compacted"
        | "thread_rolled_back"
        | "undo_started"
        | "undo_completed"
        | "background_event"
        | "warning"
        | "deprecation_notice"
        | "plan_update"
        | "plan_delta" => ("session.meta", EventApplicability::Degraded),
        "mcp_list_tools_response" | "remote_skill_downloaded" | "skills_update_available" => {
            ("unsupported.skills", EventApplicability::OutOfScope)
        }
        other if other.starts_with("list_") => ("unsupported.list", EventApplicability::OutOfScope),
        other if other.starts_with("realtime_conversation_") => {
            ("unsupported.realtime", EventApplicability::OutOfScope)
        }
        other if other.starts_with("collab_") => {
            ("unsupported.collab", EventApplicability::OutOfScope)
        }
        _ => ("unknown", EventApplicability::Required),
    }
}

fn extract_event_text(event: &Value) -> &str {
    event
        .pointer("/params/delta")
        .and_then(|v| v.as_str())
        .or_else(|| event.pointer("/params/text").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/output").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/message").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/prompt").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/diff").and_then(|v| v.as_str()))
        .unwrap_or("")
}

fn render_attached_text(env: &AttachedRenderEnvelope) -> String {
    match env.class.as_str() {
        "elicitation.request" => {
            if env.text.is_empty() {
                "input requested".to_string()
            } else {
                format!("? {}", env.text)
            }
        }
        "tool.lifecycle" => {
            let label = extract_tool_name(&env.raw).unwrap_or("tool");
            if env.text.is_empty() {
                format!("{label} {}", env.event_type)
            } else {
                format!("{label} {}: {}", env.event_type, env.text)
            }
        }
        "file.edit" => render_diff_text(&env.text, &env.event_type),
        _ => {
            if env.class == "unknown" {
                format!("unknown.{}", env.event_type)
            } else if env.text.is_empty() {
                env.event_type.clone()
            } else {
                env.text.clone()
            }
        }
    }
}

fn extract_tool_name(frame: &Value) -> Option<&str> {
    let event = frame.get("event").unwrap_or(frame);
    event
        .pointer("/params/tool_name")
        .and_then(|v| v.as_str())
        .or_else(|| event.pointer("/params/toolName").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/name").and_then(|v| v.as_str()))
}

fn render_diff_text(text: &str, fallback: &str) -> String {
    if text.trim().is_empty() {
        return fallback.to_string();
    }
    text.lines()
        .map(|line| {
            if line.starts_with('+') {
                format!("\u{1b}[32m{line}\u{1b}[0m")
            } else if line.starts_with('-') {
                format!("\u{1b}[31m{line}\u{1b}[0m")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

fn format_mail_actor(actor: &str) -> String {
    if actor.contains('@') {
        return actor.to_string();
    }
    let team = std::env::var("ATM_TEAM").unwrap_or_else(|_| "atm-dev".to_string());
    format!("{actor}@{}", team.trim())
}

fn clamp_three_lines(text: &str) -> String {
    let mut lines = text.lines();
    let l1 = lines.next().unwrap_or_default();
    let l2 = lines.next().unwrap_or_default();
    let l3 = lines.next().unwrap_or_default();
    let has_more = lines.next().is_some();

    let mut out: Vec<&str> = Vec::new();
    if !l1.is_empty() {
        out.push(l1);
    }
    if !l2.is_empty() {
        out.push(l2);
    }
    if !l3.is_empty() {
        out.push(l3);
    }
    let mut joined = out.join(" / ");
    if has_more {
        if !joined.is_empty() {
            joined.push_str(" ...");
        } else {
            joined = "...".to_string();
        }
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_text_routes_to_agent() {
        assert_eq!(
            parse_attach_input("hello"),
            AttachInput::AgentText("hello".to_string())
        );
    }

    #[test]
    fn parse_control_commands() {
        assert_eq!(
            parse_attach_input(":interrupt"),
            AttachInput::Control {
                verb: ControlVerb::Interrupt,
                arg: None
            }
        );
        assert_eq!(
            parse_attach_input(":approve ship it"),
            AttachInput::Control {
                verb: ControlVerb::Approve,
                arg: Some("ship it".to_string())
            }
        );
    }

    #[test]
    fn clamp_three_lines_applies_ellipsis() {
        let text = "a\nb\nc\nd";
        assert_eq!(clamp_three_lines(text), "a / b / c ...");
    }

    #[test]
    fn classify_atm_mail_has_priority() {
        assert_eq!(
            classify_event_class("agent_message_delta", "atm_mail").0,
            "input.atm_mail"
        );
    }

    #[test]
    fn attached_envelope_maps_event_fields() {
        let frame = serde_json::json!({
            "agent_id":"codex:abc",
            "source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},
            "event":{"params":{"type":"agent_message_delta","delta":"hello"}}
        });
        let env = to_attached_envelope("codex:abc", &frame);
        assert_eq!(env.mode, "attached");
        assert_eq!(env.class, "assistant.output");
        assert_eq!(env.applicability, "required");
        assert_eq!(env.text, "hello");
        assert_eq!(env.source_actor, "arch-atm");
    }

    #[test]
    fn classify_tool_lifecycle_degraded() {
        let (class, applicability) = classify_event_class("mcp_tool_call_begin", "client_prompt");
        assert_eq!(class, "tool.lifecycle");
        assert_eq!(applicability.as_str(), "degraded");
    }

    #[test]
    fn render_diff_text_applies_ansi_colors() {
        let rendered = render_diff_text("-old\n+new\n context", "turn_diff");
        assert!(rendered.contains("\u{1b}[31m-old\u{1b}[0m"));
        assert!(rendered.contains("\u{1b}[32m+new\u{1b}[0m"));
        assert!(rendered.contains(" context"));
    }

    #[test]
    fn format_mail_actor_adds_team_suffix() {
        let actor = format_mail_actor("arch-atm");
        assert!(actor.contains("arch-atm@"));
    }
}
