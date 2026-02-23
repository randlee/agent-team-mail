//! Integration tests for unified logging bridge in [`event_log`].
//!
//! These tests verify the `ATM_LOG_BRIDGE` env var routing without requiring a
//! running daemon. All tests use `tempfile::TempDir` for isolation and
//! `serial_test` to prevent env-var races.

use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use serial_test::serial;
use std::fs;
use tempfile::TempDir;

/// Helper to create a temp `ATM_LOG_FILE` and return (TempDir, path).
fn setup_log_file() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().expect("temp dir");
    let log_path = tmp.path().join("events.jsonl");
    (tmp, log_path)
}

#[test]
#[serial]
fn test_bridge_dual_writes_legacy_file() {
    let (tmp, log_path) = setup_log_file();
    unsafe {
        std::env::set_var("ATM_LOG_FILE", &log_path);
        std::env::set_var("ATM_LOG_BRIDGE", "dual");
        std::env::remove_var("CLAUDE_SESSION_ID");
    }

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "test_dual",
        team: Some("atm-dev".to_string()),
        ..Default::default()
    });

    let content = fs::read_to_string(&log_path).expect("log file should exist");
    assert!(!content.is_empty(), "dual mode should write legacy JSONL");

    // At minimum a header and an event line.
    let lines: Vec<&str> = content.lines().collect();
    assert!(
        lines.len() >= 2,
        "expected header + event; got {} lines",
        lines.len()
    );

    let event_line: serde_json::Value =
        serde_json::from_str(lines[1]).expect("event line should be valid JSON");
    assert_eq!(event_line["act"], "test_dual");

    // Cleanup env.
    unsafe {
        std::env::remove_var("ATM_LOG_FILE");
        std::env::remove_var("ATM_LOG_BRIDGE");
    }
    drop(tmp);
}

#[test]
#[serial]
fn test_bridge_legacy_only_writes_legacy_file() {
    let (tmp, log_path) = setup_log_file();
    unsafe {
        std::env::set_var("ATM_LOG_FILE", &log_path);
        std::env::set_var("ATM_LOG_BRIDGE", "legacy_only");
        std::env::remove_var("CLAUDE_SESSION_ID");
    }

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "test_legacy_only",
        ..Default::default()
    });

    let content = fs::read_to_string(&log_path).expect("log file should exist");
    assert!(
        !content.is_empty(),
        "legacy_only mode should write legacy JSONL"
    );

    // Cleanup env.
    unsafe {
        std::env::remove_var("ATM_LOG_FILE");
        std::env::remove_var("ATM_LOG_BRIDGE");
    }
    drop(tmp);
}

#[test]
#[serial]
fn test_bridge_unified_only_skips_legacy_file() {
    let (tmp, log_path) = setup_log_file();
    unsafe {
        std::env::set_var("ATM_LOG_FILE", &log_path);
        std::env::set_var("ATM_LOG_BRIDGE", "unified_only");
        std::env::remove_var("CLAUDE_SESSION_ID");
    }

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "test_unified_only",
        ..Default::default()
    });

    // With unified_only the legacy JSONL should NOT be written.
    // The file may not exist at all (no header either).
    let exists = log_path.exists();
    if exists {
        let content = fs::read_to_string(&log_path).unwrap();
        // If the file exists it should be empty (no event lines written).
        assert!(
            content.is_empty(),
            "unified_only mode must not write to legacy JSONL; got: {content}"
        );
    }
    // (file not existing is also correct)

    // Cleanup env.
    unsafe {
        std::env::remove_var("ATM_LOG_FILE");
        std::env::remove_var("ATM_LOG_BRIDGE");
    }
    drop(tmp);
}

#[test]
#[serial]
fn test_bridge_default_is_dual() {
    // When ATM_LOG_BRIDGE is unset the default must be "dual".
    let (tmp, log_path) = setup_log_file();
    unsafe {
        std::env::set_var("ATM_LOG_FILE", &log_path);
        std::env::remove_var("ATM_LOG_BRIDGE");
        std::env::remove_var("CLAUDE_SESSION_ID");
    }

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "test_default_bridge",
        ..Default::default()
    });

    // Legacy JSONL should be written in default (dual) mode.
    let content = fs::read_to_string(&log_path).expect("log file should exist");
    assert!(
        !content.is_empty(),
        "default bridge mode should write legacy JSONL"
    );

    // Cleanup env.
    unsafe {
        std::env::remove_var("ATM_LOG_FILE");
    }
    drop(tmp);
}

#[test]
#[serial]
fn test_emit_empty_action_is_noop() {
    let (tmp, log_path) = setup_log_file();
    unsafe {
        std::env::set_var("ATM_LOG_FILE", &log_path);
        std::env::set_var("ATM_LOG_BRIDGE", "dual");
    }

    // Empty action should be a no-op; must not panic.
    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "",
        ..Default::default()
    });

    // File should not exist (nothing written).
    assert!(
        !log_path.exists(),
        "empty action should produce no output"
    );

    unsafe {
        std::env::remove_var("ATM_LOG_FILE");
        std::env::remove_var("ATM_LOG_BRIDGE");
    }
    drop(tmp);
}
