//! End-to-end OTel smoke test for daemon log writer.
//!
//! Verifies queue -> writer -> OTel sidecar export preserves required
//! correlation fields without blocking normal local logging.

use agent_team_mail_core::logging_event::new_log_event;
use agent_team_mail_daemon::daemon::{LogWriterConfig, new_log_event_queue, run_log_writer_task};
use sc_observability::OtelRecord;
use serial_test::serial;
use tempfile::TempDir;
use tokio::time::{Duration, sleep, timeout};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[serial]
async fn otel_smoke_writer_exports_required_correlation_fields() {
    // SAFETY: serial test; scoped env mutation.
    unsafe {
        std::env::set_var("ATM_OTEL_ENABLED", "true");
    }

    let temp = TempDir::new().expect("temp dir");
    let log_path = temp.path().join("atm.log.jsonl");
    let otel_path = temp.path().join("atm.log.otel.jsonl");

    let queue = new_log_event_queue();
    let cancel = CancellationToken::new();
    let writer_handle = tokio::spawn(run_log_writer_task(
        queue.clone(),
        LogWriterConfig {
            log_path: log_path.clone(),
            max_bytes: 50 * 1024 * 1024,
            max_files: 5,
            flush_interval_ms: 10,
        },
        cancel.clone(),
    ));

    let mut event = new_log_event("atm", "send", "atm::send", "info");
    event.team = Some("atm-dev".to_string());
    event.agent = Some("arch-ctm".to_string());
    event.runtime = Some("codex".to_string());
    event.session_id = Some("sess-abc".to_string());
    event.trace_id = Some("trace-abc".to_string());
    event.span_id = Some("span-abc".to_string());
    queue.lock().await.push(event);

    let exported_line = timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(raw) = std::fs::read_to_string(&otel_path) {
                let mut lines = raw.lines().map(str::trim).filter(|line| !line.is_empty());
                if let Some(first) = lines.next() {
                    return first.to_string();
                }
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("otel export should appear");

    cancel.cancel();
    timeout(Duration::from_secs(5), writer_handle)
        .await
        .expect("writer task should stop")
        .expect("writer join should succeed");

    let exported: OtelRecord = serde_json::from_str(&exported_line).expect("valid otel JSON line");
    assert_eq!(exported.name, "send");
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

    // Local structured logging must still be written.
    let canonical = std::fs::read_to_string(log_path).expect("canonical log should exist");
    assert!(
        canonical.lines().any(|line| !line.trim().is_empty()),
        "canonical JSONL should contain at least one event"
    );

    // SAFETY: cleanup after test.
    unsafe {
        std::env::remove_var("ATM_OTEL_ENABLED");
    }
}
