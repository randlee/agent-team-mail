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
//! | `Ctrl-I` | Send interrupt to agent |
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
mod config;
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
    daemon_client::{AgentSummary, query_list_agents, send_control},
    event_log::{EventFields, emit_event_best_effort},
    home::get_home_dir,
    logging,
};

use app::{App, MemberRow, PendingControl};
use config::{TuiConfig, load_tui_config};
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

    // Load user preferences before terminal setup so parse warnings go to stderr.
    let config = load_tui_config();

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

    let result = run_app(&mut terminal, team.clone(), config).await;

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
    config: TuiConfig,
) -> Result<()> {
    let mut app = App::new(team.clone(), config);

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
        if let Some(ref log_path) = app.session_log_path.clone() {
            match tail_log_file(log_path, app.stream_pos).await {
                Ok((new_lines, new_pos)) => {
                    if new_pos == 0 && app.stream_pos > 0 {
                        // Log was truncated — daemon likely restarted.
                        // Reset to start and show a freeze/reset indicator.
                        app.stream_pos = 0;
                        app.stream_lines.clear();
                        app.stream_source_error =
                            Some("stream reset: log truncated (daemon restart?)".to_string());
                    } else if new_pos > app.stream_pos {
                        // Successful read: clear any previous freeze indicator.
                        app.stream_source_error = None;

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
                    // else: new_pos == app.stream_pos — no new data, no change.
                }
                Err(_) => {
                    // File unreadable (permissions changed, filesystem error, etc.).
                    if app.stream_pos > 0 {
                        app.stream_source_error =
                            Some("stream frozen: log unreadable".to_string());
                    }
                }
            }
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
            let stdin_timeout = app.config.stdin_timeout_secs;
            let interrupt_timeout = app.config.interrupt_timeout_secs;
            let result =
                execute_control(&team, &app.streaming_agent, pending, stdin_timeout, interrupt_timeout)
                    .await;
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
/// # Truncation detection
///
/// When `file_len < pos` the file has been truncated (e.g., the daemon
/// restarted and cleared its log). In that case the function returns
/// `Ok((vec![], 0))` — a `new_pos` of `0` signals to the caller that the
/// stream position should be reset to the beginning of the file.
///
/// # No-op conditions
///
/// Returns `Ok((vec![], pos))` (unchanged position) when:
/// - The file does not exist.
/// - The file has not grown since `pos`.
async fn tail_log_file(path: &std::path::Path, pos: u64) -> Result<(Vec<String>, u64)> {
    use tokio::fs::File;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    if !path.exists() {
        return Ok((Vec::new(), pos));
    }

    let mut file = File::open(path).await?;
    let metadata = file.metadata().await?;
    let file_len = metadata.len();

    // Truncation: file shrank (daemon restart cleared the log).
    // Signal reset by returning new_pos=0.
    if file_len < pos {
        return Ok((Vec::new(), 0));
    }

    if file_len == pos {
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
/// retried once after `timeout_secs / 2` seconds with the same idempotency key.
///
/// `stdin_timeout_secs` controls the total retry budget for stdin actions;
/// `interrupt_timeout_secs` controls the budget for interrupt actions.
async fn execute_control(
    team: &str,
    streaming_agent: &Option<String>,
    action: PendingControl,
    stdin_timeout_secs: u64,
    interrupt_timeout_secs: u64,
) -> String {
    let Some(agent_id) = streaming_agent else {
        return "No agent selected".to_string();
    };

    let request_id = uuid::Uuid::new_v4().to_string();
    let sent_at = chrono::Utc::now().to_rfc3339();

    let (control_action, payload) = match &action {
        PendingControl::Stdin(text) => (ControlAction::Stdin, Some(text.clone())),
        PendingControl::Interrupt => (ControlAction::Interrupt, None),
    };

    // Select per-action timeout from config before control_action is moved.
    let timeout_secs = match &control_action {
        ControlAction::Stdin => stdin_timeout_secs,
        ControlAction::Interrupt => interrupt_timeout_secs,
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
        session_id: String::new(), // daemon resolves from agent_id
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
        agent_id: Some(agent_id.clone()),
        result: None,
        ..Default::default()
    });

    let ack = send_with_retry(&request, timeout_secs).await;

    let result_str = match &ack {
        Ok(a) => format_ack_result(a),
        Err(e) => format!("error: {e}"),
    };

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm-tui",
        action: "control_ack",
        team: Some(team.to_string()),
        agent_id: Some(agent_id.clone()),
        result: Some(result_str.clone()),
        ..Default::default()
    });

    result_str
}

/// Send a control request to the daemon, retrying once on [`ControlResult::Timeout`].
///
/// Uses [`tokio::task::spawn_blocking`] because [`send_control`] performs
/// blocking Unix socket I/O.
///
/// On a first-attempt timeout the function sleeps for `timeout_secs / 2`
/// seconds before issuing one retry with the same idempotency key. The
/// `timeout_secs` value comes from the per-action TUI config fields
/// (`stdin_timeout_secs` or `interrupt_timeout_secs`).
async fn send_with_retry(request: &ControlRequest, timeout_secs: u64) -> anyhow::Result<ControlAck> {
    let req1 = request.clone();
    let result = tokio::task::spawn_blocking(move || send_control(&req1)).await??;

    if result.result == ControlResult::Timeout {
        // Single retry after half the configured timeout budget.
        let delay = Duration::from_secs(timeout_secs / 2);
        tokio::time::sleep(delay).await;
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
        let result = execute_control("atm-dev", &None, PendingControl::Interrupt, 10, 5).await;
        assert_eq!(result, "No agent selected");
    }

    #[tokio::test]
    async fn test_execute_control_no_daemon_returns_error_string() {
        // With a selected agent but no daemon, the result is an error string.
        // We just assert it is non-empty and does not panic.
        let result = execute_control(
            "atm-dev",
            &Some("arch-ctm".to_string()),
            PendingControl::Stdin("hello".to_string()),
            10,
            5,
        )
        .await;
        assert!(!result.is_empty(), "result should be non-empty on daemon error");
    }

    // ── tail_log_file tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_tail_log_file_missing_returns_empty_unchanged_pos() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.log");
        let (lines, new_pos) = tail_log_file(&path, 0).await.unwrap();
        assert!(lines.is_empty());
        assert_eq!(new_pos, 0);
    }

    #[tokio::test]
    async fn test_tail_log_file_reads_new_data() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("agent.log");
        tokio::fs::write(&path, b"line one\nline two\n").await.unwrap();
        let (lines, new_pos) = tail_log_file(&path, 0).await.unwrap();
        assert!(!lines.is_empty());
        assert!(new_pos > 0);
    }

    #[tokio::test]
    async fn test_tail_log_file_no_new_data_unchanged_pos() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("agent.log");
        tokio::fs::write(&path, b"data").await.unwrap();
        let (_, first_pos) = tail_log_file(&path, 0).await.unwrap();
        let (lines2, new_pos2) = tail_log_file(&path, first_pos).await.unwrap();
        assert!(lines2.is_empty());
        assert_eq!(new_pos2, first_pos);
    }

    /// When the log file shrinks (truncated), `tail_log_file` returns new_pos=0
    /// to signal that the caller should reset the stream position.
    #[tokio::test]
    async fn test_tail_log_file_truncation_signals_reset() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("agent.log");

        // Write initial content and advance position.
        tokio::fs::write(&path, b"initial content that is quite long\n")
            .await
            .unwrap();
        let (_, pos_after_first) = tail_log_file(&path, 0).await.unwrap();
        assert!(pos_after_first > 0, "should have advanced position");

        // Truncate the file (simulates daemon restart clearing the log).
        tokio::fs::write(&path, b"new").await.unwrap();

        let (lines, new_pos) = tail_log_file(&path, pos_after_first).await.unwrap();
        assert_eq!(
            new_pos, 0,
            "new_pos should be 0 to signal truncation/reset"
        );
        assert!(
            lines.is_empty(),
            "no lines should be returned on truncation signal"
        );
    }

    /// Test that the stream source error freeze-then-clear cycle works at the
    /// `tail_log_file` level.
    ///
    /// Architecture §12 requires that after a truncation signal (`new_pos=0`)
    /// the caller resets `stream_pos` to 0 and clears `stream_source_error` on
    /// the next successful read.  This test validates that `tail_log_file`
    /// returns the correct signals at each stage so the caller can implement
    /// that cycle correctly.
    #[tokio::test]
    async fn test_stream_source_error_cleared_on_recovery() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("agent.log");

        // Step 1: file exists, first read succeeds and advances position.
        tokio::fs::write(&log_path, b"line one\nline two\n").await.unwrap();
        let (lines, pos1) = tail_log_file(&log_path, 0).await.unwrap();
        assert!(!lines.is_empty(), "should read initial content");
        assert!(pos1 > 0, "position must advance after first read");

        // Step 2: file is truncated to a smaller size (daemon restart scenario).
        // tail_log_file must return new_pos=0 to signal the caller to reset.
        tokio::fs::write(&log_path, b"x").await.unwrap(); // 1 byte < pos1
        let (trunc_lines, trunc_pos) = tail_log_file(&log_path, pos1).await.unwrap();
        assert_eq!(trunc_pos, 0, "truncation must return new_pos=0 (reset signal)");
        assert!(trunc_lines.is_empty(), "no lines on truncation signal");

        // Step 3: caller simulates clearing stream_source_error by resetting pos to 0.
        // File now has new content after the restart.
        tokio::fs::write(&log_path, b"new content after restart\n").await.unwrap();
        let (new_lines, new_pos) = tail_log_file(&log_path, 0).await.unwrap();
        assert!(
            !new_lines.is_empty(),
            "should read new content after recovery (stream_source_error cleared)"
        );
        assert!(new_pos > 0, "position must advance after recovery read");
    }
}
