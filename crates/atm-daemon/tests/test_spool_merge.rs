//! Integration tests for [`spool_merge::merge_spool_on_startup`].
//!
//! All tests use `tempfile::TempDir` for full isolation. No global env vars
//! are mutated. No home directory lookups are performed.

use agent_team_mail_core::logging_event::{LogEventV1, new_log_event};
use agent_team_mail_daemon::daemon::spool_merge::merge_spool_on_startup;
use std::fs;
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write_spool_file(dir: &std::path::Path, filename: &str, events: &[LogEventV1]) {
    let path = dir.join(filename);
    let mut content = String::new();
    for event in events {
        content.push_str(&serde_json::to_string(event).unwrap());
        content.push('\n');
    }
    fs::write(path, content).unwrap();
}

fn read_canonical_events(log_path: &std::path::Path) -> Vec<LogEventV1> {
    if !log_path.exists() {
        return vec![];
    }
    fs::read_to_string(log_path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<LogEventV1>(l).expect("valid LogEventV1 JSON"))
        .collect()
}

fn spool_jsonl_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    if !dir.exists() {
        return vec![];
    }
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|x| x.to_str())
                .map(|x| x == "jsonl")
                .unwrap_or(false)
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn test_merge_two_spool_files_six_events() {
    let tmp = TempDir::new().unwrap();
    let spool_dir = tmp.path().join("spool");
    fs::create_dir_all(&spool_dir).unwrap();
    let log_path = tmp.path().join("canonical.jsonl");

    let file1_events = vec![
        new_log_event("atm", "send_message", "atm::cmd", "info"),
        new_log_event("atm", "read_inbox", "atm::cmd", "info"),
        new_log_event("atm", "command_error", "atm::cmd", "error"),
    ];
    let file2_events = vec![
        new_log_event("atm-tui", "tui_start", "atm_tui::main", "info"),
        new_log_event("atm-tui", "tui_tick", "atm_tui::main", "debug"),
        new_log_event("atm-tui", "tui_stop", "atm_tui::main", "info"),
    ];

    write_spool_file(&spool_dir, "atm-101-1000.jsonl", &file1_events);
    write_spool_file(&spool_dir, "atm-tui-202-2000.jsonl", &file2_events);

    let merged = merge_spool_on_startup(&spool_dir, &log_path).unwrap();
    assert_eq!(merged, 6, "should merge 6 events total");

    // Canonical log must have exactly 6 events.
    let events = read_canonical_events(&log_path);
    assert_eq!(events.len(), 6);

    // All must be valid schema-v1 events.
    for event in &events {
        assert_eq!(event.v, 1);
        assert!(!event.ts.is_empty());
        assert!(!event.action.is_empty());
    }

    // Spool directory must be empty (no .jsonl files remain).
    let remaining = spool_jsonl_files(&spool_dir);
    assert!(
        remaining.is_empty(),
        "all spool files should be removed after merge; remaining: {remaining:?}"
    );
}

#[test]
fn test_merge_spool_creates_parent_dirs() {
    let tmp = TempDir::new().unwrap();
    let spool_dir = tmp.path().join("spool");
    fs::create_dir_all(&spool_dir).unwrap();

    // Log path whose parent does not yet exist.
    let log_path = tmp.path().join("sub/dir/canonical.jsonl");

    let event = new_log_event("atm", "daemon_start", "atm_daemon::main", "info");
    write_spool_file(&spool_dir, "atm-1-100.jsonl", &[event]);

    let merged = merge_spool_on_startup(&spool_dir, &log_path).unwrap();
    assert_eq!(merged, 1);
    assert!(log_path.exists(), "canonical log should be created");
}

#[test]
fn test_merge_empty_spool_dir_returns_zero() {
    let tmp = TempDir::new().unwrap();
    let spool_dir = tmp.path().join("spool");
    fs::create_dir_all(&spool_dir).unwrap();
    let log_path = tmp.path().join("canonical.jsonl");

    let merged = merge_spool_on_startup(&spool_dir, &log_path).unwrap();
    assert_eq!(merged, 0);
    assert!(
        !log_path.exists(),
        "log should not be created when nothing to merge"
    );
}

#[test]
fn test_merge_nonexistent_spool_dir_returns_zero() {
    let tmp = TempDir::new().unwrap();
    let spool_dir = tmp.path().join("nonexistent");
    let log_path = tmp.path().join("canonical.jsonl");

    let merged = merge_spool_on_startup(&spool_dir, &log_path).unwrap();
    assert_eq!(merged, 0);
}

#[test]
fn test_merge_idempotent_claiming_file_cleanup() {
    // Simulate a crashed previous daemon: a .claiming file exists but the
    // original .jsonl was already renamed away.
    let tmp = TempDir::new().unwrap();
    let spool_dir = tmp.path().join("spool");
    fs::create_dir_all(&spool_dir).unwrap();
    let log_path = tmp.path().join("canonical.jsonl");

    // Write a stale .claiming file.
    let stale_claiming = spool_dir.join("atm-1-100.claiming");
    let stale_event = new_log_event("atm", "stale_action", "atm::old", "warn");
    let stale_content = format!("{}\n", serde_json::to_string(&stale_event).unwrap());
    fs::write(&stale_claiming, stale_content).unwrap();

    // Write a new normal spool file.
    let new_event = new_log_event("atm", "new_action", "atm::new", "info");
    write_spool_file(&spool_dir, "atm-2-200.jsonl", &[new_event]);

    // Run merge: should process the .jsonl, clean up the stale .claiming.
    let merged = merge_spool_on_startup(&spool_dir, &log_path).unwrap();

    // Only the normal file contributes to the merge count.
    assert_eq!(merged, 1, "only normal spool file should be merged");

    // The stale .claiming file must be removed.
    assert!(
        !stale_claiming.exists(),
        "stale .claiming file should be cleaned up"
    );

    // The canonical log has exactly 1 event.
    let events = read_canonical_events(&log_path);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, "new_action");
}

#[test]
fn test_merge_appends_to_existing_log() {
    let tmp = TempDir::new().unwrap();
    let spool_dir = tmp.path().join("spool");
    fs::create_dir_all(&spool_dir).unwrap();
    let log_path = tmp.path().join("canonical.jsonl");

    // Pre-populate the canonical log with one event.
    let existing_event = new_log_event("atm-daemon", "daemon_start", "atm_daemon::main", "info");
    let existing_line = format!("{}\n", serde_json::to_string(&existing_event).unwrap());
    fs::write(&log_path, existing_line).unwrap();

    // Write a spool file with two events.
    let spool_events = vec![
        new_log_event("atm", "send_a", "atm::cmd", "info"),
        new_log_event("atm", "send_b", "atm::cmd", "info"),
    ];
    write_spool_file(&spool_dir, "atm-1-100.jsonl", &spool_events);

    let merged = merge_spool_on_startup(&spool_dir, &log_path).unwrap();
    assert_eq!(merged, 2);

    // Canonical log should now have 3 events total.
    let events = read_canonical_events(&log_path);
    assert_eq!(events.len(), 3, "existing event + 2 spool events = 3");
}

#[test]
fn test_merge_skips_unparseable_lines() {
    let tmp = TempDir::new().unwrap();
    let spool_dir = tmp.path().join("spool");
    fs::create_dir_all(&spool_dir).unwrap();
    let log_path = tmp.path().join("canonical.jsonl");

    // Write a spool file with 1 good event, 1 corrupt line, 1 good event.
    let good_event = new_log_event("atm", "good_action", "atm::cmd", "info");
    let good_json = serde_json::to_string(&good_event).unwrap();
    let content = format!("{good_json}\nnot valid json at all\n{good_json}\n");
    fs::write(spool_dir.join("atm-1-100.jsonl"), content).unwrap();

    let merged = merge_spool_on_startup(&spool_dir, &log_path).unwrap();
    // Only 2 good events should be counted.
    assert_eq!(merged, 2, "unparseable lines should be skipped");

    let events = read_canonical_events(&log_path);
    assert_eq!(events.len(), 2);
}

#[test]
fn test_merge_writes_to_configured_canonical_log_path() {
    let tmp = TempDir::new().unwrap();
    let spool_dir = agent_team_mail_core::logging_event::configured_spool_dir(tmp.path());
    fs::create_dir_all(&spool_dir).unwrap();
    let log_path = agent_team_mail_core::logging_event::configured_log_path(tmp.path());

    let event = new_log_event("atm", "daemon_start", "atm_daemon::main", "info");
    write_spool_file(&spool_dir, "atm-1-100.jsonl", &[event]);

    let merged = merge_spool_on_startup(&spool_dir, &log_path).unwrap();
    assert_eq!(merged, 1);
    assert!(log_path.exists(), "configured canonical log should be created");
    assert!(
        spool_jsonl_files(&spool_dir).is_empty(),
        "configured spool directory should be drained after merge"
    );
}

#[test]
fn test_merge_events_sorted_by_timestamp() {
    let tmp = TempDir::new().unwrap();
    let spool_dir = tmp.path().join("spool");
    fs::create_dir_all(&spool_dir).unwrap();
    let log_path = tmp.path().join("canonical.jsonl");

    // Build events with explicit timestamps out of order.
    let mut event_early = new_log_event("atm", "early_action", "atm::cmd", "info");
    event_early.ts = "2026-01-01T00:00:01Z".to_string();

    let mut event_late = new_log_event("atm", "late_action", "atm::cmd", "info");
    event_late.ts = "2026-01-01T00:00:10Z".to_string();

    let mut event_middle = new_log_event("atm", "middle_action", "atm::cmd", "info");
    event_middle.ts = "2026-01-01T00:00:05Z".to_string();

    // Write them in non-chronological order in the spool file.
    write_spool_file(
        &spool_dir,
        "atm-1-100.jsonl",
        &[
            event_late.clone(),
            event_early.clone(),
            event_middle.clone(),
        ],
    );

    let merged = merge_spool_on_startup(&spool_dir, &log_path).unwrap();
    assert_eq!(merged, 3);

    let events = read_canonical_events(&log_path);
    assert_eq!(events.len(), 3);

    // Events should be in timestamp order: early, middle, late.
    assert_eq!(events[0].action, "early_action", "first should be earliest");
    assert_eq!(events[1].action, "middle_action", "second should be middle");
    assert_eq!(events[2].action, "late_action", "third should be latest");
}
