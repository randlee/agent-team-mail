//! End-to-end OTel smoke tests for daemon log writer fail-open behavior.

#[path = "../../atm/tests/support/env_guard.rs"]
mod env_guard;

use agent_team_mail_core::logging_event::{LogEventV1, new_log_event};
use agent_team_mail_daemon::daemon::{
    LogWriterConfig, new_log_event_queue, run_log_writer_task, spool_merge::merge_spool_on_startup,
};
use env_guard::EnvGuard;
use sc_observability::OtelRecord;
use serial_test::serial;
use std::time::Instant;
use tempfile::TempDir;
use tokio::time::{Duration, sleep, timeout};
use tokio_util::sync::CancellationToken;

fn build_event(action: &str) -> LogEventV1 {
    let mut event = new_log_event("atm", action, "atm::send", "info");
    event.team = Some("atm-dev".to_string());
    event.agent = Some("arch-ctm".to_string());
    event.runtime = Some("codex".to_string());
    event.session_id = Some("sess-abc".to_string());
    event.trace_id = Some("trace-abc".to_string());
    event.span_id = Some("span-abc".to_string());
    event
}

async fn start_writer(
    log_path: &std::path::Path,
    max_bytes: u64,
) -> (
    agent_team_mail_daemon::daemon::LogEventQueue,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let queue = new_log_event_queue();
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(run_log_writer_task(
        queue.clone(),
        LogWriterConfig {
            log_path: log_path.to_path_buf(),
            max_bytes,
            max_files: 5,
            flush_interval_ms: 10,
        },
        cancel.clone(),
    ));
    (queue, cancel, handle)
}

async fn wait_for_non_empty_file(path: &std::path::Path) {
    timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(raw) = std::fs::read_to_string(path)
                && raw.lines().any(|line| !line.trim().is_empty())
            {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("file should be populated");
}

async fn stop_writer(cancel: CancellationToken, handle: tokio::task::JoinHandle<()>) {
    cancel.cancel();
    timeout(Duration::from_secs(5), handle)
        .await
        .expect("writer task should stop")
        .expect("writer join should succeed");
}

#[tokio::test]
#[serial]
async fn otel_smoke_writer_exports_reachable_with_runtime_and_subagent_id() {
    let _otel_guard = EnvGuard::set("ATM_OTEL_ENABLED", "true");

    let temp = TempDir::new().expect("temp dir");
    let log_path = temp.path().join("atm.log.jsonl");
    let otel_path = temp.path().join("atm.log.otel.jsonl");
    let (queue, cancel, handle) = start_writer(&log_path, 50 * 1024 * 1024).await;

    let mut event = build_event("subagent.run");
    event.subagent_id = Some("subagent-42".to_string());
    queue.lock().await.push(event);

    wait_for_non_empty_file(&otel_path).await;
    stop_writer(cancel, handle).await;

    let exported_line = std::fs::read_to_string(&otel_path)
        .expect("otel sidecar should exist")
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .expect("otel output should contain a line")
        .to_string();
    let exported: OtelRecord = serde_json::from_str(&exported_line).expect("valid otel JSON line");

    assert_eq!(exported.name, "subagent.run");
    assert_eq!(exported.trace_id.as_deref(), Some("trace-abc"));
    assert_eq!(exported.span_id.as_deref(), Some("span-abc"));
    assert_eq!(
        exported.attributes.get("team").and_then(|v| v.as_str()),
        Some("atm-dev")
    );
    assert_eq!(
        exported.attributes.get("agent").and_then(|v| v.as_str()),
        Some("arch-ctm")
    );
    assert_eq!(
        exported.attributes.get("runtime").and_then(|v| v.as_str()),
        Some("codex")
    );
    assert_eq!(
        exported
            .attributes
            .get("session_id")
            .and_then(|v| v.as_str()),
        Some("sess-abc")
    );
    assert_eq!(
        exported
            .attributes
            .get("subagent_id")
            .and_then(|v| v.as_str()),
        Some("subagent-42")
    );

    let canonical = std::fs::read_to_string(log_path).expect("canonical log should exist");
    assert!(
        canonical.lines().any(|line| !line.trim().is_empty()),
        "canonical JSONL should contain at least one event"
    );

}

#[tokio::test]
#[serial]
async fn otel_smoke_writer_fail_open_when_export_path_unreachable() {
    let _otel_guard = EnvGuard::set("ATM_OTEL_ENABLED", "true");

    let temp = TempDir::new().expect("temp dir");
    let log_path = temp.path().join("atm.log.jsonl");
    let otel_path = temp.path().join("atm.log.otel.jsonl");
    std::fs::create_dir_all(&otel_path).expect("create blocking otel directory");

    let (queue, cancel, handle) = start_writer(&log_path, 50 * 1024 * 1024).await;
    queue.lock().await.push(build_event("send"));

    let start = Instant::now();
    wait_for_non_empty_file(&log_path).await;
    stop_writer(cancel, handle).await;
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "writer should not block when exporter path is unreachable"
    );
    assert!(
        otel_path.is_dir(),
        "otel path should remain the blocking directory in outage simulation"
    );

}

#[tokio::test]
#[serial]
async fn otel_outage_rotation_and_spool_merge_smoke() {
    let _otel_guard = EnvGuard::set("ATM_OTEL_ENABLED", "true");

    let temp = TempDir::new().expect("temp dir");
    let log_path = temp.path().join("atm.log.jsonl");
    let otel_path = temp.path().join("atm.log.otel.jsonl");
    std::fs::create_dir_all(&otel_path).expect("create blocking otel directory");
    std::fs::write(&log_path, "x".repeat(400)).expect("seed oversized log for rotation");

    let (queue, cancel, handle) = start_writer(&log_path, 350).await;
    let mut event = build_event("send");
    event.fields.insert(
        "payload".to_string(),
        serde_json::Value::String(format!("rotation-payload-{}", "x".repeat(64))),
    );
    queue.lock().await.push(event);
    wait_for_non_empty_file(&log_path).await;
    stop_writer(cancel, handle).await;

    let rotation_path = temp.path().join("atm.log.jsonl.1");
    assert!(
        rotation_path.exists(),
        "rotation file should exist under exporter outage"
    );

    let spool_dir = temp.path().join("spool");
    std::fs::create_dir_all(&spool_dir).expect("create spool dir");
    let mut spool_event = build_event("send");
    spool_event.fields.insert(
        "spool".to_string(),
        serde_json::Value::String("merge-check".to_string()),
    );
    let spool_file = spool_dir.join("atm-123-1000.jsonl");
    let line = serde_json::to_string(&spool_event).expect("serialize spool event");
    std::fs::write(&spool_file, format!("{line}\n")).expect("write spool file");

    let merged = merge_spool_on_startup(&spool_dir, &log_path).expect("spool merge should succeed");
    assert_eq!(merged, 1, "spool merge should process one event");
    assert!(
        !spool_file.exists(),
        "source spool file should be removed after successful merge"
    );

}
