//! ATM TUI — terminal user interface for monitoring agent teams.
//!
//! # Usage
//!
//! ```text
//! atm-tui --team atm-dev
//! ```
//!
//! # Key bindings
//!
//! | Key | Action |
//! |-----|--------|
//! | `q` / `Ctrl-C` | Quit |
//! | `↑` / `↓` | Select agent |
//! | `Tab` | Switch panel focus |
//! | _printable_ (Agent Terminal, live agent) | Append to stdin input |
//! | `Enter` | Send stdin text to agent |
//! | `Ctrl-K` | Send interrupt to agent |
//! | `Esc` | Clear current input |
//! | `Backspace` | Delete last character |
//!
//! # Architecture
//!
//! The main loop drives a 100 ms ticker. On every tick it:
//! 1. Refreshes the agent list and inbox counts from the daemon / filesystem
//!    (rate-limited to once every 2 s to avoid socket spam).
//! 2. Appends new bytes from the selected agent's session log (full 100 ms rate).
//! 3. Dispatches any pending control action (stdin inject / interrupt).
//! 4. Redraws the terminal frame.
//!
//! All input events are handled between ticks with a non-blocking poll.

mod agent_terminal;
mod app;
mod dashboard;
mod events;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::time::interval;

use agent_team_mail_core::{
    control::{ControlAck, ControlAction, ControlRequest, ControlResult, CONTROL_SCHEMA_VERSION},
    daemon_client::{AgentSummary, query_list_agents, query_session, send_control},
    event_log::{EventFields, emit_event_best_effort},
    home::get_home_dir,
    logging,
};

use app::{App, MemberRow, PendingControl};
use dashboard::{get_inbox_count, session_log_path};

// ── CLI ───────────────────────────────────────────────────────────────────────

/// ATM TUI — live dashboard and agent stream viewer.
#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Cli {
    /// Team name to monitor (e.g. `atm-dev`).
    #[arg(short, long)]
    pub team: String,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    logging::init();

    let cli = Cli::parse();
    let team = cli.team.clone();

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm-tui",
        action: "tui_start",
        team: Some(team.clone()),
        ..Default::default()
    });

    // Set up terminal
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    let result = run_app(&mut terminal, team.clone()).await;

    // Restore terminal on exit (even on error)
    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    terminal.show_cursor().ok();

    if let Err(ref e) = result {
        eprintln!("atm-tui error: {e:#}");
    }

    result
}

// ── Application loop ──────────────────────────────────────────────────────────

/// Run the TUI until the user quits.
///
/// # Errors
///
/// Returns an error on unrecoverable terminal I/O failures.
async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    team: String,
) -> Result<()> {
    let mut app = App::new(team.clone());

    // Rate-limit daemon/inbox queries to 2-second intervals.
    const DAEMON_REFRESH: Duration = Duration::from_secs(2);
    let mut last_daemon_refresh = Instant::now() - DAEMON_REFRESH; // trigger immediately

    // Resolve ATM home once for inbox reads.
    let home: PathBuf = get_home_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Resolve home for inbox reads (used in the loop closure below)
    let mut tick = interval(Duration::from_millis(100));

    loop {
        // ── Draw ──────────────────────────────────────────────────────────────
        terminal.draw(|f| ui::draw(f, &app))?;

        // ── Daemon / inbox refresh (rate-limited) ─────────────────────────────
        if last_daemon_refresh.elapsed() >= DAEMON_REFRESH {
            last_daemon_refresh = Instant::now();

            let agent_list = refresh_agent_list();
            let members = build_member_rows(&agent_list, &home, &team);

            // Detect if the currently streaming agent has changed identity.
            if let Some(ref name) = app.streaming_agent.clone()
                && !members.iter().any(|m| &m.agent == name)
            {
                emit_stream_detach_event(&team, name);
                app.reset_stream();
                app.streaming_agent = None;
            }

            app.agent_list = agent_list;
            app.members = members;

            // Clamp selected_index within bounds after list refresh.
            if !app.members.is_empty() && app.selected_index >= app.members.len() {
                app.selected_index = app.members.len() - 1;
            }

            // Resolve streaming agent from selection.
            if let Some(agent_name) = app.selected_agent().map(str::to_owned)
                && app.streaming_agent.as_deref() != Some(&agent_name)
            {
                // Switching to a new agent.
                if let Some(ref prev) = app.streaming_agent.clone() {
                    emit_stream_detach_event(&team, prev);
                }
                app.reset_stream();
                app.streaming_agent = Some(agent_name.clone());
                app.session_log_path = Some(session_log_path(&team, &agent_name));

                emit_event_best_effort(EventFields {
                    level: "info",
                    source: "atm-tui",
                    action: "session_connect",
                    team: Some(team.clone()),
                    agent_id: Some(agent_name.clone()),
                    ..Default::default()
                });
            }
        }

        // ── Session log tail (100 ms) ─────────────────────────────────────────
        if let Some(ref log_path) = app.session_log_path.clone()
            && let Ok((new_lines, new_pos)) = tail_log_file(log_path, app.stream_pos).await
            && new_pos > app.stream_pos
        {
            // Emit stream_attach on first successful read.
            if app.stream_pos == 0 && !new_lines.is_empty() {
                emit_event_best_effort(EventFields {
                    level: "info",
                    source: "atm-tui",
                    action: "stream_attach",
                    team: Some(team.clone()),
                    agent_id: app.streaming_agent.clone(),
                    result: Some("ok".to_string()),
                    ..Default::default()
                });
            }
            app.stream_pos = new_pos;
            app.append_stream_lines(new_lines);
        }

        // ── Input event handling ──────────────────────────────────────────────
        if event::poll(Duration::from_millis(0))? {
            let ev = event::read()?;
            if events::handle_event(&ev, &mut app) || app.should_quit {
                break;
            }
        }

        // ── Control action dispatch ───────────────────────────────────────────
        if let Some(pending) = app.pending_control.take() {
            let result =
                execute_control(&team, app.selected_agent().map(str::to_owned), pending).await;
            app.status_message = Some(result);
        }

        // ── Tick ──────────────────────────────────────────────────────────────
        tick.tick().await;
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Query the daemon for the live agent list. Returns an empty vec on failure.
fn refresh_agent_list() -> Vec<AgentSummary> {
    match query_list_agents() {
        Ok(Some(list)) => list,
        _ => Vec::new(),
    }
}

/// Build [`MemberRow`] entries from the agent list with current inbox counts.
fn build_member_rows(agents: &[AgentSummary], home: &std::path::Path, team: &str) -> Vec<MemberRow> {
    agents
        .iter()
        .map(|a| MemberRow {
            agent: a.agent.clone(),
            state: a.state.clone(),
            inbox_count: get_inbox_count(home, team, &a.agent),
        })
        .collect()
}

/// Read new bytes from a log file since `pos`, returning new lines and the
/// updated byte position.
///
/// Returns `Ok((vec![], pos))` when the file does not exist or has no new data.
async fn tail_log_file(path: &std::path::Path, pos: u64) -> Result<(Vec<String>, u64)> {
    use tokio::fs::File;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    if !path.exists() {
        return Ok((Vec::new(), pos));
    }

    let mut file = File::open(path).await?;
    let metadata = file.metadata().await?;
    let file_len = metadata.len();

    if file_len <= pos {
        return Ok((Vec::new(), pos));
    }

    file.seek(std::io::SeekFrom::Start(pos)).await?;

    let read_len = (file_len - pos).min(256 * 1024) as usize; // cap at 256 KiB per tick
    let mut buf = vec![0u8; read_len];
    let n = file.read(&mut buf).await?;
    buf.truncate(n);

    let chunk = String::from_utf8_lossy(&buf);
    let lines: Vec<String> = chunk
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();

    Ok((lines, pos + n as u64))
}

fn emit_stream_detach_event(team: &str, agent: &str) {
    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm-tui",
        action: "stream_detach",
        team: Some(team.to_string()),
        agent_id: Some(agent.to_string()),
        ..Default::default()
    });
}

// ── Control dispatch ──────────────────────────────────────────────────────────

/// Build and dispatch a control request, returning a human-readable result string.
///
/// If no agent is selected, returns an error message without touching the
/// daemon.  On a first-attempt [`ControlResult::Timeout`] the request is
/// retried once after a 2-second delay with the same idempotency key.
async fn execute_control(
    team: &str,
    selected_agent: Option<String>,
    action: PendingControl,
) -> String {
    let Some(agent_id) = selected_agent else {
        return "No agent selected".to_string();
    };
    let agent_for_lookup = agent_id.clone();
    let session_info = match tokio::task::spawn_blocking(move || query_session(&agent_for_lookup)).await {
        Ok(Ok(Some(info))) => info,
        Ok(Ok(None)) => return "not live: no session".to_string(),
        Ok(Err(e)) => return format!("error: failed to query session: {e}"),
        Err(e) => return format!("error: failed to query session: {e}"),
    };
    if !session_info.alive {
        return "not live: session process not alive".to_string();
    }

    let request_id = uuid::Uuid::new_v4().to_string();
    let sent_at = chrono::Utc::now().to_rfc3339();

    let (control_action, payload) = match &action {
        PendingControl::Stdin(text) => (ControlAction::Stdin, Some(text.clone())),
        PendingControl::Interrupt => (ControlAction::Interrupt, None),
    };

    let msg_type = match &control_action {
        ControlAction::Stdin => "control.stdin.request".to_string(),
        ControlAction::Interrupt => "control.interrupt.request".to_string(),
    };
    let signal = match &control_action {
        ControlAction::Interrupt => Some("interrupt".to_string()),
        ControlAction::Stdin => None,
    };

    let request = ControlRequest {
        v: CONTROL_SCHEMA_VERSION,
        request_id,
        msg_type,
        signal,
        sent_at,
        team: team.to_string(),
        session_id: session_info.session_id,
        agent_id: agent_id.clone(),
        sender: "tui".to_string(),
        action: control_action,
        payload,
        content_ref: None,
    };

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm-tui",
        action: "control_send",
        team: Some(team.to_string()),
        session_id: Some(request.session_id.clone()),
        agent_id: Some(agent_id.clone()),
        request_id: Some(request.request_id.clone()),
        result: None,
        ..Default::default()
    });

    let ack = send_with_retry(&request).await;

    let result_str = match &ack {
        Ok(a) => format_ack_result(a),
        Err(e) => format!("error: {e}"),
    };

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm-tui",
        action: "control_ack",
        team: Some(team.to_string()),
        session_id: Some(request.session_id.clone()),
        agent_id: Some(agent_id.clone()),
        request_id: Some(request.request_id.clone()),
        result: Some(result_str.clone()),
        ..Default::default()
    });

    result_str
}

/// Send a control request to the daemon, retrying once on [`ControlResult::Timeout`].
///
/// Uses [`tokio::task::spawn_blocking`] because [`send_control`] performs
/// blocking Unix socket I/O.
async fn send_with_retry(request: &ControlRequest) -> anyhow::Result<ControlAck> {
    let req1 = request.clone();
    let result = tokio::task::spawn_blocking(move || send_control(&req1)).await??;

    if result.result == ControlResult::Timeout {
        // Single retry after 2 s with the same idempotency key.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let req2 = request.clone();
        return tokio::task::spawn_blocking(move || send_control(&req2)).await?;
    }

    Ok(result)
}

/// Format a [`ControlAck`] result as a short human-readable string for the status bar.
fn format_ack_result(ack: &ControlAck) -> String {
    match ack.result {
        ControlResult::Ok if ack.duplicate => "already delivered".to_string(),
        ControlResult::Ok => "ok".to_string(),
        ControlResult::NotLive => "not live".to_string(),
        ControlResult::NotFound => "not found".to_string(),
        ControlResult::Busy => "busy".to_string(),
        ControlResult::Timeout => "timeout".to_string(),
        ControlResult::Rejected => {
            let detail = ack.detail.as_deref().unwrap_or("no detail");
            format!("rejected: {detail}")
        }
        ControlResult::InternalError => "internal error".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_core::control::{ControlAck, ControlResult};

    fn make_ack(result: ControlResult, duplicate: bool, detail: Option<&str>) -> ControlAck {
        ControlAck {
            request_id: "req-1".to_string(),
            result,
            duplicate,
            detail: detail.map(str::to_string),
            acked_at: "2026-02-21T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_format_ack_ok() {
        let ack = make_ack(ControlResult::Ok, false, None);
        assert_eq!(format_ack_result(&ack), "ok");
    }

    #[test]
    fn test_format_ack_duplicate() {
        let ack = make_ack(ControlResult::Ok, true, None);
        assert_eq!(format_ack_result(&ack), "already delivered");
    }

    #[test]
    fn test_format_ack_not_live() {
        let ack = make_ack(ControlResult::NotLive, false, None);
        assert_eq!(format_ack_result(&ack), "not live");
    }

    #[test]
    fn test_format_ack_not_found() {
        let ack = make_ack(ControlResult::NotFound, false, None);
        assert_eq!(format_ack_result(&ack), "not found");
    }

    #[test]
    fn test_format_ack_busy() {
        let ack = make_ack(ControlResult::Busy, false, None);
        assert_eq!(format_ack_result(&ack), "busy");
    }

    #[test]
    fn test_format_ack_timeout() {
        let ack = make_ack(ControlResult::Timeout, false, None);
        assert_eq!(format_ack_result(&ack), "timeout");
    }

    #[test]
    fn test_format_ack_rejected_with_detail() {
        let ack = make_ack(ControlResult::Rejected, false, Some("rate limited"));
        assert_eq!(format_ack_result(&ack), "rejected: rate limited");
    }

    #[test]
    fn test_format_ack_rejected_without_detail() {
        let ack = make_ack(ControlResult::Rejected, false, None);
        assert_eq!(format_ack_result(&ack), "rejected: no detail");
    }

    #[test]
    fn test_format_ack_internal_error() {
        let ack = make_ack(ControlResult::InternalError, false, None);
        assert_eq!(format_ack_result(&ack), "internal error");
    }

    #[tokio::test]
    async fn test_execute_control_no_agent_returns_message() {
        // When streaming_agent is None, execute_control returns a "no agent" message.
        let result = execute_control("atm-dev", None, PendingControl::Interrupt).await;
        assert_eq!(result, "No agent selected");
    }

    #[tokio::test]
    async fn test_execute_control_no_daemon_returns_error_string() {
        // With a selected agent but no daemon, the result is an error string.
        // We just assert it is non-empty and does not panic.
        let result = execute_control(
            "atm-dev",
            Some("arch-ctm".to_string()),
            PendingControl::Stdin("hello".to_string()),
        )
        .await;
        assert!(!result.is_empty(), "result should be non-empty on daemon error");
    }
}
