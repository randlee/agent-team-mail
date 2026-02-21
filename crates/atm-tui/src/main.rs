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
//!
//! # Architecture
//!
//! The main loop drives a 100 ms ticker. On every tick it:
//! 1. Refreshes the agent list and inbox counts from the daemon / filesystem
//!    (rate-limited to once every 2 s to avoid socket spam).
//! 2. Appends new bytes from the selected agent's session log (full 100 ms rate).
//! 3. Redraws the terminal frame.
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
    daemon_client::{AgentSummary, query_list_agents},
    event_log::{EventFields, emit_event_best_effort},
    home::get_home_dir,
    logging,
};

use app::{App, MemberRow};
use dashboard::{get_inbox_count, session_log_path};

/// TUI refresh poll interval (100 ms — see docs/tui-mvp-architecture.md §5).
const TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

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
    let mut tick = interval(TICK_INTERVAL);

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
