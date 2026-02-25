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
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};
use tokio::time::interval;

const WATCH_ATTACH_REPLAY_MAX_FRAMES: usize = 50;
const WATCH_ATTACH_REPLAY_SCAN_BYTES: u64 = 512 * 1024;
const ATTACH_CHECKPOINT_VERSION: u8 = 1;
const UNSUPPORTED_WARN_THRESHOLD: u64 = 5;

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
    unsupported_count: Option<u64>,
    raw: Value,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct AttachReplayCheckpoint {
    v: u8,
    team: String,
    agent_id: String,
    pos: u64,
    updated_at: String,
}

static UNSUPPORTED_EVENT_COUNTS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

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

    let mut stream_pos: u64 = load_attach_checkpoint_pos(&team, &args.agent_id).unwrap_or(0);
    let mut ticker = interval(Duration::from_millis(poll_ms));
    let mut stdin_lines = BufReader::new(tokio::io::stdin()).lines();
    let mut pending_elicitation_id: Option<String> = None;

    // Initial attach replay (bounded).
    match tail_watch_stream_file(&watch_path, stream_pos, &args.agent_id).await {
        Ok((replay, new_pos, replay_truncated)) => {
            stream_pos = new_pos;
            if replay_truncated {
                print_replay_truncation_notice(&args.agent_id, args.json)?;
            }
            for frame in replay {
                pending_elicitation_id =
                    update_pending_elicitation_id(pending_elicitation_id, &frame);
                print_frame(&args.agent_id, frame, args.json)?;
            }
            let _ = save_attach_checkpoint_pos(&team, &args.agent_id, stream_pos);
        }
        Err(err) => print_stream_error("watch.tail.initial", &err, args.json)?,
    }

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                match tail_watch_stream_file(&watch_path, stream_pos, &args.agent_id).await {
                    Ok((frames, new_pos, _)) => {
                        stream_pos = new_pos;
                        for frame in frames {
                            pending_elicitation_id =
                                update_pending_elicitation_id(pending_elicitation_id, &frame);
                            print_frame(&args.agent_id, frame, args.json)?;
                        }
                        let _ = save_attach_checkpoint_pos(&team, &args.agent_id, stream_pos);
                    }
                    Err(err) => print_stream_error("watch.tail", &err, args.json)?,
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
                        match send_stdin_control(&team, &args.agent_id, &text) {
                            Ok(ack) => println!("ack {}", format_ack(&ack)),
                            Err(err) => {
                                print_stream_error(classify_control_send_error(&err), &err, args.json)?
                            }
                        }
                    }
                    AttachInput::Control { verb, arg } => {
                        match verb {
                            ControlVerb::Help => print_input_contract(),
                            ControlVerb::Interrupt => {
                                match send_interrupt_control(&team, &args.agent_id) {
                                    Ok(ack) => println!("ack {}", format_ack(&ack)),
                                    Err(err) => {
                                        print_stream_error(classify_control_send_error(&err), &err, args.json)?
                                    }
                                }
                            }
                            ControlVerb::Detach => break,
                            ControlVerb::Approve => {
                                let Some(elicitation_id) = pending_elicitation_id.clone() else {
                                    println!("ack no pending elicitation id to approve");
                                    continue;
                                };
                                match send_elicitation_response_control(
                                    &team,
                                    &args.agent_id,
                                    &elicitation_id,
                                    "approve",
                                    arg.as_deref(),
                                ) {
                                    Ok(ack) => println!("ack {}", format_ack(&ack)),
                                    Err(err) => {
                                        print_stream_error(classify_control_send_error(&err), &err, args.json)?
                                    }
                                }
                            }
                            ControlVerb::Reject => {
                                let Some(elicitation_id) = pending_elicitation_id.clone() else {
                                    println!("ack no pending elicitation id to reject");
                                    continue;
                                };
                                match send_elicitation_response_control(
                                    &team,
                                    &args.agent_id,
                                    &elicitation_id,
                                    "reject",
                                    arg.as_deref(),
                                ) {
                                    Ok(ack) => println!("ack {}", format_ack(&ack)),
                                    Err(err) => {
                                        print_stream_error(classify_control_send_error(&err), &err, args.json)?
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let _ = save_attach_checkpoint_pos(&team, &args.agent_id, stream_pos);
    print_unsupported_summary_on_detach(&args.agent_id, args.json)?;
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
    println!("  :approve [text] -> correlated elicitation approve");
    println!("  :reject [text]  -> correlated elicitation reject");
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
        elicitation_id: None,
        decision: None,
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
        elicitation_id: None,
        decision: None,
    };
    send_control(&req)
}

fn send_elicitation_response_control(
    team: &str,
    agent_id: &str,
    elicitation_id: &str,
    decision: &str,
    note: Option<&str>,
) -> anyhow::Result<ControlAck> {
    let req = ControlRequest {
        v: CONTROL_SCHEMA_VERSION,
        request_id: uuid::Uuid::new_v4().to_string(),
        msg_type: "control.elicitation.response".to_string(),
        signal: None,
        sent_at: chrono::Utc::now().to_rfc3339(),
        team: team.to_string(),
        session_id: String::new(),
        agent_id: agent_id.to_string(),
        sender: "attach_cli".to_string(),
        action: ControlAction::ElicitationResponse,
        payload: note.map(str::to_string),
        content_ref: None,
        elicitation_id: Some(elicitation_id.to_string()),
        decision: Some(decision.to_string()),
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

fn update_pending_elicitation_id(current: Option<String>, frame: &Value) -> Option<String> {
    let event = frame.get("event").unwrap_or(frame);
    let kind = event
        .pointer("/params/type")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    match kind {
        "exec_approval_request" | "approval_prompt" | "approval_request"
        | "apply_patch_approval_request" | "request_user_input" | "elicitation_request" => {
            extract_elicitation_id(event).or(current)
        }
        "approval_approved" | "approval_rejected" | "approval_resolved" => None,
        _ => current,
    }
}

fn extract_elicitation_id(event: &Value) -> Option<String> {
    event
        .pointer("/params/elicitation_id")
        .and_then(|v| v.as_str())
        .or_else(|| event.pointer("/params/approval_id").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/request_id").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/id").and_then(|v| v.as_str()))
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string)
}

fn watch_feed_path(agent_id: &str) -> Option<PathBuf> {
    let safe_id = safe_agent_id(agent_id);
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

fn safe_agent_id(agent_id: &str) -> Cow<'_, str> {
    if agent_id.contains('/') || agent_id.contains('\\') {
        Cow::Owned(agent_id.replace(['/', '\\'], "_"))
    } else {
        Cow::Borrowed(agent_id)
    }
}

fn attach_checkpoint_path(team: &str, agent_id: &str) -> Option<PathBuf> {
    let safe_id = safe_agent_id(agent_id);
    if let Ok(atm_home) = std::env::var("ATM_HOME") {
        let trimmed = atm_home.trim();
        if !trimmed.is_empty() {
            return Some(
                PathBuf::from(trimmed)
                    .join(".config/atm/agent-sessions")
                    .join(team)
                    .join(safe_id.as_ref())
                    .join("attach-checkpoint.json"),
            );
        }
    }
    let home = agent_team_mail_core::home::get_home_dir().ok()?;
    Some(
        home.join(".config/atm/agent-sessions")
            .join(team)
            .join(safe_id.as_ref())
            .join("attach-checkpoint.json"),
    )
}

fn load_attach_checkpoint_pos(team: &str, agent_id: &str) -> Option<u64> {
    let path = attach_checkpoint_path(team, agent_id)?;
    let raw = std::fs::read_to_string(path).ok()?;
    let checkpoint: AttachReplayCheckpoint = serde_json::from_str(&raw).ok()?;
    if checkpoint.team != team || checkpoint.agent_id != agent_id {
        return None;
    }
    Some(checkpoint.pos)
}

fn save_attach_checkpoint_pos(team: &str, agent_id: &str, pos: u64) -> anyhow::Result<()> {
    let path = attach_checkpoint_path(team, agent_id).ok_or_else(|| {
        anyhow::anyhow!("failed to resolve attach checkpoint path for team={team} agent={agent_id}")
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let checkpoint = AttachReplayCheckpoint {
        v: ATTACH_CHECKPOINT_VERSION,
        team: team.to_string(),
        agent_id: agent_id.to_string(),
        pos,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    std::fs::write(path, serde_json::to_string_pretty(&checkpoint)?)?;
    Ok(())
}

fn print_replay_truncation_notice(agent_id: &str, as_json: bool) -> anyhow::Result<()> {
    if as_json {
        let payload = serde_json::json!({
            "v": 1,
            "mode": "attached",
            "agent_id": agent_id,
            "class": "session.meta",
            "event_type": "replay_truncated",
            "text": "replay clipped to the most recent turn boundary; older events omitted"
        });
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }
    println!("note: replay clipped to the most recent turn boundary; older events omitted");
    Ok(())
}

async fn tail_watch_stream_file(
    path: &Path,
    pos: u64,
    agent_id: &str,
) -> anyhow::Result<(Vec<Value>, u64, bool)> {
    use tokio::fs::File;

    if !path.exists() {
        return Ok((Vec::new(), pos, false));
    }

    let mut file = File::open(path).await?;
    let file_len = file.metadata().await?.len();
    if pos == 0 || file_len < pos {
        return read_watch_replay_for_attach(path, &mut file, file_len, agent_id).await;
    }
    if file_len == pos {
        return Ok((Vec::new(), pos, false));
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
    Ok((out, pos + n as u64, false))
}

async fn read_watch_replay_for_attach(
    path: &Path,
    file: &mut tokio::fs::File,
    file_len: u64,
    agent_id: &str,
) -> anyhow::Result<(Vec<Value>, u64, bool)> {
    if !path.exists() || file_len == 0 {
        return Ok((Vec::new(), 0, false));
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

    let mut replay: Vec<Value> = Vec::new();
    for line in lines.filter(|l| !l.trim().is_empty()) {
        if let Some(frame) = extract_frame(line)
            && frame
                .get("agent_id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id == agent_id)
        {
            replay.push(frame);
        }
    }
    let (replay, truncated) = trim_replay_to_recent_turn_boundary(replay, WATCH_ATTACH_REPLAY_MAX_FRAMES);
    Ok((replay, file_len, truncated))
}

fn trim_replay_to_recent_turn_boundary(replay: Vec<Value>, max_frames: usize) -> (Vec<Value>, bool) {
    if replay.len() <= max_frames {
        return (replay, false);
    }
    let len = replay.len();
    let floor = len.saturating_sub(max_frames);
    let mut start = floor;
    while start < len && !is_turn_boundary_frame(&replay[start]) {
        start += 1;
    }
    if start >= len {
        start = floor;
    }
    (replay[start..].to_vec(), true)
}

fn is_turn_boundary_frame(frame: &Value) -> bool {
    let event = frame.get("event").unwrap_or(frame);
    let ty = event
        .pointer("/params/type")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    matches!(
        ty,
        "turn_started"
            | "turn_completed"
            | "task_complete"
            | "done"
            | "turn_aborted"
            | "turn_idle"
            | "idle"
            | "turn_interrupted"
            | "interrupt"
            | "turn_cancelled"
            | "cancelled"
    )
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

    let payload = if env.text.is_empty() {
        env.event_type.clone()
    } else {
        env.text.clone()
    };
    match env.class.as_str() {
        "input.atm_mail" => println!("{} <{}>", env.source_actor, clamp_three_lines(&env.text)),
        "input.client" => println!("client: {payload}"),
        "input.user_steer" => println!("steer: {payload}"),
        "assistant.output" => println!("assistant: {}", render_markdown_text(&payload)),
        "assistant.reasoning" => {
            if is_reasoning_section_break(&env.raw) {
                println!("reasoning: {}", format_reasoning_section_break(&payload));
            } else {
                println!("reasoning: {payload}");
            }
        }
        "turn.lifecycle" => println!("turn: {payload}"),
        "approval.exec" | "approval.patch" | "approval.review" => {
            println!("approval: {payload}")
        }
        "elicitation.request_user_input" => println!("user-input-request: {payload}"),
        "elicitation.request" => println!("input-request: {payload}"),
        "stream.warning" => println!("stream-warning: {payload}"),
        "stream.error.proxy" => println!("stream-error(proxy): {payload}"),
        "stream.error.child" => println!("stream-error(child): {payload}"),
        "stream.error.upstream" => println!("stream-error(upstream): {payload}"),
        "stream.error.fatal" => {
            println!("stream-error(fatal): {payload} [detach/reconnect recommended]")
        }
        "file.edit" => print_file_edit_lines(&payload),
        _ => println!("[{}][{}] {}", env.class, env.source_kind, payload),
    }
    Ok(())
}

fn is_reasoning_section_break(raw: &Value) -> bool {
    if raw
        .pointer("/event/params/section_break")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    if raw
        .pointer("/event/params/is_section_break")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    raw.pointer("/event/params/delta_type")
        .and_then(|v| v.as_str())
        .or_else(|| {
            raw.pointer("/event/params/reasoning_delta/type")
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            raw.pointer("/event/params/content/type")
                .and_then(|v| v.as_str())
        })
        .is_some_and(|v| v.eq_ignore_ascii_case("section_break"))
}

fn format_reasoning_section_break(payload: &str) -> String {
    if payload.trim().is_empty() {
        "----".to_string()
    } else {
        format!("---- {} ----", payload.trim())
    }
}

fn render_markdown_text(payload: &str) -> String {
    let trimmed = payload.trim();
    if trimmed.starts_with("```") {
        let lang = trimmed
            .trim_start_matches('`')
            .split_whitespace()
            .next()
            .unwrap_or_default();
        if lang.is_empty() {
            return "[code-block]".to_string();
        }
        return format!("[code-block:{lang}]");
    }
    if trimmed.starts_with('#') {
        return trimmed.to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("- ") {
        return format!("• {rest}");
    }
    if let Some(rest) = trimmed.strip_prefix("* ") {
        return format!("• {rest}");
    }
    payload.to_string()
}

fn print_file_edit_lines(payload: &str) {
    let normalized = payload.replace("\\n", "\n");
    let mut printed = false;
    for line in normalized.lines() {
        printed = true;
        if line.starts_with('+') && !line.starts_with("+++") {
            println!("file-edit: [+] {}", line.trim_start_matches('+').trim_start());
        } else if line.starts_with('-') && !line.starts_with("---") {
            println!("file-edit: [-] {}", line.trim_start_matches('-').trim_start());
        } else if line.starts_with("@@") {
            println!("file-edit: [@@] {}", line.trim_start_matches("@@").trim_start());
        } else {
            println!("file-edit: {line}");
        }
    }
    if !printed {
        println!("file-edit:");
    }
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
    let text = event
        .pointer("/params/delta")
        .and_then(|v| v.as_str())
        .or_else(|| event.pointer("/params/text").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/output").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/message").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    let (class, unsupported_count, applicability) =
        classify_event_class(&event_type, &source_kind, event, &text);
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
        class,
        applicability: applicability.to_string(),
        source_kind,
        source_actor,
        source_channel,
        event_type,
        text,
        is_turn_boundary,
        unsupported_count,
        raw: frame.clone(),
    }
}

fn classify_event_class(
    event_type: &str,
    source_kind: &str,
    event: &Value,
    text: &str,
) -> (String, Option<u64>, &'static str) {
    if source_kind == "atm_mail" || source_kind == "atm_mcp" {
        return ("input.atm_mail".to_string(), None, "required");
    }
    if source_kind == "user_steer" || source_kind == "tui_user" {
        return ("input.user_steer".to_string(), None, "required");
    }

    let class = match event_type {
        "user_message" => "input.client",
        "agent_message" | "agent_message_delta" | "agent_message_chunk" | "item_delta" => {
            "assistant.output"
        }
        "reasoning_content_delta" | "agent_reasoning_delta" | "reasoning_content" => {
            "assistant.reasoning"
        }
        "exec_approval_request" | "approval_prompt" | "approval_request" => "approval.exec",
        "apply_patch_approval_request" => "approval.patch",
        "approval_rejected"
        | "approval_approved"
        | "entered_review_mode"
        | "exited_review_mode"
        | "item/enteredReviewMode"
        | "item/exitedReviewMode" => "approval.review",
        "exec_command_begin"
        | "exec_command_started"
        | "exec_command_output_delta"
        | "exec_command_completed"
        | "exec_command_error" => "tool.exec",
        "mcp_tool_call_begin" | "mcp_tool_call_end" | "web_search_begin" | "web_search_end" => {
            "tool.lifecycle"
        }
        "patch_apply_begin" | "patch_apply_end" | "turn_diff" | "file_change" => "file.edit",
        "request_user_input" => "elicitation.request_user_input",
        "elicitation_request" => "elicitation.request",
        "session_configured"
        | "thread_name_updated"
        | "token_count"
        | "model_reroute"
        | "context_compacted"
        | "thread_rolled_back"
        | "undo_started"
        | "undo_completed" => "session.meta",
        "plan_update" | "plan_delta" => "plan.update",
        "turn_started" | "turn_completed" | "turn_aborted" | "task_started" | "task_complete"
        | "turn_idle" | "idle" | "done" | "item_started" | "item_completed" => "turn.lifecycle",
        "stream_warning" => "stream.warning",
        "stream_error" | "error" => {
            if is_fatal_stream_error(event, text) {
                "stream.error.fatal"
            } else {
                match stream_error_source(event) {
                    "child" => "stream.error.child",
                    "upstream" => "stream.error.upstream",
                    _ => "stream.error.proxy",
                }
            }
        }
        _ => {
            let ty = sanitize_event_type(event_type);
            let count = record_unsupported_event(&ty);
            return (format!("unsupported.{ty}"), Some(count), "out_of_scope");
        }
    };
    let applicability = match class {
        "tool.lifecycle" | "session.meta" | "plan.update" => "degraded",
        _ => "required",
    };
    (class.to_string(), None, applicability)
}

fn sanitize_event_type(event_type: &str) -> String {
    let raw = if event_type.trim().is_empty() {
        "unknown"
    } else {
        event_type.trim()
    };
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn stream_error_source(event: &Value) -> &'static str {
    let source = event
        .pointer("/params/error_source")
        .and_then(|v| v.as_str())
        .or_else(|| event.pointer("/params/errorSource").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/source").and_then(|v| v.as_str()))
        .unwrap_or("proxy");
    match source {
        "child" => "child",
        "upstream" | "upstream_mcp" => "upstream",
        _ => "proxy",
    }
}

fn is_fatal_stream_error(event: &Value, text: &str) -> bool {
    if event
        .pointer("/params/fatal")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    text.to_ascii_lowercase().contains("fatal")
}

fn record_unsupported_event(event_type: &str) -> u64 {
    let map = UNSUPPORTED_EVENT_COUNTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("unsupported event counter mutex");
    let entry = guard.entry(event_type.to_string()).or_insert(0);
    *entry += 1;
    *entry
}

fn unsupported_summary_lines(warn_threshold: u64) -> Vec<String> {
    let map = UNSUPPORTED_EVENT_COUNTS.get_or_init(|| Mutex::new(HashMap::new()));
    let guard = map.lock().expect("unsupported event counter mutex");
    let mut keys: Vec<&String> = guard.keys().collect();
    keys.sort();
    let mut out = Vec::new();
    for key in keys {
        let count = guard.get(key).copied().unwrap_or(0);
        out.push(format!("unsupported.summary {key}={count}"));
        if count >= warn_threshold {
            out.push(format!(
                "stream.warning unsupported event '{key}' seen {count} times"
            ));
        }
    }
    out
}

fn clear_unsupported_event_counts() {
    let map = UNSUPPORTED_EVENT_COUNTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("unsupported event counter mutex");
    guard.clear();
}

fn print_unsupported_summary_on_detach(agent_id: &str, as_json: bool) -> anyhow::Result<()> {
    let lines = unsupported_summary_lines(UNSUPPORTED_WARN_THRESHOLD);
    for line in &lines {
        if as_json {
            let class = if line.starts_with("stream.warning ") {
                "stream.warning"
            } else {
                "session.meta"
            };
            let payload = serde_json::json!({
                "v": 1,
                "mode": "attached",
                "agent_id": agent_id,
                "class": class,
                "event_type": "unsupported_summary",
                "text": line
            });
            println!("{}", serde_json::to_string(&payload)?);
        } else {
            println!("{line}");
        }
    }
    clear_unsupported_event_counts();
    Ok(())
}

#[cfg(test)]
fn unsupported_event_count(event_type: &str) -> u64 {
    let map = UNSUPPORTED_EVENT_COUNTS.get_or_init(|| Mutex::new(HashMap::new()));
    let guard = map.lock().expect("unsupported event counter mutex");
    guard.get(event_type).copied().unwrap_or(0)
}

fn print_stream_error(context: &str, err: &anyhow::Error, as_json: bool) -> anyhow::Result<()> {
    if as_json {
        let payload = serde_json::json!({
            "v": 1,
            "mode": "attached",
            "class": "stream.error",
            "context": context,
            "message": err.to_string()
        });
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }
    println!("[stream.error][{context}] {err}");
    Ok(())
}

fn classify_control_send_error(err: &anyhow::Error) -> &'static str {
    let kind = err
        .chain()
        .find_map(|e| e.downcast_ref::<std::io::Error>())
        .map(std::io::Error::kind);
    match kind {
        Some(ErrorKind::NotFound) => "control.not_found",
        Some(ErrorKind::ConnectionRefused) => "control.connection_refused",
        Some(ErrorKind::BrokenPipe) => "control.broken_pipe",
        Some(ErrorKind::TimedOut) => "control.timeout",
        Some(ErrorKind::PermissionDenied) => "control.permission_denied",
        Some(ErrorKind::WouldBlock) => "control.would_block",
        Some(_) => "control.io_error",
        None => "control.error",
    }
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
    use std::fs;
    use std::path::PathBuf;

    fn attach_fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/attach")
    }

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
    fn parse_additional_control_commands() {
        assert_eq!(
            parse_attach_input(":detach"),
            AttachInput::Control {
                verb: ControlVerb::Detach,
                arg: None
            }
        );
        assert_eq!(
            parse_attach_input(":reject no"),
            AttachInput::Control {
                verb: ControlVerb::Reject,
                arg: Some("no".to_string())
            }
        );
        assert_eq!(
            parse_attach_input(":help"),
            AttachInput::Control {
                verb: ControlVerb::Help,
                arg: None
            }
        );
        assert_eq!(parse_attach_input(":"), AttachInput::Ignore);
        assert_eq!(parse_attach_input("   "), AttachInput::Ignore);
        assert_eq!(
            parse_attach_input(":unknown"),
            AttachInput::Control {
                verb: ControlVerb::Help,
                arg: None
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
            classify_event_class("agent_message_delta", "atm_mail", &serde_json::json!({}), "").0,
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
        assert_eq!(env.unsupported_count, None);
    }

    #[test]
    fn classify_unknown_event_emits_supported_prefix_and_counter() {
        clear_unsupported_event_counts();
        let (class, count1, applicability1) =
            classify_event_class("future/event", "client_prompt", &serde_json::json!({}), "");
        let (_, count2, applicability2) =
            classify_event_class("future/event", "client_prompt", &serde_json::json!({}), "");
        assert_eq!(class, "unsupported.future_event");
        assert_eq!(applicability1, "out_of_scope");
        assert_eq!(applicability2, "out_of_scope");
        let c1 = count1.expect("first unsupported count present");
        let c2 = count2.expect("second unsupported count present");
        assert!(c1 >= 1);
        assert!(c2 >= c1);
        assert!(unsupported_event_count("future_event") >= c2);
        clear_unsupported_event_counts();
    }

    #[test]
    fn unsupported_summary_below_threshold_has_no_warning_line() {
        clear_unsupported_event_counts();
        for _ in 0..(UNSUPPORTED_WARN_THRESHOLD - 1) {
            let _ = record_unsupported_event("future_event");
        }
        let lines = unsupported_summary_lines(UNSUPPORTED_WARN_THRESHOLD);
        assert!(lines.iter().any(|l| l == "unsupported.summary future_event=4"));
        assert!(
            !lines.iter().any(|l| l.starts_with("stream.warning ")),
            "below-threshold counters must not emit stream.warning summary"
        );
        clear_unsupported_event_counts();
    }

    #[test]
    fn classify_stream_error_source_and_fatal_variants() {
        let child = serde_json::json!({"params":{"error_source":"child","message":"oops"}});
        let upstream = serde_json::json!({"params":{"errorSource":"upstream_mcp","message":"oops"}});
        let fatal = serde_json::json!({"params":{"fatal":true,"error_source":"proxy","message":"boom"}});

        assert_eq!(
            classify_event_class("stream_error", "client_prompt", &child, "oops").0,
            "stream.error.child"
        );
        assert_eq!(
            classify_event_class("stream_error", "client_prompt", &upstream, "oops").0,
            "stream.error.upstream"
        );
        assert_eq!(
            classify_event_class("stream_error", "client_prompt", &fatal, "boom").0,
            "stream.error.fatal"
        );
    }

    #[test]
    fn classify_splits_request_user_input_and_elicitation_request() {
        assert_eq!(
            classify_event_class(
                "request_user_input",
                "client_prompt",
                &serde_json::json!({}),
                "choose"
            )
            .0,
            "elicitation.request_user_input"
        );
        assert_eq!(
            classify_event_class(
                "elicitation_request",
                "client_prompt",
                &serde_json::json!({}),
                "approve?"
            )
            .0,
            "elicitation.request"
        );
    }

    #[test]
    fn markdown_render_hints_code_block_and_bullet() {
        assert_eq!(render_markdown_text("```rust"), "[code-block:rust]");
        assert_eq!(render_markdown_text("- item"), "• item");
        assert_eq!(render_markdown_text("* item"), "• item");
        assert_eq!(render_markdown_text("# heading"), "# heading");
    }

    #[test]
    fn reasoning_section_break_detected_from_delta_type() {
        let frame = serde_json::json!({
            "event":{"params":{"type":"reasoning_content_delta","delta_type":"section_break"}}
        });
        assert!(is_reasoning_section_break(&frame));
        assert_eq!(format_reasoning_section_break("plan"), "---- plan ----");
    }

    #[test]
    fn class_map_fixture_matches_expected_class_and_applicability() {
        let input_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/parity/attach/class-map.input.jsonl");
        let expected_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/parity/attach/class-map.expected.jsonl");

        let input = fs::read_to_string(input_path).expect("input fixture");
        let expected = fs::read_to_string(expected_path).expect("expected fixture");
        let input_rows: Vec<&str> = input.lines().filter(|l| !l.trim().is_empty()).collect();
        let expected_rows: Vec<&str> = expected.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            input_rows.len(),
            expected_rows.len(),
            "fixture row count must match"
        );

        for (idx, (frame_line, expected_line)) in
            input_rows.iter().zip(expected_rows.iter()).enumerate()
        {
            let frame: Value = serde_json::from_str(frame_line).expect("valid frame fixture line");
            let expected_json: Value =
                serde_json::from_str(expected_line).expect("valid expected fixture line");
            let env = to_attached_envelope("codex:test", &frame);
            let expected_class = expected_json
                .get("class")
                .and_then(|v| v.as_str())
                .expect("expected class");
            let expected_applicability = expected_json
                .get("applicability")
                .and_then(|v| v.as_str())
                .expect("expected applicability");
            assert_eq!(
                env.class,
                expected_class,
                "class mismatch at row {}",
                idx + 1
            );
            assert_eq!(
                env.applicability,
                expected_applicability,
                "applicability mismatch at row {}",
                idx + 1
            );
        }
    }

    #[test]
    fn classify_control_error_uses_io_kind() {
        let err = anyhow::Error::new(std::io::Error::new(ErrorKind::ConnectionRefused, "refused"));
        assert_eq!(
            classify_control_send_error(&err),
            "control.connection_refused"
        );
    }

    #[test]
    fn pending_elicitation_id_tracks_and_clears() {
        let approval = serde_json::json!({
            "event":{"params":{"type":"approval_request","elicitation_id":"eli-123"}}
        });
        let resolved = serde_json::json!({
            "event":{"params":{"type":"approval_approved"}}
        });
        let pending = update_pending_elicitation_id(None, &approval);
        assert_eq!(pending.as_deref(), Some("eli-123"));
        let cleared = update_pending_elicitation_id(pending, &resolved);
        assert_eq!(cleared, None);
    }

    #[tokio::test]
    async fn replay_tail_reads_fixture_jsonl() {
        let fixture = attach_fixture_dir().join("replay.sample.jsonl");
        let raw = fs::read_to_string(&fixture).expect("fixture exists");
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let feed_path = temp_dir.path().join("feed.jsonl");
        fs::write(&feed_path, raw).expect("write feed");

        let (frames, pos, truncated) = tail_watch_stream_file(&feed_path, 0, "codex:test")
            .await
            .expect("tail succeeds");
        assert_eq!(frames.len(), 2);
        assert!(pos > 0);
        assert!(!truncated);
        assert_eq!(
            frames[0]
                .pointer("/event/params/type")
                .and_then(|v| v.as_str()),
            Some("turn_started")
        );
        assert_eq!(
            frames[1]
                .pointer("/event/params/type")
                .and_then(|v| v.as_str()),
            Some("item_delta")
        );
    }

    #[test]
    fn trim_replay_prefers_recent_turn_boundary_when_clipped() {
        let make = |t: &str| serde_json::json!({"event":{"params":{"type": t}}});
        let replay = vec![
            make("item_delta"),
            make("item_delta"),
            make("turn_started"),
            make("item_delta"),
            make("item_delta"),
            make("turn_completed"),
            make("item_delta"),
        ];
        let (trimmed, truncated) = trim_replay_to_recent_turn_boundary(replay, 4);
        assert!(truncated);
        assert_eq!(trimmed.len(), 2);
        assert_eq!(
            trimmed[0].pointer("/event/params/type").and_then(|v| v.as_str()),
            Some("turn_completed")
        );
    }

    #[test]
    #[serial_test::serial]
    fn checkpoint_round_trip_uses_atm_home() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let old_home = std::env::var("ATM_HOME").ok();
        // SAFETY: test-scoped env mutation under serial test execution.
        unsafe {
            std::env::set_var("ATM_HOME", temp_dir.path());
        }
        save_attach_checkpoint_pos("atm-dev", "codex:test", 42).expect("save checkpoint");
        let loaded = load_attach_checkpoint_pos("atm-dev", "codex:test");
        assert_eq!(loaded, Some(42));
        if let Some(home) = old_home {
            // SAFETY: test-scoped env mutation under serial test execution.
            unsafe {
                std::env::set_var("ATM_HOME", home);
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial]
    async fn send_stdin_control_with_mock_daemon_ack() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let old_home = std::env::var("ATM_HOME").ok();
        // SAFETY: test-scoped env mutation under serial test execution.
        unsafe {
            std::env::set_var("ATM_HOME", temp_dir.path());
        }

        let daemon_dir = temp_dir.path().join(".claude/daemon");
        fs::create_dir_all(&daemon_dir).expect("daemon dir");
        let socket_path = daemon_dir.join("atm-daemon.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind unix socket");

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut req_line = String::new();
            {
                let mut reader = BufReader::new(&stream);
                reader.read_line(&mut req_line).expect("read request");
            }
            let req: serde_json::Value = serde_json::from_str(req_line.trim()).expect("json req");
            assert_eq!(req.get("command").and_then(|v| v.as_str()), Some("control"));
            let socket_request_id = req
                .get("request_id")
                .and_then(|v| v.as_str())
                .unwrap_or("sock-test");
            let response = serde_json::json!({
                "version": 1,
                "request_id": socket_request_id,
                "status": "ok",
                "payload": {
                    "request_id": "req-attach-test",
                    "result": "ok",
                    "duplicate": false,
                    "detail": "mock-daemon-ack",
                    "acked_at": "2026-02-24T00:00:00Z"
                }
            });
            let mut writer = std::io::BufWriter::new(&stream);
            writer
                .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .expect("write response");
            writer.write_all(b"\n").expect("newline");
            writer.flush().expect("flush");
        });

        let ack = send_stdin_control("atm-dev", "codex:test", "hello").expect("control ack");
        assert_eq!(ack.result, agent_team_mail_core::control::ControlResult::Ok);
        assert_eq!(ack.detail.as_deref(), Some("mock-daemon-ack"));

        server.join().expect("server join");
        if let Some(home) = old_home {
            // SAFETY: test-scoped env mutation under serial test execution.
            unsafe {
                std::env::set_var("ATM_HOME", home);
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial]
    async fn send_elicitation_response_control_with_mock_daemon_ack() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let old_home = std::env::var("ATM_HOME").ok();
        // SAFETY: test-scoped env mutation under serial test execution.
        unsafe {
            std::env::set_var("ATM_HOME", temp_dir.path());
        }

        let daemon_dir = temp_dir.path().join(".claude/daemon");
        fs::create_dir_all(&daemon_dir).expect("daemon dir");
        let socket_path = daemon_dir.join("atm-daemon.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind unix socket");

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut req_line = String::new();
            {
                let mut reader = BufReader::new(&stream);
                reader.read_line(&mut req_line).expect("read request");
            }
            let req: serde_json::Value = serde_json::from_str(req_line.trim()).expect("json req");
            let payload = req.get("payload").expect("payload");
            assert_eq!(
                payload.get("action").and_then(|v| v.as_str()),
                Some("elicitation_response")
            );
            assert_eq!(
                payload.get("elicitation_id").and_then(|v| v.as_str()),
                Some("eli-123")
            );
            assert_eq!(
                payload.get("decision").and_then(|v| v.as_str()),
                Some("approve")
            );

            let socket_request_id = req
                .get("request_id")
                .and_then(|v| v.as_str())
                .unwrap_or("sock-test");
            let response = serde_json::json!({
                "version": 1,
                "request_id": socket_request_id,
                "status": "ok",
                "payload": {
                    "request_id": "req-attach-elicitation-test",
                    "result": "ok",
                    "duplicate": false,
                    "detail": "mock-daemon-ack",
                    "acked_at": "2026-02-25T00:00:00Z"
                }
            });
            let mut writer = std::io::BufWriter::new(&stream);
            writer
                .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .expect("write response");
            writer.write_all(b"\n").expect("newline");
            writer.flush().expect("flush");
        });

        let ack = send_elicitation_response_control(
            "atm-dev",
            "codex:test",
            "eli-123",
            "approve",
            Some("looks good"),
        )
        .expect("control ack");
        assert_eq!(ack.result, agent_team_mail_core::control::ControlResult::Ok);
        assert_eq!(ack.detail.as_deref(), Some("mock-daemon-ack"));

        server.join().expect("server join");
        if let Some(home) = old_home {
            // SAFETY: test-scoped env mutation under serial test execution.
            unsafe {
                std::env::set_var("ATM_HOME", home);
            }
        }
    }
}
