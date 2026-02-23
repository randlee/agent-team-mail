//! `atm logs` — view and follow the unified ATM daemon log file.
//!
//! # Overview
//!
//! Reads from the structured JSONL log written by `atm-daemon`.  Events can be
//! filtered by agent, level, and time window, and displayed either in a concise
//! human-readable format or as raw JSON lines.
//!
//! # Log file resolution (precedence)
//!
//! 1. `--file <path>` CLI flag
//! 2. `ATM_LOG_FILE` environment variable
//! 3. `ATM_LOG_PATH` environment variable (backward-compat alias for `ATM_LOG_FILE`)
//! 4. `{ATM_HOME or home_dir}/.config/atm/atm.log.jsonl`
//!
//! # Examples
//!
//! ```text
//! # Show last 50 entries (default)
//! atm logs
//!
//! # Follow new entries as they arrive
//! atm logs --follow
//!
//! # Filter by agent and show last 20
//! atm logs --agent team-lead --limit 20
//!
//! # Show events from the last 30 minutes in JSON
//! atm logs --since 30m --json
//! ```

use agent_team_mail_core::log_reader::{LogFilter, LogReader, format_event_human, parse_since};
use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

/// Arguments for `atm logs`
#[derive(Args, Debug)]
pub struct LogsArgs {
    /// Filter by source agent identity
    #[arg(long)]
    pub agent: Option<String>,

    /// Filter by log level (trace, debug, info, warn, error)
    #[arg(long)]
    pub level: Option<String>,

    /// Show logs from last N minutes/hours/seconds (e.g., 30m, 2h, 90s)
    #[arg(long)]
    pub since: Option<String>,

    /// Follow mode — tail new log entries as they arrive
    #[arg(short = 'f', long)]
    pub follow: bool,

    /// Output raw JSON lines instead of human-readable format
    #[arg(long)]
    pub json: bool,

    /// Show last N entries (default: 50)
    #[arg(long, default_value_t = 50)]
    pub limit: usize,

    /// Path to log file (default: ~/.config/atm/atm.log.jsonl)
    #[arg(long)]
    pub file: Option<PathBuf>,
}

/// Execute `atm logs`.
///
/// Resolves the log file path, builds a [`LogReader`] with the requested
/// filters, and prints matching events to stdout.
///
/// # Errors
///
/// Returns an error if the `--since` value cannot be parsed or if the log
/// file exists but cannot be read.
pub fn execute(args: LogsArgs) -> Result<()> {
    let log_path = resolve_log_path(&args)?;

    // Warn and exit cleanly if the log file does not exist.
    if !log_path.exists() {
        eprintln!("Log file not found: {}", log_path.display());
        return Ok(());
    }

    // Parse --since into a Duration.
    let since = match &args.since {
        Some(s) => Some(parse_since(s)?),
        None => None,
    };

    let filter = LogFilter {
        agent: args.agent.clone(),
        level: args.level.clone(),
        since,
        limit: if args.limit == 0 { None } else { Some(args.limit) },
    };

    let reader = LogReader::new(log_path.clone(), filter.clone());

    if args.follow {
        // Print the last `limit` existing entries first.
        let existing = reader.read_filtered()?;
        for event in &existing {
            print_event(event, args.json);
        }

        // Build a follow-mode filter (no limit — print every new match).
        let follow_filter = LogFilter {
            agent: filter.agent.clone(),
            level: filter.level.clone(),
            since: None, // follow mode: don't filter by time window
            limit: None,
        };
        let follow_reader = LogReader::new(log_path, follow_filter);
        follow_reader.follow(|event| {
            print_event(event, args.json);
            true // continue forever until Ctrl-C
        })?;
    } else {
        let events = reader.read_filtered()?;
        for event in &events {
            print_event(event, args.json);
        }
    }

    Ok(())
}

/// Resolve the log file path from CLI args, environment variable, or default.
fn resolve_log_path(args: &LogsArgs) -> Result<PathBuf> {
    if let Some(path) = &args.file {
        return Ok(path.clone());
    }
    if let Ok(p) = std::env::var("ATM_LOG_FILE") {
        if !p.trim().is_empty() {
            return Ok(PathBuf::from(p.trim()));
        }
    }
    if let Ok(p) = std::env::var("ATM_LOG_PATH") {
        eprintln!("atm: warning: ATM_LOG_PATH is deprecated; use ATM_LOG_FILE instead");
        if !p.trim().is_empty() {
            return Ok(PathBuf::from(p.trim()));
        }
    }
    let home = agent_team_mail_core::home::get_home_dir()?;
    Ok(home.join(".config/atm/atm.log.jsonl"))
}

/// Print a single event in either JSON or human-readable format.
fn print_event(event: &agent_team_mail_core::logging_event::LogEventV1, json_mode: bool) {
    if json_mode {
        if let Ok(line) = serde_json::to_string(event) {
            println!("{line}");
        }
    } else {
        println!("{}", format_event_human(event));
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_core::logging_event::{LogEventV1, new_log_event};
    use serial_test::serial;
    use std::io::Write;
    use tempfile::TempDir;

    fn make_event(agent: Option<&str>, level: &str, action: &str) -> LogEventV1 {
        let mut ev = new_log_event("atm", action, "atm::test", level);
        if let Some(a) = agent {
            ev.agent = Some(a.to_string());
        }
        ev
    }

    fn write_jsonl(tmp: &TempDir, events: &[LogEventV1]) -> PathBuf {
        let path = tmp.path().join("atm.log.jsonl");
        let mut f = std::fs::File::create(&path).expect("create log file");
        for ev in events {
            writeln!(f, "{}", serde_json::to_string(ev).unwrap()).unwrap();
        }
        f.flush().unwrap();
        path
    }

    // ── resolve_log_path ──────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_resolve_log_path_from_flag() {
        let tmp = TempDir::new().unwrap();
        let explicit = tmp.path().join("explicit.jsonl");
        let args = LogsArgs {
            agent: None,
            level: None,
            since: None,
            follow: false,
            json: false,
            limit: 50,
            file: Some(explicit.clone()),
        };
        let resolved = resolve_log_path(&args).unwrap();
        assert_eq!(resolved, explicit);
    }

    #[test]
    #[serial]
    fn test_resolve_log_path_from_env() {
        let tmp = TempDir::new().unwrap();
        let env_path = tmp.path().join("env.jsonl");

        unsafe { std::env::set_var("ATM_LOG_FILE", &env_path) };
        let args = LogsArgs {
            agent: None,
            level: None,
            since: None,
            follow: false,
            json: false,
            limit: 50,
            file: None,
        };
        let resolved = resolve_log_path(&args).unwrap();
        unsafe { std::env::remove_var("ATM_LOG_FILE") };

        assert_eq!(resolved, env_path);
    }

    #[test]
    #[serial]
    fn test_resolve_log_path_default_uses_atm_home() {
        let tmp = TempDir::new().unwrap();
        unsafe { std::env::remove_var("ATM_LOG_FILE") };
        unsafe { std::env::set_var("ATM_HOME", tmp.path()) };

        let args = LogsArgs {
            agent: None,
            level: None,
            since: None,
            follow: false,
            json: false,
            limit: 50,
            file: None,
        };
        let resolved = resolve_log_path(&args).unwrap();
        unsafe { std::env::remove_var("ATM_HOME") };

        let expected = tmp.path().join(".config/atm/atm.log.jsonl");
        assert_eq!(resolved, expected);
    }

    // ── missing log file prints warning and exits Ok ──────────────────────────

    #[test]
    #[serial]
    fn test_missing_log_file_exits_ok() {
        let tmp = TempDir::new().unwrap();
        let nonexistent = tmp.path().join("no-such.jsonl");

        unsafe { std::env::remove_var("ATM_LOG_FILE") };
        unsafe { std::env::set_var("ATM_HOME", tmp.path()) };

        let args = LogsArgs {
            agent: None,
            level: None,
            since: None,
            follow: false,
            json: false,
            limit: 50,
            file: Some(nonexistent),
        };
        let result = execute(args);
        unsafe { std::env::remove_var("ATM_HOME") };

        assert!(result.is_ok(), "missing log file should return Ok");
    }

    // ── --json output ─────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_json_output_mode() {
        let tmp = TempDir::new().unwrap();
        let events = vec![
            make_event(Some("team-lead"), "info", "send_message"),
            make_event(None, "warn", "queue_full"),
        ];
        let path = write_jsonl(&tmp, &events);

        // We test the LogReader + JSON re-serialization directly
        // (avoiding stdout capture complexity in unit tests).
        let filter = LogFilter {
            limit: Some(50),
            ..Default::default()
        };
        let reader = LogReader::new(path, filter);
        let results = reader.read_filtered().unwrap();

        assert_eq!(results.len(), 2);
        for ev in &results {
            // Verify JSON round-trip.
            let json = serde_json::to_string(ev).unwrap();
            let parsed: LogEventV1 = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.action, ev.action);
        }
    }

    // ── human-readable output ─────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_human_readable_output() {
        let tmp = TempDir::new().unwrap();
        let events = vec![make_event(Some("team-lead"), "info", "send_message")];
        let path = write_jsonl(&tmp, &events);

        let filter = LogFilter {
            limit: Some(50),
            ..Default::default()
        };
        let reader = LogReader::new(path, filter);
        let results = reader.read_filtered().unwrap();

        assert_eq!(results.len(), 1);
        let formatted = agent_team_mail_core::log_reader::format_event_human(&results[0]);
        assert!(formatted.contains("send_message"), "human output must contain action");
        assert!(formatted.contains("INFO"), "human output must contain level");
    }

    // ── --agent filter via execute ────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_agent_filter_via_execute() {
        let tmp = TempDir::new().unwrap();
        let events = vec![
            make_event(Some("team-lead"), "info", "send_message"),
            make_event(Some("team-lead"), "info", "read_messages"),
            make_event(Some("arch-ctm"), "info", "process_task"),
        ];
        let path = write_jsonl(&tmp, &events);

        // Use LogReader directly (execute() writes to stdout).
        let filter = LogFilter {
            agent: Some("team-lead".to_string()),
            limit: Some(50),
            ..Default::default()
        };
        let reader = LogReader::new(path, filter);
        let results = reader.read_filtered().unwrap();

        assert_eq!(results.len(), 2, "only team-lead events should be returned");
        for ev in &results {
            assert_eq!(ev.agent.as_deref(), Some("team-lead"));
        }
    }

    // ── ATM_LOG_PATH backward-compat alias ────────────────────────────────────

    #[test]
    #[serial]
    fn test_resolve_log_path_from_compat_env() {
        let dir = TempDir::new().unwrap();
        let custom = dir.path().join("compat.log.jsonl");

        unsafe { std::env::remove_var("ATM_LOG_FILE") };
        unsafe { std::env::set_var("ATM_LOG_PATH", &custom) };

        let args = LogsArgs {
            agent: None,
            level: None,
            since: None,
            follow: false,
            json: false,
            limit: 50,
            file: None,
        };
        let resolved = resolve_log_path(&args).unwrap();

        unsafe { std::env::remove_var("ATM_LOG_PATH") };

        assert_eq!(resolved, custom, "ATM_LOG_PATH should be honoured when ATM_LOG_FILE is absent");
    }

    // ── --limit 0 means unlimited ─────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_limit_zero_means_unlimited() {
        let tmp = TempDir::new().unwrap();
        let events: Vec<LogEventV1> = (0..5)
            .map(|i| make_event(Some("team-lead"), "info", &format!("action_{i}")))
            .collect();
        let path = write_jsonl(&tmp, &events);

        // limit=0 must map to None (no limit), returning all 5 events.
        let filter = LogFilter {
            limit: None, // mirrors what execute() produces when args.limit == 0
            ..Default::default()
        };
        let reader = LogReader::new(path, filter);
        let results = reader.read_filtered().unwrap();

        assert_eq!(results.len(), 5, "--limit 0 should return all events, not zero");
    }
}
