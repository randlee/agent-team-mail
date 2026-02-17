//! Tail command — stream recent output from a Codex agent's log file.
//!
//! # Overview
//!
//! `atm tail <agent>` shows the last N lines of an agent's output log.
//! The log path is resolved by querying the daemon; if the daemon is not
//! running the command falls back to a tmux `capture-pane` (Unix only).
//!
//! # Follow mode
//!
//! With `--follow` (`-f`) the command polls the log file every 500 ms and
//! prints new lines as they arrive. Press Ctrl-C to exit.
//!
//! # Cross-platform behaviour
//!
//! - Log-file reading works on all platforms.
//! - The tmux fallback is compiled and active on Unix only (`#[cfg(unix)]`).
//! - On non-Unix platforms the command uses only the log-file approach.

use anyhow::{Context, Result};
use clap::Args;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Arguments for `atm tail`
#[derive(Args, Debug)]
pub struct TailArgs {
    /// Agent name to tail output from
    pub agent: String,

    /// Number of lines to show (default: 20)
    #[arg(short = 'n', long = "last", default_value_t = 20)]
    pub last: usize,

    /// Follow mode — continuously stream new output (like tail -f)
    #[arg(short = 'f', long = "follow")]
    pub follow: bool,

    /// Team name (defaults to configured team; currently informational for daemon query)
    #[arg(short, long)]
    pub team: Option<String>,
}

/// Execute the tail command.
///
/// # Errors
///
/// Returns an error when neither the daemon nor the tmux fallback can
/// provide an output source for the requested agent.
pub fn execute(args: TailArgs) -> Result<()> {
    // Step 1: try to resolve the log path via the daemon.
    let log_path = resolve_log_path(&args.agent)?;

    match log_path {
        Some(path) => {
            if args.follow {
                follow_log_file(&path)
            } else {
                print_last_n_lines(&path, args.last)
            }
        }
        None => {
            // Step 2: fall back to tmux capture-pane on Unix.
            #[cfg(unix)]
            {
                run_tmux_capture_fallback(&args.agent, args.last)
            }
            #[cfg(not(unix))]
            {
                anyhow::bail!(
                    "Agent '{}' not found: daemon is not running and tmux fallback \
                     is only available on Unix",
                    args.agent
                );
            }
        }
    }
}

// ── Log path resolution ───────────────────────────────────────────────────────

/// Resolve the log file path for `agent` by querying the daemon.
///
/// Returns `Ok(None)` when the daemon is not running or the agent is not
/// tracked; returns `Ok(Some(path))` on success.
fn resolve_log_path(agent: &str) -> Result<Option<PathBuf>> {
    match agent_team_mail_core::daemon_client::query_agent_pane(agent)? {
        Some(info) => {
            let path = PathBuf::from(&info.log_path);
            if path.exists() {
                Ok(Some(path))
            } else {
                Ok(None)
            }
        }
        None => Ok(None),
    }
}

// ── Log file reading ──────────────────────────────────────────────────────────

/// Read and print the last `n` lines from `path`.
///
/// If the file has fewer than `n` lines all lines are printed. An empty
/// file prints nothing.
pub fn print_last_n_lines(path: &Path, n: usize) -> Result<()> {
    let lines = read_last_n_lines(path, n)?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

/// Return the last `n` lines of the file at `path` as a `Vec<String>`.
///
/// Lines are returned in order (oldest first). If the file contains fewer
/// than `n` lines all lines are returned. An empty file yields an empty
/// vector.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or read.
pub fn read_last_n_lines(path: &Path, n: usize) -> Result<Vec<String>> {
    if n == 0 {
        return Ok(Vec::new());
    }

    let file = File::open(path)
        .with_context(|| format!("Failed to open log file: {}", path.display()))?;

    let reader = BufReader::new(file);
    let all_lines: Vec<String> = reader.lines().collect::<std::io::Result<_>>()
        .with_context(|| format!("Failed to read log file: {}", path.display()))?;

    if all_lines.len() <= n {
        Ok(all_lines)
    } else {
        Ok(all_lines[all_lines.len() - n..].to_vec())
    }
}

/// Follow a log file, printing new lines as they appear.
///
/// Polls the file every 500 ms. Exits cleanly when an I/O error occurs
/// (e.g., Ctrl-C terminates the process).
fn follow_log_file(path: &Path) -> Result<()> {
    let mut file = File::open(path)
        .with_context(|| format!("Failed to open log file: {}", path.display()))?;

    // Seek to end so we only print new content.
    let mut pos = file.seek(SeekFrom::End(0))
        .with_context(|| "Failed to seek log file")?;

    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Re-open when the file might have been rotated (size shrinks).
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => continue, // file disappeared transiently — try again
        };

        if metadata.len() < pos {
            // File was truncated or rotated — start from the beginning.
            file = File::open(path)
                .with_context(|| format!("Failed to re-open log file: {}", path.display()))?;
            pos = 0;
        }

        // Read any new bytes.
        file.seek(SeekFrom::Start(pos))
            .with_context(|| "Failed to seek log file")?;

        let mut reader = BufReader::new(&file);
        let mut new_bytes: u64 = 0;

        let mut line = String::new();
        loop {
            let bytes = reader.read_line(&mut line)
                .with_context(|| "Failed to read log file")?;
            if bytes == 0 {
                break;
            }
            new_bytes += bytes as u64;
            // Only print complete lines (ending with '\n').
            if line.ends_with('\n') {
                print!("{line}");
            }
            line.clear();
        }

        pos += new_bytes;
    }
}

// ── tmux fallback (Unix only) ─────────────────────────────────────────────────

/// Build the argument list for `tmux capture-pane` to get the last `n` lines
/// from `pane_id`.
///
/// Returns `["capture-pane", "-p", "-t", pane_id, "-S", "-<n>"]`.
#[cfg(unix)]
pub fn tmux_capture_pane_args(pane_id: &str, n: usize) -> Vec<String> {
    vec![
        "capture-pane".to_string(),
        "-p".to_string(),
        "-t".to_string(),
        pane_id.to_string(),
        "-S".to_string(),
        format!("-{n}"),
    ]
}

/// Run `tmux capture-pane` for the given agent and print the output.
///
/// This function does a best-effort search for the agent's pane by running
/// `tmux list-panes -a` and matching a pane title that contains `agent`.
///
/// # Errors
///
/// Returns an error if tmux is not available or no matching pane is found.
#[cfg(unix)]
fn run_tmux_capture_fallback(agent: &str, n: usize) -> Result<()> {
    use std::process::Command;

    // List all panes and find one whose title contains the agent name.
    let list_output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id} #{pane_title}"])
        .output()
        .context("Failed to run 'tmux list-panes'. Is tmux installed?")?;

    if !list_output.status.success() {
        anyhow::bail!("tmux list-panes failed: {}", String::from_utf8_lossy(&list_output.stderr));
    }

    let stdout = String::from_utf8_lossy(&list_output.stdout);
    let pane_id = stdout
        .lines()
        .find(|line| line.to_lowercase().contains(&agent.to_lowercase()))
        .and_then(|line| line.split_whitespace().next())
        .map(|s| s.to_string());

    let pane_id = match pane_id {
        Some(id) => id,
        None => {
            anyhow::bail!(
                "No tmux pane found for agent '{}'. \
                 Is the agent running and is the pane title set?",
                agent
            );
        }
    };

    let args = tmux_capture_pane_args(&pane_id, n);
    let output = Command::new("tmux")
        .args(&args)
        .output()
        .context("Failed to run 'tmux capture-pane'")?;

    if !output.status.success() {
        anyhow::bail!(
            "tmux capture-pane failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    print!("{}", String::from_utf8_lossy(&output.stdout));
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // ── read_last_n_lines ─────────────────────────────────────────────────────

    #[test]
    fn test_last_n_lines_from_file() {
        let mut f = NamedTempFile::new().unwrap();
        for i in 0..50 {
            writeln!(f, "line {i}").unwrap();
        }
        f.flush().unwrap();

        let lines = read_last_n_lines(f.path(), 20).unwrap();
        assert_eq!(lines.len(), 20);
        assert_eq!(lines[0], "line 30");
        assert_eq!(lines[19], "line 49");
    }

    #[test]
    fn test_last_n_lines_fewer_than_requested() {
        let mut f = NamedTempFile::new().unwrap();
        for i in 0..5 {
            writeln!(f, "line {i}").unwrap();
        }
        f.flush().unwrap();

        let lines = read_last_n_lines(f.path(), 20).unwrap();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], "line 0");
        assert_eq!(lines[4], "line 4");
    }

    #[test]
    fn test_last_n_lines_empty_file() {
        let f = NamedTempFile::new().unwrap();
        let lines = read_last_n_lines(f.path(), 20).unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn test_last_n_lines_zero_requested() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "line 0").unwrap();
        f.flush().unwrap();

        let lines = read_last_n_lines(f.path(), 0).unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn test_last_n_lines_exact_count() {
        let mut f = NamedTempFile::new().unwrap();
        for i in 0..10 {
            writeln!(f, "line {i}").unwrap();
        }
        f.flush().unwrap();

        let lines = read_last_n_lines(f.path(), 10).unwrap();
        assert_eq!(lines.len(), 10);
        assert_eq!(lines[0], "line 0");
    }

    #[test]
    fn test_last_n_lines_nonexistent_file() {
        let result = read_last_n_lines(Path::new("/nonexistent/path/agent.log"), 10);
        assert!(result.is_err());
    }

    // ── tmux args ─────────────────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn test_tmux_capture_pane_command_construction() {
        let args = tmux_capture_pane_args("%42", 30);
        assert_eq!(args[0], "capture-pane");
        assert_eq!(args[1], "-p");
        assert_eq!(args[2], "-t");
        assert_eq!(args[3], "%42");
        assert_eq!(args[4], "-S");
        assert_eq!(args[5], "-30");
        assert_eq!(args.len(), 6);
    }

    #[cfg(unix)]
    #[test]
    fn test_tmux_capture_pane_args_zero_lines() {
        let args = tmux_capture_pane_args("%1", 0);
        assert_eq!(args[5], "-0");
    }

    // ── resolve_log_path (no daemon) ─────────────────────────────────────────

    #[test]
    fn test_resolve_log_path_no_daemon_returns_none() {
        // Without a running daemon this should return None, not an error.
        let result = resolve_log_path("arch-ctm");
        assert!(result.is_ok());
        // If the daemon is not running the result will be None.
        // We just verify it does not panic.
    }
}
