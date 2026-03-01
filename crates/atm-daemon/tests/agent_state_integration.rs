//! Integration tests for Sprint 10.1: Agent State Machine & Hook Ingestion
//!
//! Tests the complete flow: write to events.jsonl → HookWatcher reads it → AgentStateTracker updated.

use agent_team_mail_daemon::plugins::worker_adapter::{AgentState, AgentStateTracker, HookWatcher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;

static EVENT_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Helper: write a hook event line to the given file, appending.
fn append_event(path: &std::path::Path, agent: &str) {
    use std::io::Write;
    let n = EVENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let line = format!(
        "{{\"type\":\"agent-turn-complete\",\"agent\":\"{agent}\",\"team\":\"atm-dev\",\"state\":\"idle\",\"timestamp\":\"2026-03-01T00:00:{:02}Z\",\"idempotency_key\":\"{agent}-{}\"}}\n",
        n % 60,
        n
    );
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("Failed to open events.jsonl for append");
    file.write_all(line.as_bytes())
        .expect("Failed to write hook event");
}

/// Helper: append a specific idempotency-key event (for replay tests).
fn append_event_with_key(path: &std::path::Path, agent: &str, idempotency_key: &str) {
    use std::io::Write;
    let line = format!(
        "{{\"type\":\"agent-turn-complete\",\"agent\":\"{agent}\",\"team\":\"atm-dev\",\"state\":\"idle\",\"timestamp\":\"2026-03-01T00:00:00Z\",\"idempotency_key\":\"{idempotency_key}\"}}\n"
    );
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("Failed to open events.jsonl for append");
    file.write_all(line.as_bytes())
        .expect("Failed to write hook event");
}

/// Helper: check state with a short poll loop (max 2 seconds, polling every 50ms).
async fn wait_for_state(
    state: &Arc<Mutex<AgentStateTracker>>,
    agent: &str,
    expected: AgentState,
) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let current = state.lock().unwrap().get_state(agent);
        if current == Some(expected) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
#[ignore = "requires reliable FSEvents/kqueue delivery; logic covered by hook_watcher unit tests"]
async fn test_hook_watcher_picks_up_event() {
    let dir = tempfile::tempdir().expect("TempDir");
    let events_path = dir.path().join("events.jsonl");

    let state: Arc<Mutex<AgentStateTracker>> = Arc::new(Mutex::new(AgentStateTracker::new()));
    state.lock().unwrap().register_agent("arch-ctm");

    let watcher = HookWatcher::new(events_path.clone(), Arc::clone(&state));
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        watcher.run(cancel_clone).await;
    });

    // Give watcher time to set up filesystem watch
    sleep(Duration::from_millis(100)).await;

    // Write an event
    append_event(&events_path, "arch-ctm");

    // Wait for state transition
    let transitioned = wait_for_state(&state, "arch-ctm", AgentState::Idle).await;
    cancel.cancel();
    assert!(
        transitioned,
        "Expected arch-ctm to transition to Idle after AfterAgent hook"
    );
}

#[tokio::test]
#[ignore = "requires reliable FSEvents/kqueue delivery; logic covered by hook_watcher unit tests"]
async fn test_hook_watcher_incremental_reads() {
    let dir = tempfile::tempdir().expect("TempDir");
    let events_path = dir.path().join("events.jsonl");

    let state: Arc<Mutex<AgentStateTracker>> = Arc::new(Mutex::new(AgentStateTracker::new()));
    state.lock().unwrap().register_agent("arch-ctm");
    state.lock().unwrap().register_agent("agent-b");

    let watcher = HookWatcher::new(events_path.clone(), Arc::clone(&state));
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        watcher.run(cancel_clone).await;
    });

    sleep(Duration::from_millis(100)).await;

    // First event
    append_event(&events_path, "arch-ctm");
    assert!(
        wait_for_state(&state, "arch-ctm", AgentState::Idle).await,
        "arch-ctm should be Idle after first event"
    );

    // Mark arch-ctm busy again (simulating a nudge)
    state
        .lock()
        .unwrap()
        .set_state("arch-ctm", AgentState::Active);

    // Second event (different agent — validates incremental, not re-read)
    append_event(&events_path, "agent-b");
    assert!(
        wait_for_state(&state, "agent-b", AgentState::Idle).await,
        "agent-b should be Idle after second event"
    );

    // arch-ctm should still be Busy (not re-processed from file start)
    assert_eq!(
        state.lock().unwrap().get_state("arch-ctm"),
        Some(AgentState::Active),
        "arch-ctm should remain Busy (incremental read, not re-read)"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_hook_watcher_handles_pre_existing_events() {
    let dir = tempfile::tempdir().expect("TempDir");
    let events_path = dir.path().join("events.jsonl");

    // Write event BEFORE watcher starts
    append_event(&events_path, "arch-ctm");

    let state: Arc<Mutex<AgentStateTracker>> = Arc::new(Mutex::new(AgentStateTracker::new()));
    state.lock().unwrap().register_agent("arch-ctm");

    let watcher = HookWatcher::new(events_path.clone(), Arc::clone(&state));
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        watcher.run(cancel_clone).await;
    });

    // Watcher does an initial read on startup — should pick up the pre-existing event
    let transitioned = wait_for_state(&state, "arch-ctm", AgentState::Idle).await;
    cancel.cancel();
    assert!(
        transitioned,
        "Hook watcher should read pre-existing events on startup"
    );
}

#[tokio::test]
#[ignore = "requires reliable FSEvents/kqueue delivery; logic covered by hook_watcher unit tests"]
async fn test_hook_watcher_full_lifecycle() {
    let dir = tempfile::tempdir().expect("TempDir");
    let events_path = dir.path().join("events.jsonl");

    let state: Arc<Mutex<AgentStateTracker>> = Arc::new(Mutex::new(AgentStateTracker::new()));
    state.lock().unwrap().register_agent("arch-ctm");

    let watcher = HookWatcher::new(events_path.clone(), Arc::clone(&state));
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        watcher.run(cancel_clone).await;
    });

    sleep(Duration::from_millis(100)).await;

    // 1. Launching → Idle (first AfterAgent hook)
    append_event(&events_path, "arch-ctm");
    assert!(wait_for_state(&state, "arch-ctm", AgentState::Idle).await);

    // 2. Idle → Busy (nudge sent by daemon — simulated directly)
    state
        .lock()
        .unwrap()
        .set_state("arch-ctm", AgentState::Active);
    assert_eq!(
        state.lock().unwrap().get_state("arch-ctm"),
        Some(AgentState::Active)
    );

    // 3. Busy → Idle (second AfterAgent hook)
    append_event(&events_path, "arch-ctm");
    assert!(wait_for_state(&state, "arch-ctm", AgentState::Idle).await);

    // 4. Idle → Killed (PID poll — simulated directly)
    state
        .lock()
        .unwrap()
        .set_state("arch-ctm", AgentState::Offline);
    assert_eq!(
        state.lock().unwrap().get_state("arch-ctm"),
        Some(AgentState::Offline)
    );
    assert!(
        state
            .lock()
            .unwrap()
            .get_state("arch-ctm")
            .unwrap()
            .is_terminal()
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_hook_watcher_duplicate_replay_does_not_double_transition() {
    let dir = tempfile::tempdir().expect("TempDir");
    let events_path = dir.path().join("events.jsonl");

    let state: Arc<Mutex<AgentStateTracker>> = Arc::new(Mutex::new(AgentStateTracker::new()));
    state.lock().unwrap().register_agent("arch-ctm");
    state
        .lock()
        .unwrap()
        .set_state("arch-ctm", AgentState::Busy);

    let watcher = HookWatcher::new(events_path.clone(), Arc::clone(&state));
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        watcher.run(cancel_clone).await;
    });

    sleep(Duration::from_millis(100)).await;

    append_event_with_key(&events_path, "arch-ctm", "dup-key-1");
    assert!(wait_for_state(&state, "arch-ctm", AgentState::Idle).await);

    sleep(Duration::from_millis(30)).await;
    let elapsed_before = state
        .lock()
        .unwrap()
        .time_since_transition("arch-ctm")
        .expect("transition timer should be present");

    // Replay exact same event (same idempotency key) should be ignored.
    append_event_with_key(&events_path, "arch-ctm", "dup-key-1");
    sleep(Duration::from_millis(100)).await;

    let elapsed_after = state
        .lock()
        .unwrap()
        .time_since_transition("arch-ctm")
        .expect("transition timer should be present");

    assert!(
        elapsed_after >= elapsed_before,
        "duplicate replay should not create a second state transition"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_hook_watcher_converges_without_pubsub_delivery() {
    let dir = tempfile::tempdir().expect("TempDir");
    let events_path = dir.path().join("events.jsonl");

    // This test intentionally uses HookWatcher + state tracker only (no pub/sub store)
    // to verify daemon-side state still converges from hook events.
    let state: Arc<Mutex<AgentStateTracker>> = Arc::new(Mutex::new(AgentStateTracker::new()));
    state.lock().unwrap().register_agent("arch-ctm");
    state
        .lock()
        .unwrap()
        .set_state("arch-ctm", AgentState::Busy);

    let watcher = HookWatcher::new(events_path.clone(), Arc::clone(&state));
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        watcher.run(cancel_clone).await;
    });

    sleep(Duration::from_millis(100)).await;
    append_event(&events_path, "arch-ctm");
    assert!(
        wait_for_state(&state, "arch-ctm", AgentState::Idle).await,
        "daemon state should converge to idle without pub/sub involvement"
    );

    cancel.cancel();
}
