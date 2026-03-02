//! Log reader and filter utilities for the unified ATM logging pipeline.
//!
//! This module provides [`LogReader`] for reading and filtering [`LogEventV1`]
//! events from the daemon's JSONL log file, and [`format_event_human`] for
//! rendering events as human-readable text.
//!
//! # Usage
//!
//! ```no_run
//! use agent_team_mail_core::log_reader::{LogFilter, LogReader, format_event_human};
//! use std::path::PathBuf;
//! use std::time::Duration;
//!
//! let filter = LogFilter {
//!     agent: Some("team-lead".to_string()),
//!     level: Some("info".to_string()),
//!     since: Some(Duration::from_secs(3600)), // last hour
//!     limit: Some(50),
//! };
//!
//! let reader = LogReader::new(PathBuf::from("/path/to/atm.log.jsonl"), filter);
//! let events = reader.read_filtered().unwrap();
//! for event in &events {
//!     println!("{}", format_event_human(event));
//! }
//! ```

use crate::logging_event::LogEventV1;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

// ── LogFilter ─────────────────────────────────────────────────────────────────

/// Filter criteria applied when reading log events.
///
/// All provided filters are combined with AND semantics: an event must
/// satisfy every non-`None` criterion to be included in results.
///
/// `limit` is applied last, after all other filters, and returns the
/// *last* N matching events (tail semantics).
#[derive(Debug, Clone, Default)]
pub struct LogFilter {
    /// Include only events where `event.agent` matches this value (case-sensitive).
    pub agent: Option<String>,
    /// Include only events where `event.level` matches this value (case-insensitive).
    pub level: Option<String>,
    /// Include only events emitted within the last N seconds of wall-clock time.
    pub since: Option<Duration>,
    /// Return at most the last N matching events (applied after all other filters).
    pub limit: Option<usize>,
}

impl LogFilter {
    /// Return `true` if `event` passes all non-`None` filter criteria.
    pub fn matches(&self, event: &LogEventV1) -> bool {
        if let Some(agent) = &self.agent {
            if event.agent.as_deref() != Some(agent.as_str()) {
                return false;
            }
        }

        if let Some(level) = &self.level {
            if !event.level.eq_ignore_ascii_case(level) {
                return false;
            }
        }

        if let Some(since) = self.since {
            if !is_within_since(&event.ts, since) {
                return false;
            }
        }

        true
    }
}

/// Return `true` if the RFC 3339 timestamp `ts` is within `since` of now.
///
/// Malformed timestamps are treated as excluded (returns `false`).
fn is_within_since(ts: &str, since: Duration) -> bool {
    let Ok(event_time) = ts.parse::<DateTime<Utc>>() else {
        return false;
    };
    let cutoff = Utc::now()
        .checked_sub_signed(chrono::Duration::from_std(since).unwrap_or(chrono::Duration::zero()))
        .unwrap_or(DateTime::<Utc>::from(SystemTime::UNIX_EPOCH));
    event_time >= cutoff
}

// ── LogReader ─────────────────────────────────────────────────────────────────

/// Reader for the ATM daemon JSONL log file.
///
/// Reads events from the log file at `path`, applying [`LogFilter`] criteria.
///
/// # File not found
///
/// [`LogReader::read_filtered`] returns `Ok(Vec::new())` when the log file
/// does not exist. Follow mode via [`LogReader::follow`] waits for the file
/// to appear.
pub struct LogReader {
    path: PathBuf,
    filter: LogFilter,
}

impl LogReader {
    /// Create a new [`LogReader`] for the given path and filter.
    pub fn new(path: PathBuf, filter: LogFilter) -> Self {
        Self { path, filter }
    }

    /// Read all events matching the filter.
    ///
    /// Lines that cannot be parsed as [`LogEventV1`] are silently skipped.
    /// When `filter.limit` is set, the **last** N matching events are returned
    /// (all lines are parsed first; then the final N are retained).
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be opened or read.
    /// A missing file returns `Ok(Vec::new())`.
    pub fn read_filtered(&self) -> Result<Vec<LogEventV1>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&self.path)
            .with_context(|| format!("Failed to open log file: {}", self.path.display()))?;
        let reader = BufReader::new(file);

        let mut matched: Vec<LogEventV1> = Vec::new();

        for line in reader.lines() {
            let line =
                line.with_context(|| format!("Failed to read log file: {}", self.path.display()))?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_str::<LogEventV1>(line) else {
                continue;
            };
            if self.filter.matches(&event) {
                matched.push(event);
            }
        }

        // Apply limit: keep the last N.
        if let Some(limit) = self.filter.limit {
            if matched.len() > limit {
                let start = matched.len() - limit;
                matched = matched[start..].to_vec();
            }
        }

        Ok(matched)
    }

    /// Follow the log file, calling `callback` for each new matching event.
    ///
    /// Seeks to the end of the file on entry, then polls every 500 ms for new
    /// lines. If the file does not yet exist the function waits until it appears.
    ///
    /// The `callback` receives a reference to each new matching [`LogEventV1`].
    /// Return `true` to continue following; return `false` to stop.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be opened or read.
    pub fn follow<F>(&self, mut callback: F) -> Result<()>
    where
        F: FnMut(&LogEventV1) -> bool,
    {
        // Wait for the file to appear if it does not exist yet.
        let mut file = loop {
            if self.path.exists() {
                break File::open(&self.path).with_context(|| {
                    format!("Failed to open log file: {}", self.path.display())
                })?;
            }
            std::thread::sleep(Duration::from_millis(500));
        };

        // Seek to end — only show new entries from this point.
        let mut pos = file
            .seek(SeekFrom::End(0))
            .context("Failed to seek to end of log file")?;

        loop {
            std::thread::sleep(Duration::from_millis(500));

            // Detect rotation / truncation.
            let metadata = match std::fs::metadata(&self.path) {
                Ok(m) => m,
                Err(_) => continue, // file transiently disappeared
            };

            if metadata.len() < pos {
                // File was truncated or rotated — reopen from the start.
                file = File::open(&self.path).with_context(|| {
                    format!("Failed to re-open log file: {}", self.path.display())
                })?;
                pos = 0;
            }

            file.seek(SeekFrom::Start(pos))
                .context("Failed to seek log file")?;

            let mut reader = BufReader::new(&file);
            let mut new_bytes: u64 = 0;
            let mut line = String::new();

            loop {
                let bytes = match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => return Err(e).context("Failed to read log file"),
                };
                new_bytes += bytes as u64;
                // Only process complete lines.
                if line.ends_with('\n') {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        if let Ok(event) = serde_json::from_str::<LogEventV1>(trimmed) {
                            if self.filter.matches(&event) && !callback(&event) {
                                return Ok(());
                            }
                        }
                    }
                }
                line.clear();
            }

            pos += new_bytes;
        }
    }
}

// ── Human-readable formatting ─────────────────────────────────────────────────

/// ANSI color codes for log level highlighting.
mod ansi {
    pub const RED: &str = "\x1b[31m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const CYAN: &str = "\x1b[36m";
    pub const DIM: &str = "\x1b[2m";
    pub const RESET: &str = "\x1b[0m";
}

/// Return `true` when stdout is connected to a TTY (ANSI colors are appropriate).
fn stdout_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// Format a [`LogEventV1`] as a human-readable line.
///
/// Output format:
/// ```text
/// 2026-02-23T10:30:00Z  INFO  [atm/team-lead] send_message (ok)
/// 2026-02-23T10:30:01Z  WARN  [atm-daemon] queue_full: dropped 5 events
/// 2026-02-23T10:30:02Z ERROR  [atm/arch-ctm] dispatch_error: connection refused
/// ```
///
/// When stdout is a TTY, level names are colorized:
/// - `error` → red
/// - `warn` → yellow
/// - `info` → default
/// - `debug` → cyan
/// - `trace` → dim
pub fn format_event_human(event: &LogEventV1) -> String {
    let use_color = stdout_is_tty();

    let level_upper = event.level.to_uppercase();
    // Pad level to 5 chars for alignment.
    let level_padded = format!("{:<5}", level_upper);

    let colored_level = if use_color {
        let color = match event.level.to_lowercase().as_str() {
            "error" => ansi::RED,
            "warn" => ansi::YELLOW,
            "debug" => ansi::CYAN,
            "trace" => ansi::DIM,
            _ => ansi::RESET,
        };
        format!("{color}{level_padded}{}", ansi::RESET)
    } else {
        level_padded
    };

    let agent_suffix = match &event.agent {
        Some(a) => format!("/{a}"),
        None => String::new(),
    };

    let msg_suffix = if let Some(err) = &event.error {
        format!(": {err}")
    } else if let Some(outcome) = &event.outcome {
        format!(" ({outcome})")
    } else {
        String::new()
    };

    let ppid_suffix = event
        .fields
        .get("ppid")
        .and_then(|v| v.as_u64())
        .map(|ppid| format!("/ppid={ppid}"))
        .unwrap_or_default();

    let target_suffix = if event.target.is_empty() {
        String::new()
    } else {
        format!(" -> {}", event.target)
    };

    format!(
        "{}  {}  [{}{} pid={}{}] {}{}{}",
        event.ts,
        colored_level,
        event.source_binary,
        agent_suffix,
        event.pid,
        ppid_suffix,
        event.action,
        msg_suffix,
        target_suffix,
    )
}

/// Parse a human-readable duration string into a [`Duration`].
///
/// Accepted formats:
/// - `Ns` — N seconds (e.g., `"90s"`)
/// - `Nm` — N minutes (e.g., `"30m"`)
/// - `Nh` — N hours (e.g., `"2h"`)
///
/// # Errors
///
/// Returns an error if the string does not match the expected format or the
/// numeric part cannot be parsed.
pub fn parse_since(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("empty duration string");
    }
    let (num_part, unit) = s.split_at(s.len() - 1);
    let n: u64 = num_part
        .parse()
        .with_context(|| format!("invalid duration value '{num_part}' in '{s}'"))?;
    match unit {
        "s" => Ok(Duration::from_secs(n)),
        "m" => Ok(Duration::from_secs(n * 60)),
        "h" => Ok(Duration::from_secs(n * 3600)),
        other => {
            anyhow::bail!("unknown duration unit '{other}' in '{s}'; expected 's', 'm', or 'h'")
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging_event::{LogEventV1, new_log_event};
    use chrono::{Duration as ChronoDuration, Utc};
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    // ── helpers ───────────────────────────────────────────────────────────────

    fn make_event_with_agent(agent: &str, level: &str) -> LogEventV1 {
        LogEventV1::builder("atm", "test_action", "atm::test")
            .level(level)
            .agent(agent)
            .build()
    }

    fn make_event_at_ts(ts: &str, level: &str) -> LogEventV1 {
        let mut ev = new_log_event("atm", "test_action", "atm::test", level);
        ev.ts = ts.to_string();
        ev
    }

    fn write_events_to_file(events: &[LogEventV1]) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("temp file");
        for event in events {
            let line = serde_json::to_string(event).expect("serialize");
            writeln!(f, "{line}").expect("write line");
        }
        f.flush().expect("flush");
        f
    }

    // ── parse_since ───────────────────────────────────────────────────────────

    #[test]
    fn test_parse_since_seconds() {
        assert_eq!(parse_since("90s").unwrap(), Duration::from_secs(90));
    }

    #[test]
    fn test_parse_since_minutes() {
        assert_eq!(parse_since("30m").unwrap(), Duration::from_secs(1800));
    }

    #[test]
    fn test_parse_since_hours() {
        assert_eq!(parse_since("2h").unwrap(), Duration::from_secs(7200));
    }

    #[test]
    fn test_parse_since_invalid_unit() {
        assert!(parse_since("5d").is_err());
    }

    #[test]
    fn test_parse_since_empty_string() {
        assert!(parse_since("").is_err());
    }

    #[test]
    fn test_parse_since_non_numeric() {
        assert!(parse_since("xm").is_err());
    }

    // ── test_filter_by_agent ──────────────────────────────────────────────────

    #[test]
    fn test_filter_by_agent() {
        let events = vec![
            make_event_with_agent("team-lead", "info"),
            make_event_with_agent("team-lead", "info"),
            make_event_with_agent("arch-ctm", "info"),
        ];
        let f = write_events_to_file(&events);

        let filter = LogFilter {
            agent: Some("team-lead".to_string()),
            ..Default::default()
        };
        let reader = LogReader::new(f.path().to_path_buf(), filter);
        let results = reader.read_filtered().expect("read_filtered");

        assert_eq!(results.len(), 2);
        for ev in &results {
            assert_eq!(ev.agent.as_deref(), Some("team-lead"));
        }
    }

    // ── test_filter_by_level ──────────────────────────────────────────────────

    #[test]
    fn test_filter_by_level() {
        let events = vec![
            make_event_with_agent("a", "info"),
            make_event_with_agent("a", "warn"),
            make_event_with_agent("a", "error"),
            make_event_with_agent("a", "warn"),
        ];
        let f = write_events_to_file(&events);

        let filter = LogFilter {
            level: Some("warn".to_string()),
            ..Default::default()
        };
        let reader = LogReader::new(f.path().to_path_buf(), filter);
        let results = reader.read_filtered().expect("read_filtered");

        assert_eq!(results.len(), 2);
        for ev in &results {
            assert_eq!(ev.level, "warn");
        }
    }

    #[test]
    fn test_filter_by_level_case_insensitive() {
        let events = vec![
            make_event_with_agent("a", "INFO"),
            make_event_with_agent("a", "warn"),
        ];
        let f = write_events_to_file(&events);

        let filter = LogFilter {
            level: Some("info".to_string()),
            ..Default::default()
        };
        let reader = LogReader::new(f.path().to_path_buf(), filter);
        let results = reader.read_filtered().expect("read_filtered");

        assert_eq!(results.len(), 1);
    }

    // ── test_filter_since ─────────────────────────────────────────────────────

    #[test]
    fn test_filter_since() {
        let now = Utc::now();
        let two_hours_ago =
            (now - ChronoDuration::hours(2)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let thirty_mins_ago =
            (now - ChronoDuration::minutes(30)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let one_min_ago =
            (now - ChronoDuration::minutes(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let events = vec![
            make_event_at_ts(&two_hours_ago, "info"),   // too old
            make_event_at_ts(&thirty_mins_ago, "info"), // within last hour
            make_event_at_ts(&one_min_ago, "info"),     // within last hour
        ];
        let f = write_events_to_file(&events);

        let filter = LogFilter {
            since: Some(Duration::from_secs(3600)), // last hour
            ..Default::default()
        };
        let reader = LogReader::new(f.path().to_path_buf(), filter);
        let results = reader.read_filtered().expect("read_filtered");

        assert_eq!(
            results.len(),
            2,
            "only events within the last hour should match"
        );
    }

    // ── test_limit ────────────────────────────────────────────────────────────

    #[test]
    fn test_limit() {
        let events: Vec<LogEventV1> = (0..10)
            .map(|i| {
                let mut ev = new_log_event("atm", &format!("action_{i}"), "atm::test", "info");
                ev.action = format!("action_{i}");
                ev
            })
            .collect();
        let f = write_events_to_file(&events);

        let filter = LogFilter {
            limit: Some(3),
            ..Default::default()
        };
        let reader = LogReader::new(f.path().to_path_buf(), filter);
        let results = reader.read_filtered().expect("read_filtered");

        assert_eq!(results.len(), 3, "limit=3 should return last 3 events");
        // Last 3 events should be action_7, action_8, action_9.
        assert_eq!(results[0].action, "action_7");
        assert_eq!(results[1].action, "action_8");
        assert_eq!(results[2].action, "action_9");
    }

    // ── test_format_human ─────────────────────────────────────────────────────

    #[test]
    fn test_format_human() {
        let mut ev = new_log_event("atm", "send_message", "atm::send", "info");
        ev.ts = "2026-02-23T10:30:00Z".to_string();
        ev.agent = Some("team-lead".to_string());

        let formatted = format_event_human(&ev);
        assert!(
            formatted.contains("2026-02-23T10:30:00Z"),
            "must contain timestamp"
        );
        assert!(formatted.contains("INFO"), "must contain level");
        assert!(formatted.contains("send_message"), "must contain action");
        assert!(
            formatted.contains("pid="),
            "must include pid in human-rendered output"
        );
    }

    #[test]
    fn test_format_human_error_suffix() {
        let mut ev = new_log_event("atm", "dispatch_error", "atm::send", "error");
        ev.ts = "2026-02-23T10:30:02Z".to_string();
        ev.agent = Some("arch-ctm".to_string());
        ev.error = Some("connection refused".to_string());

        let formatted = format_event_human(&ev);
        assert!(
            formatted.contains(": connection refused"),
            "must contain error suffix"
        );
    }

    #[test]
    fn test_format_human_outcome_suffix() {
        let mut ev = new_log_event("atm", "send_message", "atm::send", "info");
        ev.ts = "2026-02-23T10:30:00Z".to_string();
        ev.outcome = Some("ok".to_string());

        let formatted = format_event_human(&ev);
        assert!(formatted.contains("(ok)"), "must contain outcome suffix");
    }

    #[test]
    fn test_format_human_no_agent_suffix() {
        let ev = new_log_event("atm-daemon", "daemon_start", "atm_daemon::main", "info");
        let formatted = format_event_human(&ev);
        assert!(
            formatted.contains("[atm-daemon pid="),
            "no agent suffix when agent is None; got: {formatted}"
        );
    }

    #[test]
    fn test_format_human_includes_target_suffix() {
        let ev = new_log_event("atm", "send_message", "atm::send", "info");
        let formatted = format_event_human(&ev);
        assert!(
            formatted.contains("-> atm::send"),
            "formatted event should include target suffix"
        );
    }

    #[test]
    fn test_format_human_includes_ppid_when_present() {
        let mut ev = new_log_event("atm", "send_message", "atm::send", "info");
        ev.fields
            .insert("ppid".to_string(), serde_json::Value::Number(123u64.into()));
        let formatted = format_event_human(&ev);
        assert!(
            formatted.contains("ppid=123"),
            "formatted event should include ppid when available"
        );
    }

    // ── test_nonexistent_file_returns_empty ───────────────────────────────────

    #[test]
    fn test_nonexistent_file_returns_empty() {
        let tmp = TempDir::new().expect("temp dir");
        let path = tmp.path().join("no-such-file.jsonl");

        let filter = LogFilter::default();
        let reader = LogReader::new(path, filter);
        let results = reader
            .read_filtered()
            .expect("should return Ok on missing file");

        assert!(results.is_empty(), "missing file should return empty vec");
    }

    // ── test_follow_mode ──────────────────────────────────────────────────────

    #[test]
    fn test_follow_mode() {
        use std::io::Write;
        use std::sync::mpsc;
        use std::sync::{Arc, Mutex};

        // Create an initial log file with 2 events.
        let tmp = TempDir::new().expect("temp dir");
        let log_path = tmp.path().join("atm.log.jsonl");

        // Write initial events (these will be skipped by follow — it seeks to end).
        {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .expect("open file");
            let ev = new_log_event("atm", "before_follow", "atm::test", "info");
            writeln!(file, "{}", serde_json::to_string(&ev).unwrap()).unwrap();
        }

        let collected: Arc<Mutex<Vec<LogEventV1>>> = Arc::new(Mutex::new(Vec::new()));
        let collected_clone = collected.clone();
        let log_path_clone = log_path.clone();

        // Spawn a thread that appends 3 events after a short delay.
        // 700ms initial delay gives the follow thread time to open the file and
        // seek to the end before any events are written.  Events are written
        // 150ms apart so all 3 arrive well within the 10s test timeout below.
        let writer_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path_clone)
                .expect("open for appending");
            for i in 0..3u32 {
                let mut ev =
                    new_log_event("atm", &format!("follow_event_{i}"), "atm::test", "info");
                ev.action = format!("follow_event_{i}");
                writeln!(file, "{}", serde_json::to_string(&ev).unwrap()).unwrap();
                file.flush().unwrap();
                std::thread::sleep(Duration::from_millis(150));
            }
        });

        // Use a channel so the test can detect a stuck follow thread and fail
        // with a clear diagnostic message instead of hanging forever.
        let (done_tx, done_rx) = mpsc::channel::<()>();

        // Run follow on a reader thread, stopping after 3 events.
        let filter = LogFilter::default();
        let reader = LogReader::new(log_path.clone(), filter);

        let follow_thread = std::thread::spawn(move || {
            reader
                .follow(|event| {
                    let mut guard = collected_clone.lock().unwrap();
                    guard.push(event.clone());
                    // Stop after 3 new events.
                    guard.len() < 3
                })
                .expect("follow should succeed");
            // Signal the main thread that follow completed.
            let _ = done_tx.send(());
        });

        writer_thread.join().expect("writer thread joined");

        // Wait for the follow thread to finish with a generous timeout.
        // 10 seconds is far more than needed (total expected time ≈ 700 + 3*150 + 1*500 = ~1.7s)
        // but still prevents an infinite hang on pathological CI runners.
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("follow thread did not finish within 10s — possible deadlock");

        follow_thread.join().expect("follow thread joined");

        let guard = collected.lock().unwrap();
        assert_eq!(
            guard.len(),
            3,
            "follow should have yielded exactly 3 new events"
        );
        assert!(
            guard[0].action.starts_with("follow_event_"),
            "actions should be follow events"
        );
    }

    // ── malformed lines are skipped ───────────────────────────────────────────

    #[test]
    fn test_malformed_lines_skipped() {
        let tmp = TempDir::new().expect("temp dir");
        let path = tmp.path().join("atm.log.jsonl");

        {
            let mut f = std::fs::File::create(&path).expect("create");
            writeln!(f, "not valid json").unwrap();
            writeln!(f, "{{\"garbage\": true}}").unwrap();
            let ev = new_log_event("atm", "real_event", "atm::test", "info");
            writeln!(f, "{}", serde_json::to_string(&ev).unwrap()).unwrap();
        }

        let filter = LogFilter::default();
        let reader = LogReader::new(path, filter);
        let results = reader.read_filtered().expect("read_filtered");
        assert_eq!(results.len(), 1, "only the valid event should be returned");
        assert_eq!(results[0].action, "real_event");
    }
}
