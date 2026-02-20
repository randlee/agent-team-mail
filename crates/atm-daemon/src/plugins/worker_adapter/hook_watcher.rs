//! Hook event file watcher for Codex agent lifecycle signals
//!
//! Watches `${ATM_HOME}/.claude/daemon/hooks/events.jsonl` for new hook events
//! appended by the `atm-hook-relay.sh` script (Sprint 10.0). On each file
//! change, reads only new lines from the last-known offset (incremental, no
//! re-reading the full file). Parses JSON lines and routes events to the
//! appropriate trackers:
//!
//! - `agent-turn-complete` → [`AgentStateTracker`] (agent transitions to Idle)
//! - `session-start` → [`SessionRegistry`] (`upsert` with session ID and PID)
//! - `session-end` → [`SessionRegistry`] (`mark_dead` for the agent)
//!
//! ## Event Format
//!
//! Each line of `events.jsonl` is a JSON object:
//!
//! ```json
//! {"type":"agent-turn-complete","agent":"arch-ctm","team":"atm-dev",
//!  "thread-id":"...","turn-id":"...","received_at":"2026-02-16T22:30:00Z"}
//! ```
//!
//! Session lifecycle events carry additional fields:
//!
//! ```json
//! {"type":"session-start","agent":"arch-ctm","sessionId":"uuid","processId":12345}
//! {"type":"session-end","agent":"arch-ctm","sessionId":"uuid"}
//! ```
//!
//! ## Truncation Handling
//!
//! If the stored offset exceeds the current file size (e.g., file was rotated),
//! the offset resets to 0 and the file is read from the beginning.

use super::agent_state::{AgentState, AgentStateTracker};
use crate::daemon::session_registry::SharedSessionRegistry;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Codex `notify` hook event (kebab-case fields per Codex source).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct HookEvent {
    /// Event type: `"agent-turn-complete"`, `"session-start"`, or `"session-end"`.
    #[serde(rename = "type")]
    pub event_type: String,
    /// ATM identity of the agent that fired the hook (e.g., `"arch-ctm"`).
    pub agent: Option<String>,
    /// ATM team name (e.g., `"atm-dev"`).
    pub team: Option<String>,
    /// Codex thread ID.
    pub thread_id: Option<String>,
    /// Codex turn ID.
    pub turn_id: Option<String>,
    /// ISO-8601 timestamp added by the relay script.
    pub received_at: Option<String>,
    /// Claude Code session UUID (present on `session-start` and `session-end`).
    ///
    /// Field name in JSON is `sessionId` (camelCase), but we use `#[serde(rename)]`
    /// because `kebab-case` cannot express camelCase.
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    /// OS process ID of the agent process (present on `session-start`).
    ///
    /// Field name in JSON is `processId` (camelCase).
    #[serde(rename = "processId")]
    pub process_id: Option<u32>,
}

/// Watches `events.jsonl` for new hook events and updates [`AgentStateTracker`]
/// and [`SessionRegistry`].
pub struct HookWatcher {
    /// Path to the `events.jsonl` file.
    path: PathBuf,
    /// Shared state tracker to update on each event.
    state: Arc<Mutex<AgentStateTracker>>,
    /// Shared session registry for session lifecycle events.
    session_registry: Option<SharedSessionRegistry>,
}

impl HookWatcher {
    /// Create a new hook watcher with agent-state tracking only.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to `events.jsonl`
    /// * `state` - Shared agent state tracker
    pub fn new(path: PathBuf, state: Arc<Mutex<AgentStateTracker>>) -> Self {
        Self { path, state, session_registry: None }
    }

    /// Create a new hook watcher that also updates the session registry.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to `events.jsonl`
    /// * `state` - Shared agent state tracker
    /// * `session_registry` - Shared session registry for `session-start`/`session-end` events
    pub fn new_with_session_registry(
        path: PathBuf,
        state: Arc<Mutex<AgentStateTracker>>,
        session_registry: SharedSessionRegistry,
    ) -> Self {
        Self { path, state, session_registry: Some(session_registry) }
    }

    /// Run the watcher until cancellation.
    ///
    /// Watches the parent directory of `events.jsonl`. On file change,
    /// reads new lines from the last-known byte offset and processes each event.
    pub async fn run(self, cancel: CancellationToken) {
        let (tx, mut rx) = mpsc::unbounded_channel::<notify::Event>();

        // Create notify watcher. The callback sends events through an unbounded
        // channel. UnboundedSender::send is safe to call from any thread.
        let tx_clone = tx.clone();
        let watcher_result =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                match res {
                    Ok(event) => {
                        let _ = tx_clone.send(event);
                    }
                    Err(e) => warn!("Hook watcher notify error: {e}"),
                }
            });

        let mut watcher: RecommendedWatcher = match watcher_result {
            Ok(w) => w,
            Err(e) => {
                warn!("Failed to create file watcher for hook events: {e}");
                return;
            }
        };

        // Watch the parent directory (more reliable than watching a specific file
        // that may not yet exist).
        let watch_dir = self.path.parent().unwrap_or(Path::new("."));
        if let Err(e) = watcher.watch(watch_dir, RecursiveMode::NonRecursive) {
            warn!("Failed to watch hook events directory {}: {e}", watch_dir.display());
            return;
        }

        debug!(
            "Hook watcher started: watching {} for changes to {}",
            watch_dir.display(),
            self.path.display()
        );

        let mut offset: u64 = 0;

        // Do an initial read in case events were written before we started watching.
        offset = read_new_events(&self.path, offset, &self.state, self.session_registry.as_ref());

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("Hook watcher shutting down");
                    break;
                }
                Some(event) = rx.recv() => {
                    if should_process_event(&event, &self.path) {
                        offset = read_new_events(&self.path, offset, &self.state, self.session_registry.as_ref());
                    }
                }
            }
        }

        // `watcher` is dropped here, which stops the OS-level watch.
    }
}

/// Returns `true` if this notify event is for (or near) our target file.
fn should_process_event(event: &notify::Event, target: &Path) -> bool {
    // Process on data modify or create; ignore metadata-only changes.
    let is_data_event = matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Modify(notify::event::ModifyKind::Data(_))
            | EventKind::Modify(notify::event::ModifyKind::Any)
            | EventKind::Modify(notify::event::ModifyKind::Other)
    );

    if !is_data_event {
        return false;
    }

    // Check if any of the event paths refer to our target file.
    // Fall back to true if no path info available (conservative).
    if event.paths.is_empty() {
        return true;
    }

    let target_name = target.file_name();
    event.paths.iter().any(|p| {
        // Exact match
        if p == target {
            return true;
        }
        // File name match: handles macOS /var → /private/var symlink differences
        // and other path canonicalization issues across platforms.
        p.file_name().is_some() && p.file_name() == target_name
    })
}

/// Read new lines from `path` starting at `offset`, process each hook event,
/// and return the new offset.
///
/// Handles truncation: if `offset > file_size`, resets to 0.
fn read_new_events(
    path: &Path,
    offset: u64,
    state: &Arc<Mutex<AgentStateTracker>>,
    session_registry: Option<&SharedSessionRegistry>,
) -> u64 {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => {
            // File does not exist yet; stay at current offset.
            return offset;
        }
    };

    let file_size = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return offset,
    };

    // Handle truncation (log rotation or file reset).
    let effective_offset = if offset > file_size {
        debug!(
            "events.jsonl truncated (offset {offset} > size {file_size}), resetting to 0"
        );
        0
    } else {
        offset
    };

    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(effective_offset)).is_err() {
        return offset;
    }

    let mut new_offset = effective_offset;
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(n) => {
                new_offset += n as u64;
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    process_hook_line(trimmed, state, session_registry);
                }
            }
            Err(e) => {
                warn!("Error reading events.jsonl: {e}");
                break;
            }
        }
    }

    new_offset
}

/// Parse and apply a single JSON line from `events.jsonl`.
fn process_hook_line(
    line: &str,
    state: &Arc<Mutex<AgentStateTracker>>,
    session_registry: Option<&SharedSessionRegistry>,
) {
    let event: HookEvent = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(e) => {
            warn!("Malformed hook event JSON (skipping): {e} — line: {line}");
            return;
        }
    };

    apply_hook_event(&event, state, session_registry);
}

/// Apply the semantic effect of a hook event to the state tracker and session
/// registry.
fn apply_hook_event(
    event: &HookEvent,
    state: &Arc<Mutex<AgentStateTracker>>,
    session_registry: Option<&SharedSessionRegistry>,
) {
    match event.event_type.as_str() {
        "agent-turn-complete" => {
            let agent_id = match &event.agent {
                Some(id) => id.clone(),
                None => {
                    warn!("agent-turn-complete event missing 'agent' field, skipping");
                    return;
                }
            };
            debug!(
                "AfterAgent hook received for {agent_id} (turn: {:?})",
                event.turn_id
            );
            let mut tracker = state.lock().unwrap();
            // Transition Launching → Idle (first hook) or Busy → Idle.
            // Any registered state maps to Idle on AfterAgent.
            if tracker.get_state(&agent_id).is_some() {
                tracker.set_state(&agent_id, AgentState::Idle);
            } else {
                // Agent not yet registered — auto-register as Idle.
                debug!("Auto-registering untracked agent {agent_id} as Idle");
                tracker.register_agent(&agent_id);
                tracker.set_state(&agent_id, AgentState::Idle);
            }
        }
        "session-start" => {
            let agent_id = match &event.agent {
                Some(id) => id.clone(),
                None => {
                    warn!("session-start event missing 'agent' field, skipping");
                    return;
                }
            };
            let session_id = match &event.session_id {
                Some(sid) => sid.clone(),
                None => {
                    warn!("session-start event for {agent_id} missing 'sessionId', skipping");
                    return;
                }
            };
            let process_id = event.process_id.unwrap_or(0);
            debug!(
                "SessionStart hook received for {agent_id} (session: {session_id}, pid: {process_id})"
            );
            if let Some(registry) = session_registry {
                registry.lock().unwrap().upsert(&agent_id, &session_id, process_id);
            }
        }
        "session-end" => {
            let agent_id = match &event.agent {
                Some(id) => id.clone(),
                None => {
                    warn!("session-end event missing 'agent' field, skipping");
                    return;
                }
            };
            debug!("SessionEnd hook received for {agent_id}");
            if let Some(registry) = session_registry {
                registry.lock().unwrap().mark_dead(&agent_id);
            }
        }
        unknown => {
            debug!("Unrecognised hook event type '{unknown}', ignoring");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::session_registry::new_session_registry;

    fn make_state() -> Arc<Mutex<AgentStateTracker>> {
        Arc::new(Mutex::new(AgentStateTracker::new()))
    }

    // ── hook event parsing ────────────────────────────────────────────────

    #[test]
    fn test_parse_agent_turn_complete() {
        let json = r#"{"type":"agent-turn-complete","agent":"arch-ctm","team":"atm-dev","thread-id":"t1","turn-id":"42","received_at":"2026-02-16T22:30:00Z"}"#;
        let event: HookEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "agent-turn-complete");
        assert_eq!(event.agent.as_deref(), Some("arch-ctm"));
        assert_eq!(event.team.as_deref(), Some("atm-dev"));
        assert_eq!(event.thread_id.as_deref(), Some("t1"));
        assert_eq!(event.turn_id.as_deref(), Some("42"));
        assert!(event.session_id.is_none());
        assert!(event.process_id.is_none());
    }

    #[test]
    fn test_parse_session_start_event() {
        let json = r#"{"type":"session-start","agent":"arch-ctm","sessionId":"uuid-1234","processId":9876}"#;
        let event: HookEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "session-start");
        assert_eq!(event.agent.as_deref(), Some("arch-ctm"));
        assert_eq!(event.session_id.as_deref(), Some("uuid-1234"));
        assert_eq!(event.process_id, Some(9876));
    }

    #[test]
    fn test_parse_session_end_event() {
        let json = r#"{"type":"session-end","agent":"arch-ctm","sessionId":"uuid-1234"}"#;
        let event: HookEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "session-end");
        assert_eq!(event.agent.as_deref(), Some("arch-ctm"));
        assert_eq!(event.session_id.as_deref(), Some("uuid-1234"));
    }

    #[test]
    fn test_malformed_json_does_not_panic() {
        let state = make_state();
        // Should log a warning and return without panicking.
        process_hook_line("not json at all", &state, None);
        process_hook_line("{broken", &state, None);
        process_hook_line("", &state, None);
        // State should be unchanged.
        assert!(state.lock().unwrap().all_states().is_empty());
    }

    #[test]
    fn test_agent_turn_complete_transitions_to_idle() {
        let state = make_state();
        state.lock().unwrap().register_agent("arch-ctm");
        state
            .lock()
            .unwrap()
            .set_state("arch-ctm", AgentState::Launching);

        let json = r#"{"type":"agent-turn-complete","agent":"arch-ctm","team":"atm-dev"}"#;
        process_hook_line(json, &state, None);

        assert_eq!(
            state.lock().unwrap().get_state("arch-ctm"),
            Some(AgentState::Idle)
        );
    }

    #[test]
    fn test_busy_to_idle_via_hook() {
        let state = make_state();
        state.lock().unwrap().register_agent("arch-ctm");
        state
            .lock()
            .unwrap()
            .set_state("arch-ctm", AgentState::Busy);

        let json = r#"{"type":"agent-turn-complete","agent":"arch-ctm","team":"atm-dev"}"#;
        process_hook_line(json, &state, None);

        assert_eq!(
            state.lock().unwrap().get_state("arch-ctm"),
            Some(AgentState::Idle)
        );
    }

    #[test]
    fn test_auto_register_on_hook_for_unknown_agent() {
        let state = make_state();
        // Agent not pre-registered.
        let json = r#"{"type":"agent-turn-complete","agent":"new-agent","team":"atm-dev"}"#;
        process_hook_line(json, &state, None);

        assert_eq!(
            state.lock().unwrap().get_state("new-agent"),
            Some(AgentState::Idle)
        );
    }

    #[test]
    fn test_missing_agent_field_does_not_panic() {
        let state = make_state();
        // event_type present but agent field missing
        let json = r#"{"type":"agent-turn-complete","team":"atm-dev"}"#;
        process_hook_line(json, &state, None);
        // Nothing should be added to state.
        assert!(state.lock().unwrap().all_states().is_empty());
    }

    #[test]
    fn test_unknown_event_type_ignored() {
        let state = make_state();
        let json = r#"{"type":"after-tool-use","agent":"arch-ctm"}"#;
        process_hook_line(json, &state, None);
        assert!(state.lock().unwrap().all_states().is_empty());
    }

    // ── session-start / session-end events ───────────────────────────────

    #[test]
    fn test_session_start_calls_upsert_on_registry() {
        let state = make_state();
        let registry = new_session_registry();

        let json = r#"{"type":"session-start","agent":"arch-ctm","sessionId":"sess-abc","processId":4242}"#;
        process_hook_line(json, &state, Some(&registry));

        let reg = registry.lock().unwrap();
        let record = reg.query("arch-ctm").expect("arch-ctm should be in registry");
        assert_eq!(record.session_id, "sess-abc");
        assert_eq!(record.process_id, 4242);
        use crate::daemon::session_registry::SessionState;
        assert_eq!(record.state, SessionState::Active);
    }

    #[test]
    fn test_session_end_calls_mark_dead_on_registry() {
        let state = make_state();
        let registry = new_session_registry();

        // First register via session-start
        registry.lock().unwrap().upsert("arch-ctm", "sess-abc", 4242);

        let json = r#"{"type":"session-end","agent":"arch-ctm","sessionId":"sess-abc"}"#;
        process_hook_line(json, &state, Some(&registry));

        let reg = registry.lock().unwrap();
        let record = reg.query("arch-ctm").expect("arch-ctm should be in registry");
        use crate::daemon::session_registry::SessionState;
        assert_eq!(record.state, SessionState::Dead);
    }

    #[test]
    fn test_session_start_without_registry_does_not_panic() {
        let state = make_state();
        // No registry provided — should not panic.
        let json = r#"{"type":"session-start","agent":"arch-ctm","sessionId":"sess-abc","processId":1}"#;
        process_hook_line(json, &state, None);
        // State tracker should not be affected.
        assert!(state.lock().unwrap().all_states().is_empty());
    }

    #[test]
    fn test_session_start_missing_session_id_skips() {
        let state = make_state();
        let registry = new_session_registry();
        let json = r#"{"type":"session-start","agent":"arch-ctm"}"#;
        process_hook_line(json, &state, Some(&registry));
        // Registry should remain empty because sessionId is missing.
        assert!(registry.lock().unwrap().is_empty());
    }

    #[test]
    fn test_session_start_missing_agent_skips() {
        let state = make_state();
        let registry = new_session_registry();
        let json = r#"{"type":"session-start","sessionId":"sess-abc","processId":1}"#;
        process_hook_line(json, &state, Some(&registry));
        assert!(registry.lock().unwrap().is_empty());
    }

    // ── incremental file reading ──────────────────────────────────────────

    #[test]
    fn test_read_new_events_empty_file() {
        let state = make_state();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, b"").unwrap();

        let new_offset = read_new_events(&path, 0, &state, None);
        assert_eq!(new_offset, 0);
    }

    #[test]
    fn test_read_new_events_processes_lines() {
        let state = make_state();
        state.lock().unwrap().register_agent("arch-ctm");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let line = "{\"type\":\"agent-turn-complete\",\"agent\":\"arch-ctm\",\"team\":\"atm-dev\"}\n";
        std::fs::write(&path, line.as_bytes()).unwrap();

        let new_offset = read_new_events(&path, 0, &state, None);
        assert_eq!(new_offset, line.len() as u64);
        assert_eq!(
            state.lock().unwrap().get_state("arch-ctm"),
            Some(AgentState::Idle)
        );
    }

    #[test]
    fn test_read_new_events_incremental() {
        let state = make_state();
        state.lock().unwrap().register_agent("arch-ctm");
        state.lock().unwrap().register_agent("agent-b");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let line1 = "{\"type\":\"agent-turn-complete\",\"agent\":\"arch-ctm\",\"team\":\"atm-dev\"}\n";
        std::fs::write(&path, line1.as_bytes()).unwrap();

        // First read
        let offset1 = read_new_events(&path, 0, &state, None);
        assert_eq!(offset1, line1.len() as u64);
        assert_eq!(
            state.lock().unwrap().get_state("arch-ctm"),
            Some(AgentState::Idle)
        );

        // Append second event
        let line2 = "{\"type\":\"agent-turn-complete\",\"agent\":\"agent-b\",\"team\":\"atm-dev\"}\n";
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, line2.as_bytes()).unwrap();
        drop(file);

        // Second read should only process line2
        let offset2 = read_new_events(&path, offset1, &state, None);
        assert_eq!(offset2, (line1.len() + line2.len()) as u64);
        assert_eq!(
            state.lock().unwrap().get_state("agent-b"),
            Some(AgentState::Idle)
        );
    }

    #[test]
    fn test_read_new_events_handles_truncation() {
        let state = make_state();
        state.lock().unwrap().register_agent("arch-ctm");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let line = "{\"type\":\"agent-turn-complete\",\"agent\":\"arch-ctm\",\"team\":\"atm-dev\"}\n";
        std::fs::write(&path, line.as_bytes()).unwrap();

        // offset beyond file size (simulating truncation)
        let new_offset = read_new_events(&path, 9999, &state, None);
        // Should re-read from 0, process the line, and return correct offset
        assert_eq!(new_offset, line.len() as u64);
        assert_eq!(
            state.lock().unwrap().get_state("arch-ctm"),
            Some(AgentState::Idle)
        );
    }

    #[test]
    fn test_read_new_events_file_not_found() {
        let state = make_state();
        let path = std::path::PathBuf::from("/nonexistent/path/events.jsonl");
        let new_offset = read_new_events(&path, 42, &state, None);
        // Should return the same offset unchanged
        assert_eq!(new_offset, 42);
    }

    #[test]
    fn test_read_new_events_session_start_updates_registry() {
        let state = make_state();
        let registry = new_session_registry();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let line = "{\"type\":\"session-start\",\"agent\":\"arch-ctm\",\"sessionId\":\"sess-xyz\",\"processId\":999}\n";
        std::fs::write(&path, line.as_bytes()).unwrap();

        let new_offset = read_new_events(&path, 0, &state, Some(&registry));
        assert_eq!(new_offset, line.len() as u64);

        let reg = registry.lock().unwrap();
        let record = reg.query("arch-ctm").expect("agent should be in registry");
        assert_eq!(record.session_id, "sess-xyz");
        assert_eq!(record.process_id, 999);
    }
}
