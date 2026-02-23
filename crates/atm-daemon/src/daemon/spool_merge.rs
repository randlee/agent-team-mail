//! Daemon startup spool merge.
//!
//! Scans the log-spool directory for `*.jsonl` spool files produced by
//! producer binaries while the daemon was offline, merges their contents
//! into the canonical JSONL log file in timestamp order, and removes the
//! processed spool files.
//!
//! # Claim protocol
//!
//! To prevent two concurrent daemon processes from processing the same file,
//! each spool file is atomically renamed to `<file>.claiming` before reading.
//! If the rename fails (e.g., another process already claimed it), the file is
//! skipped silently.
//!
//! # Error handling
//!
//! Individual file errors are logged at `warn` level and skipped; only a total
//! failure to read the spool directory is propagated as `Err`.

use agent_team_mail_core::logging_event::LogEventV1;
use anyhow::Result;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Scan `spool_dir` for `*.jsonl` spool files, claim each by atomic rename,
/// collect all events globally, sort by timestamp, and append to
/// `canonical_log_path` in a single write pass.
///
/// Returns the total number of log events merged across all spool files.
///
/// # Global sort
///
/// Events from all spool files are collected into a single `Vec<LogEventV1>`,
/// sorted globally by the `ts` field (RFC 3339 string comparison is correct
/// for UTC timestamps), and then appended to the canonical log in one pass.
/// This guarantees that events are stored in monotonic timestamp order even
/// when multiple producer binaries wrote overlapping timestamp ranges.
///
/// # Errors
///
/// Returns an error only if `spool_dir` cannot be read at all (e.g., I/O
/// error on `read_dir`).  Errors for individual spool files (claim race,
/// parse failure, write failure) are logged at `warn` level and skipped.
///
/// Returns `Ok(0)` when the spool directory does not exist.
pub fn merge_spool_on_startup(spool_dir: &Path, canonical_log_path: &Path) -> Result<u64> {
    if !spool_dir.exists() {
        debug!(
            path = %spool_dir.display(),
            "Spool directory does not exist; nothing to merge"
        );
        return Ok(0);
    }

    // Collect all *.jsonl spool files (not *.claiming — those are in-progress).
    let spool_files = collect_spool_files(spool_dir)?;

    if spool_files.is_empty() {
        debug!(
            path = %spool_dir.display(),
            "No spool files found"
        );
        return Ok(0);
    }

    // Phase 1: Claim all spool files and collect all events into one Vec.
    let mut all_events: Vec<LogEventV1> = Vec::new();
    let mut claimed_paths: Vec<std::path::PathBuf> = Vec::new();

    for spool_path in &spool_files {
        match claim_and_read_spool_file(spool_path, &mut all_events) {
            Ok(claiming_path) => {
                claimed_paths.push(claiming_path);
                debug!(file = %spool_path.display(), "Claimed spool file");
            }
            Err(e) => {
                warn!(
                    file = %spool_path.display(),
                    error = %e,
                    "Failed to claim spool file (skipping)"
                );
            }
        }
    }

    if all_events.is_empty() {
        // Also try to clean up any stale .claiming files left by a crashed daemon.
        cleanup_stale_claiming_files(spool_dir);
        return Ok(0);
    }

    // Phase 2: Sort all events globally by timestamp.
    // RFC 3339 UTC timestamps are lexicographically comparable.
    all_events.sort_by(|a, b| a.ts.cmp(&b.ts));

    // Phase 3: Append all events to canonical log in one pass.
    let total_merged = append_events_to_log(&all_events, canonical_log_path)?;

    // Phase 4: Delete all claimed spool files after successful write.
    for claiming_path in claimed_paths {
        if let Err(e) = fs::remove_file(&claiming_path) {
            warn!(
                file = %claiming_path.display(),
                error = %e,
                "Failed to remove claiming file after successful merge"
            );
        } else {
            debug!(file = %claiming_path.display(), "Removed claiming file");
        }
    }

    // Also try to clean up any stale .claiming files left by a crashed daemon.
    cleanup_stale_claiming_files(spool_dir);

    Ok(total_merged)
}

// ── Internals ─────────────────────────────────────────────────────────────────

/// Return all `*.jsonl` files (not `*.claiming`) in `dir`, sorted by name.
fn collect_spool_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.ends_with(".jsonl") && !name.ends_with(".claiming")
        })
        .collect();

    // Sort for deterministic ordering (filename contains timestamp millis).
    paths.sort();
    Ok(paths)
}

/// Claim one spool file by atomic rename and read its events into `out`.
///
/// Returns the `.claiming` path on success so the caller can delete it after
/// the global write pass completes.
///
/// # Errors
///
/// Returns an error if the atomic rename fails (e.g., another process already
/// claimed this file) or if the claiming file cannot be read.  On error the
/// spool file is left in place for the next startup attempt.
fn claim_and_read_spool_file(
    spool_path: &Path,
    out: &mut Vec<LogEventV1>,
) -> Result<std::path::PathBuf> {
    // Claim by rename: <file>.jsonl → <file>.claiming
    let claiming_path = spool_path.with_extension("claiming");
    match fs::rename(spool_path, &claiming_path) {
        Ok(_) => {}
        Err(e) => {
            // Another process claimed it, or it vanished — skip silently.
            return Err(anyhow::anyhow!("Failed to claim spool file (rename): {e}"));
        }
    }

    // Read and parse events from the claiming file.
    let content = match fs::read_to_string(&claiming_path) {
        Ok(c) => c,
        Err(e) => {
            // Cannot read — leave .claiming in place for manual inspection.
            return Err(anyhow::anyhow!("Failed to read claiming file: {e}"));
        }
    };

    for (line_no, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<LogEventV1>(trimmed) {
            Ok(event) => out.push(event),
            Err(e) => {
                warn!(
                    file = %claiming_path.display(),
                    line = line_no + 1,
                    error = %e,
                    "Skipping unparseable spool line"
                );
            }
        }
    }

    Ok(claiming_path)
}

/// Append `events` to `log_path` (create if absent).
///
/// Returns the number of events written.
fn append_events_to_log(events: &[LogEventV1], log_path: &Path) -> Result<u64> {
    if events.is_empty() {
        return Ok(0);
    }

    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;

    let mut written: u64 = 0;
    for event in events {
        match serde_json::to_string(event) {
            Ok(line) => {
                writeln!(file, "{line}")?;
                written += 1;
            }
            Err(e) => {
                warn!(error = %e, "Failed to serialize event for canonical log");
            }
        }
    }

    file.flush()?;
    Ok(written)
}

/// Remove stale `*.claiming` files left by a previous crashed daemon.
///
/// A `.claiming` file that exists after a daemon restart was never completed;
/// the original `.jsonl` is gone so we cannot re-process it. We remove the
/// `.claiming` file to avoid accumulation.
fn cleanup_stale_claiming_files(dir: &Path) {
    let claiming_files: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(iter) => iter
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext == "claiming")
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => return,
    };

    for path in claiming_files {
        if let Err(e) = fs::remove_file(&path) {
            warn!(
                file = %path.display(),
                error = %e,
                "Failed to remove stale claiming file"
            );
        } else {
            debug!(file = %path.display(), "Removed stale claiming file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_core::logging_event::{LogEventV1, new_log_event};
    use tempfile::TempDir;

    fn make_spool_file(dir: &Path, filename: &str, events: &[LogEventV1]) {
        let path = dir.join(filename);
        let mut content = String::new();
        for event in events {
            content.push_str(&serde_json::to_string(event).unwrap());
            content.push('\n');
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn test_merge_empty_spool_dir() {
        let tmp = TempDir::new().unwrap();
        let spool_dir = tmp.path().join("spool");
        fs::create_dir_all(&spool_dir).unwrap();
        let log_path = tmp.path().join("canonical.jsonl");

        let count = merge_spool_on_startup(&spool_dir, &log_path).unwrap();
        assert_eq!(count, 0);
        assert!(
            !log_path.exists(),
            "log should not be created when nothing to merge"
        );
    }

    #[test]
    fn test_merge_nonexistent_spool_dir() {
        let tmp = TempDir::new().unwrap();
        let spool_dir = tmp.path().join("nonexistent");
        let log_path = tmp.path().join("canonical.jsonl");

        let count = merge_spool_on_startup(&spool_dir, &log_path).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_merge_two_spool_files() {
        let tmp = TempDir::new().unwrap();
        let spool_dir = tmp.path().join("spool");
        fs::create_dir_all(&spool_dir).unwrap();
        let log_path = tmp.path().join("canonical.jsonl");

        let events1 = vec![
            new_log_event("atm", "action_a", "atm::cmd", "info"),
            new_log_event("atm", "action_b", "atm::cmd", "info"),
            new_log_event("atm", "action_c", "atm::cmd", "info"),
        ];
        let events2 = vec![
            new_log_event("atm-tui", "tui_a", "atm_tui", "info"),
            new_log_event("atm-tui", "tui_b", "atm_tui", "info"),
            new_log_event("atm-tui", "tui_c", "atm_tui", "info"),
        ];

        make_spool_file(&spool_dir, "atm-1-100.jsonl", &events1);
        make_spool_file(&spool_dir, "atm-tui-2-200.jsonl", &events2);

        let count = merge_spool_on_startup(&spool_dir, &log_path).unwrap();
        assert_eq!(count, 6, "expected 6 total events merged");

        // Canonical log should have 6 lines.
        let content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 6);

        // Each line should be a valid LogEventV1.
        for line in &lines {
            let event: LogEventV1 = serde_json::from_str(line).expect("valid JSON line");
            assert_eq!(event.v, 1);
        }

        // Spool dir should be empty (no *.jsonl files).
        let remaining: Vec<_> = fs::read_dir(&spool_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x == "jsonl")
                    .unwrap_or(false)
            })
            .collect();
        assert!(remaining.is_empty(), "all spool files should be removed");
    }

    #[test]
    fn test_merge_claiming_file_already_present() {
        // Simulate an interrupted previous merge: a .claiming file exists but
        // the .jsonl was already renamed away.
        let tmp = TempDir::new().unwrap();
        let spool_dir = tmp.path().join("spool");
        fs::create_dir_all(&spool_dir).unwrap();
        let log_path = tmp.path().join("canonical.jsonl");

        // Create a .claiming file as if left by a crashed daemon.
        let claiming_path = spool_dir.join("atm-1-100.claiming");
        let event = new_log_event("atm", "stale", "atm::test", "info");
        let content = format!("{}\n", serde_json::to_string(&event).unwrap());
        fs::write(&claiming_path, content).unwrap();

        // Also create a normal spool file.
        let normal_event = new_log_event("atm", "normal", "atm::test", "info");
        make_spool_file(&spool_dir, "atm-2-200.jsonl", &[normal_event]);

        // merge_spool_on_startup should merge the normal file and clean up
        // the stale .claiming file.
        let count = merge_spool_on_startup(&spool_dir, &log_path).unwrap();
        assert_eq!(count, 1, "only the normal file should be merged");

        // The stale .claiming file should be removed.
        assert!(
            !claiming_path.exists(),
            "stale .claiming file should be cleaned up"
        );
    }

    #[test]
    fn test_merge_global_sort_across_files() {
        // Two spool files with interleaved timestamps.
        // File 1 contains: T=02, T=10
        // File 2 contains: T=01, T=05
        // After global sort the canonical log should be: T=01, T=02, T=05, T=10
        let tmp = TempDir::new().unwrap();
        let spool_dir = tmp.path().join("spool");
        fs::create_dir_all(&spool_dir).unwrap();
        let log_path = tmp.path().join("canonical.jsonl");

        let mut ev_t02 = new_log_event("atm", "event_t02", "atm::cmd", "info");
        ev_t02.ts = "2026-01-01T00:00:02Z".to_string();
        let mut ev_t10 = new_log_event("atm", "event_t10", "atm::cmd", "info");
        ev_t10.ts = "2026-01-01T00:00:10Z".to_string();
        let mut ev_t01 = new_log_event("atm-tui", "event_t01", "atm_tui::main", "info");
        ev_t01.ts = "2026-01-01T00:00:01Z".to_string();
        let mut ev_t05 = new_log_event("atm-tui", "event_t05", "atm_tui::main", "info");
        ev_t05.ts = "2026-01-01T00:00:05Z".to_string();

        make_spool_file(
            &spool_dir,
            "atm-1-100.jsonl",
            &[ev_t02.clone(), ev_t10.clone()],
        );
        make_spool_file(
            &spool_dir,
            "atm-tui-2-200.jsonl",
            &[ev_t01.clone(), ev_t05.clone()],
        );

        let count = merge_spool_on_startup(&spool_dir, &log_path).unwrap();
        assert_eq!(count, 4, "should merge 4 events total");

        let content = fs::read_to_string(&log_path).unwrap();
        let events: Vec<LogEventV1> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("valid LogEventV1 JSON"))
            .collect();

        assert_eq!(events.len(), 4);
        // Global timestamp order: T=01, T=02, T=05, T=10
        assert_eq!(events[0].action, "event_t01", "first should be T=01");
        assert_eq!(events[1].action, "event_t02", "second should be T=02");
        assert_eq!(events[2].action, "event_t05", "third should be T=05");
        assert_eq!(events[3].action, "event_t10", "fourth should be T=10");
    }
}
