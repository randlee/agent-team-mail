//! Hook event file watcher for Codex agent lifecycle signals
//!
//! Watches `${ATM_HOME}/.atm/daemon/hooks/events.jsonl` for new hook events
//! appended by the `atm-hook-relay.sh` script (Sprint 10.0). On each file
//! change, reads only new lines from the last-known offset (incremental, no
//! re-reading the full file). Parses JSON lines and routes events to the
//! appropriate trackers:
//!
//! - `agent-turn-complete` → [`AgentStateTracker`] (agent transitions to Idle)
//! - `session-start` → [`SessionRegistry`] (`upsert` with session ID and PID)
//! - `session-end` → [`SessionRegistry`] (session-scoped dead-mark)
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
use crate::daemon::session_registry::{MarkDeadForSessionOutcome, SharedSessionRegistry};
use agent_team_mail_core::io::atomic::atomic_swap;
use agent_team_mail_core::io::lock::acquire_lock;
use agent_team_mail_core::schema::TeamConfig;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
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
    /// Codex internal thread/conversation handle.
    ///
    /// **MCP-internal adapter field only.** This field parses the `thread-id` key
    /// from Codex hook relay events (`events.jsonl`). It is NOT part of the public
    /// ATM TUI or control protocol API — it is an adapter concern for the MCP layer.
    /// The public TUI control API uses `session_id` + `agent_id` for routing.
    pub thread_id: Option<String>,
    /// Codex turn ID.
    pub turn_id: Option<String>,
    /// ISO-8601 timestamp added by the relay script.
    pub received_at: Option<String>,
    /// Canonical availability state for signaling contract payloads.
    ///
    /// Expected value for AfterAgent lifecycle updates is `"idle"`.
    pub state: Option<String>,
    /// Canonical availability timestamp (ISO-8601).
    ///
    /// When absent, `received_at` is used as the timestamp source.
    pub timestamp: Option<String>,
    /// Stable idempotency key for availability dedup.
    pub idempotency_key: Option<String>,
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

#[derive(Debug, Clone)]
struct AvailabilitySignal {
    agent: String,
    team: String,
    state: String,
    timestamp: String,
    idempotency_key: String,
}

#[derive(Debug, Default)]
struct AvailabilityDeduper {
    seen_keys: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TeamMembership {
    Member,
    NonMember,
    Unknown,
}

impl AvailabilityDeduper {
    fn should_process(&mut self, key: &str) -> bool {
        self.seen_keys.insert(key.to_string())
    }
}

impl HookEvent {
    fn normalized_availability_signal(&self) -> Option<AvailabilitySignal> {
        if self.event_type != "agent-turn-complete" {
            return None;
        }

        let agent = self.agent.as_ref()?.trim();
        let team = self.team.as_ref()?.trim();
        if agent.is_empty() || team.is_empty() {
            return None;
        }

        let state = self
            .state
            .as_deref()
            .unwrap_or("idle")
            .trim()
            .to_ascii_lowercase();

        let timestamp = self
            .timestamp
            .as_ref()
            .or(self.received_at.as_ref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())?;

        // Backward-compatible derivation for older relays that do not yet send
        // idempotency_key explicitly.
        let idempotency_key = self
            .idempotency_key
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                let turn = self.turn_id.as_deref().unwrap_or("no-turn");
                format!("{}:{}:{}", team, agent, turn)
            });

        Some(AvailabilitySignal {
            agent: agent.to_string(),
            team: team.to_string(),
            state,
            timestamp,
            idempotency_key,
        })
    }
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
    /// Claude root directory (`.claude/` inside ATM home).
    ///
    /// When set, `session-start` events automatically update the matching
    /// member's `sessionId` field in the team config files found under
    /// `{claude_root}/teams/`.  This is best-effort: errors are logged at
    /// `debug` level and never abort event processing.
    claude_root: Option<PathBuf>,
}

impl HookWatcher {
    /// Create a new hook watcher with agent-state tracking only.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to `events.jsonl`
    /// * `state` - Shared agent state tracker
    pub fn new(path: PathBuf, state: Arc<Mutex<AgentStateTracker>>) -> Self {
        Self {
            path,
            state,
            session_registry: None,
            claude_root: None,
        }
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
        Self {
            path,
            state,
            session_registry: Some(session_registry),
            claude_root: None,
        }
    }

    /// Attach a claude root path for automatic `session_id` updates in team
    /// config files on `session-start` events.
    ///
    /// This is the `.claude/` directory inside the ATM home directory.
    pub fn with_claude_root(mut self, claude_root: PathBuf) -> Self {
        self.claude_root = Some(claude_root);
        self
    }

    /// Run the watcher until cancellation.
    ///
    /// Watches the parent directory of `events.jsonl`. On file change,
    /// reads new lines from the last-known byte offset and processes each event.
    pub async fn run(self, cancel: CancellationToken) {
        let (tx, mut rx) = mpsc::unbounded_channel::<notify::Event>();
        let mut availability_deduper = AvailabilityDeduper::default();
        let mut reconcile_tick = tokio::time::interval(std::time::Duration::from_millis(200));

        // Create notify watcher. The callback sends events through an unbounded
        // channel. UnboundedSender::send is safe to call from any thread.
        let tx_clone = tx.clone();
        let watcher_result =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    let _ = tx_clone.send(event);
                }
                Err(e) => warn!("Hook watcher notify error: {e}"),
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
            warn!(
                "Failed to watch hook events directory {}: {e}",
                watch_dir.display()
            );
            return;
        }

        debug!(
            "Hook watcher started: watching {} for changes to {}",
            watch_dir.display(),
            self.path.display()
        );

        let mut offset: u64 = 0;

        // Do an initial read in case events were written before we started watching.
        offset = read_new_events(
            &self.path,
            offset,
            &self.state,
            self.session_registry.as_ref(),
            self.claude_root.as_deref(),
            &mut availability_deduper,
        );

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("Hook watcher shutting down");
                    break;
                }
                Some(event) = rx.recv() => {
                    if should_process_event(&event, &self.path) {
                        offset = read_new_events(
                            &self.path,
                            offset,
                            &self.state,
                            self.session_registry.as_ref(),
                            self.claude_root.as_deref(),
                            &mut availability_deduper,
                        );
                    }
                }
                _ = reconcile_tick.tick() => {
                    // Polling fallback: converge state even if a filesystem
                    // notification is dropped by the OS watcher.
                    offset = read_new_events(
                        &self.path,
                        offset,
                        &self.state,
                        self.session_registry.as_ref(),
                        self.claude_root.as_deref(),
                        &mut availability_deduper,
                    );
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
    claude_root: Option<&Path>,
    availability_deduper: &mut AvailabilityDeduper,
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
        debug!("events.jsonl truncated (offset {offset} > size {file_size}), resetting to 0");
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
                    process_hook_line(
                        trimmed,
                        state,
                        session_registry,
                        claude_root,
                        availability_deduper,
                    );
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
    claude_root: Option<&Path>,
    availability_deduper: &mut AvailabilityDeduper,
) {
    let event: HookEvent = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(e) => {
            warn!("Malformed hook event JSON (skipping): {e} — line: {line}");
            return;
        }
    };

    apply_hook_event(
        &event,
        state,
        session_registry,
        claude_root,
        availability_deduper,
    );
}

/// Apply the semantic effect of a hook event to the state tracker and session
/// registry.
///
/// When `claude_root` is provided and the event is `session-start`, this
/// function additionally updates the matching member's `sessionId` field in
/// all team config files found under `{claude_root}/teams/`.  The update is
/// best-effort: any I/O errors are logged at `debug` level and never abort
/// event processing.
fn apply_hook_event(
    event: &HookEvent,
    state: &Arc<Mutex<AgentStateTracker>>,
    session_registry: Option<&SharedSessionRegistry>,
    claude_root: Option<&Path>,
    availability_deduper: &mut AvailabilityDeduper,
) {
    match event.event_type.as_str() {
        "agent-turn-complete" => {
            let Some(signal) = event.normalized_availability_signal() else {
                warn!("agent-turn-complete event missing required availability fields, skipping");
                return;
            };
            let membership =
                classify_team_membership(claude_root, Some(signal.team.as_str()), &signal.agent);
            if matches!(membership, TeamMembership::NonMember) {
                debug!(
                    "Skipping transient availability signal for non-member {}/{}",
                    signal.team, signal.agent
                );
                return;
            }
            if signal.state != "idle" {
                debug!(
                    "Skipping availability event with unsupported state '{}' for {}/{}",
                    signal.state, signal.team, signal.agent
                );
                return;
            }
            if !availability_deduper.should_process(&signal.idempotency_key) {
                debug!(
                    "Skipping duplicate availability event for {}/{} key={}",
                    signal.team, signal.agent, signal.idempotency_key
                );
                return;
            }
            let agent_id = signal.agent;
            debug!(
                "AfterAgent hook received for {agent_id} (turn: {:?}, ts: {}, key: {})",
                event.turn_id, signal.timestamp, signal.idempotency_key
            );
            let mut tracker = state.lock().unwrap();
            // Transition Launching → Idle (first hook) or Busy → Idle.
            // Any registered state maps to Idle on AfterAgent.
            if tracker.get_state(&agent_id).is_some() {
                tracker.set_state_with_context(
                    &agent_id,
                    AgentState::Idle,
                    "agent-turn-complete hook",
                    "hook_watcher",
                );
            } else {
                match membership {
                    TeamMembership::Member => {
                        // Team member with no tracker row yet — bootstrap to Idle.
                        debug!("Bootstrapping tracked team member {agent_id} as Idle");
                        tracker.register_agent(&agent_id);
                        tracker.set_state_with_context(
                            &agent_id,
                            AgentState::Idle,
                            "agent-turn-complete hook (team-member bootstrap)",
                            "hook_watcher",
                        );
                    }
                    TeamMembership::Unknown => {
                        // Preserve existing fail-open behavior when membership cannot be resolved.
                        debug!(
                            "Auto-registering untracked agent {agent_id} as Idle (membership unknown)"
                        );
                        tracker.register_agent(&agent_id);
                        tracker.set_state_with_context(
                            &agent_id,
                            AgentState::Idle,
                            "agent-turn-complete hook (auto-register, membership unknown)",
                            "hook_watcher",
                        );
                    }
                    TeamMembership::NonMember => {
                        // Early return above handles this case.
                    }
                }
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
            let membership =
                classify_team_membership(claude_root, event.team.as_deref(), &agent_id);
            if matches!(membership, TeamMembership::NonMember) {
                if let Some(team) = event.team.as_deref() {
                    debug!(
                        "Skipping transient session-start for non-member {}@{}",
                        agent_id, team
                    );
                } else {
                    debug!(
                        "Skipping transient session-start for non-member {}",
                        agent_id
                    );
                }
                return;
            }
            if let Some(registry) = session_registry {
                let mut reg = registry.lock().unwrap();
                if let Some(team) = event.team.as_deref() {
                    reg.upsert_for_team(team, &agent_id, &session_id, process_id);
                } else {
                    reg.upsert(&agent_id, &session_id, process_id);
                }
            }

            // Best-effort: auto-update session_id on matching external members
            // in the target team config under claude_root/teams/<team>/.
            if let Some(root) = claude_root {
                if let Some(team) = event.team.as_deref() {
                    auto_update_member_session_id(root, team, &agent_id, &session_id);
                } else {
                    debug!(
                        "session-start event missing 'team'; skipping config sessionId auto-update for '{}'",
                        agent_id
                    );
                }
            }
        }
        "session-end" | "session_end" => {
            let agent_id = match &event.agent {
                Some(id) => id.clone(),
                None => {
                    warn!("session-end event missing 'agent' field, skipping");
                    return;
                }
            };
            let membership =
                classify_team_membership(claude_root, event.team.as_deref(), &agent_id);
            if matches!(membership, TeamMembership::NonMember) {
                if let Some(team) = event.team.as_deref() {
                    debug!(
                        "Skipping transient session-end for non-member {}@{}",
                        agent_id, team
                    );
                } else {
                    debug!("Skipping transient session-end for non-member {}", agent_id);
                }
                return;
            }
            let session_id = match event
                .session_id
                .as_deref()
                .filter(|sid| !sid.trim().is_empty())
            {
                Some(sid) => sid,
                None => {
                    debug!("SessionEnd hook missing sessionId for {agent_id}; skipping");
                    return;
                }
            };
            debug!("SessionEnd hook received for {agent_id}");
            if let Some(registry) = session_registry {
                let mut reg = registry.lock().unwrap();
                if let Some(team) = event.team.as_deref() {
                    match reg.mark_dead_for_team_session(team, &agent_id, session_id) {
                        MarkDeadForSessionOutcome::MarkedDead => {}
                        MarkDeadForSessionOutcome::AlreadyDead => {
                            debug!(
                                "SessionEnd duplicate ignored for {agent_id}@{team} session={session_id}"
                            );
                        }
                        MarkDeadForSessionOutcome::UnknownSession => {
                            debug!(
                                "SessionEnd ignored for unknown session {agent_id}@{team} session={session_id}"
                            );
                        }
                        MarkDeadForSessionOutcome::SessionMismatch { current_session_id } => {
                            warn!(
                                team = %team,
                                agent = %agent_id,
                                current_session_id = %current_session_id,
                                received_session_id = %session_id,
                                "SessionEnd session_id mismatch; ignoring"
                            );
                        }
                    }
                } else {
                    debug!(
                        "SessionEnd hook missing team for {agent_id}; skipping scoped dead-mark"
                    );
                }
            }
        }
        unknown => {
            debug!("Unrecognised hook event type '{unknown}', ignoring");
        }
    }
}

fn classify_team_membership(
    claude_root: Option<&Path>,
    team: Option<&str>,
    agent_name: &str,
) -> TeamMembership {
    let Some(claude_root) = claude_root else {
        return TeamMembership::Unknown;
    };
    let Some(team) = team.map(str::trim).filter(|t| !t.is_empty()) else {
        return TeamMembership::Unknown;
    };
    let config_path = claude_root.join("teams").join(team).join("config.json");
    if !config_path.is_file() {
        return TeamMembership::Unknown;
    }

    let content = match std::fs::read_to_string(&config_path) {
        Ok(content) => content,
        Err(_) => return TeamMembership::Unknown,
    };
    let team_config: TeamConfig = match serde_json::from_str(&content) {
        Ok(config) => config,
        Err(_) => return TeamMembership::Unknown,
    };

    let expected_agent_id = format!("{agent_name}@{team}");
    if team_config
        .members
        .iter()
        .any(|member| member.name == agent_name || member.agent_id == expected_agent_id)
    {
        TeamMembership::Member
    } else {
        TeamMembership::NonMember
    }
}

/// Atomically write `team_config` to `config_path` using a lock file, a `.tmp`
/// staging file, and the project's `atomic_swap` infrastructure.
///
/// This is the daemon-side equivalent of `write_team_config` in the CLI crate.
/// Returns `Err` on any I/O failure so callers can log at the appropriate level.
fn write_team_config_atomic(config_path: &Path, config: &TeamConfig) -> Result<(), anyhow::Error> {
    let lock_path = config_path.with_extension("lock");
    let _lock =
        acquire_lock(&lock_path, 5).map_err(|e| anyhow::anyhow!("failed to acquire lock: {e}"))?;

    let serialized = serde_json::to_string_pretty(config)
        .map_err(|e| anyhow::anyhow!("serialisation failed: {e}"))?;

    let tmp_path = config_path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp_path)
        .map_err(|e| anyhow::anyhow!("cannot create tmp file: {e}"))?;
    file.write_all(serialized.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| anyhow::anyhow!("write failed: {e}"))?;
    drop(file);

    atomic_swap(config_path, &tmp_path).map_err(|e| anyhow::anyhow!("atomic swap failed: {e}"))?;

    Ok(())
}

/// Update `sessionId` in one team config under
/// `{claude_root}/teams/{team}/config.json` for any member whose `name`
/// matches `agent_name` (the bare agent name, without `@team` suffix) and
/// whose stored `sessionId` differs from `new_session_id`.
///
/// This is a best-effort operation: all errors are logged at `debug` level
/// and never propagated to the caller.  Only members that have
/// `externalBackendType` set (i.e., external agents registered via
/// `add-member`) are updated; Claude Code members are left untouched.
fn auto_update_member_session_id(
    claude_root: &Path,
    team: &str,
    agent_name: &str,
    new_session_id: &str,
) {
    let config_path = claude_root.join("teams").join(team).join("config.json");
    if !config_path.is_file() {
        debug!(
            "auto_update_member_session_id: team config not found for team '{}': {}",
            team,
            config_path.display()
        );
        return;
    }

    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(e) => {
            debug!(
                "auto_update_member_session_id: failed to read {}: {e}",
                config_path.display()
            );
            return;
        }
    };

    let mut team_config: TeamConfig = match serde_json::from_str(&content) {
        Ok(tc) => tc,
        Err(e) => {
            debug!(
                "auto_update_member_session_id: failed to parse {}: {e}",
                config_path.display()
            );
            return;
        }
    };

    let mut changed = false;
    for member in &mut team_config.members {
        // Only update external agents (those with externalBackendType set).
        if member.name == agent_name
            && member.external_backend_type.is_some()
            && member.session_id.as_deref() != Some(new_session_id)
        {
            debug!(
                "auto_update_member_session_id: updating sessionId for '{}' in '{}'",
                agent_name,
                config_path.display()
            );
            member.session_id = Some(new_session_id.to_string());
            changed = true;
        }
    }

    if !changed {
        return;
    }

    // Write updated config atomically via lock + tmp file + atomic swap.
    if let Err(e) = write_team_config_atomic(&config_path, &team_config) {
        debug!("auto_update_member_session_id: atomic write failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::session_registry::new_session_registry;

    fn make_state() -> Arc<Mutex<AgentStateTracker>> {
        Arc::new(Mutex::new(AgentStateTracker::new()))
    }

    fn make_deduper() -> AvailabilityDeduper {
        AvailabilityDeduper::default()
    }

    fn write_team_config(root: &Path, team: &str, members: &[(&str, &str)]) -> std::path::PathBuf {
        let team_dir = root.join("teams").join(team);
        std::fs::create_dir_all(&team_dir).unwrap();
        let members_json: Vec<serde_json::Value> = members
            .iter()
            .map(|(name, agent_type)| {
                serde_json::json!({
                    "agentId": format!("{name}@{team}"),
                    "name": name,
                    "agentType": agent_type,
                    "model": "unknown",
                    "joinedAt": 1739284800000u64,
                    "cwd": ".",
                    "subscriptions": []
                })
            })
            .collect();
        let config = serde_json::json!({
            "name": team,
            "createdAt": 1739284800000u64,
            "leadAgentId": format!("team-lead@{team}"),
            "leadSessionId": "lead-session",
            "members": members_json
        });
        let config_path = team_dir.join("config.json");
        std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
        config_path
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
        let mut deduper = make_deduper();
        // Should log a warning and return without panicking.
        process_hook_line("not json at all", &state, None, None, &mut deduper);
        process_hook_line("{broken", &state, None, None, &mut deduper);
        process_hook_line("", &state, None, None, &mut deduper);
        // State should be unchanged.
        assert!(state.lock().unwrap().all_states().is_empty());
    }

    #[test]
    fn test_agent_turn_complete_transitions_to_idle() {
        let state = make_state();
        let mut deduper = make_deduper();
        state.lock().unwrap().register_agent("arch-ctm");
        state
            .lock()
            .unwrap()
            .set_state("arch-ctm", AgentState::Unknown);

        let json = r#"{"type":"agent-turn-complete","agent":"arch-ctm","team":"atm-dev","state":"idle","timestamp":"2026-03-01T00:00:00Z","idempotency_key":"k1"}"#;
        process_hook_line(json, &state, None, None, &mut deduper);

        assert_eq!(
            state.lock().unwrap().get_state("arch-ctm"),
            Some(AgentState::Idle)
        );
    }

    #[test]
    fn test_busy_to_idle_via_hook() {
        let state = make_state();
        let mut deduper = make_deduper();
        state.lock().unwrap().register_agent("arch-ctm");
        state
            .lock()
            .unwrap()
            .set_state("arch-ctm", AgentState::Active);

        let json = r#"{"type":"agent-turn-complete","agent":"arch-ctm","team":"atm-dev","state":"idle","timestamp":"2026-03-01T00:00:00Z","idempotency_key":"k2"}"#;
        process_hook_line(json, &state, None, None, &mut deduper);

        assert_eq!(
            state.lock().unwrap().get_state("arch-ctm"),
            Some(AgentState::Idle)
        );
    }

    #[test]
    fn test_auto_register_on_hook_for_unknown_agent() {
        let state = make_state();
        let mut deduper = make_deduper();
        // Agent not pre-registered.
        let json = r#"{"type":"agent-turn-complete","agent":"new-agent","team":"atm-dev","state":"idle","timestamp":"2026-03-01T00:00:00Z","idempotency_key":"k3"}"#;
        process_hook_line(json, &state, None, None, &mut deduper);

        assert_eq!(
            state.lock().unwrap().get_state("new-agent"),
            Some(AgentState::Idle)
        );
    }

    #[test]
    fn test_agent_turn_complete_non_member_with_team_config_is_ignored() {
        let state = make_state();
        let mut deduper = make_deduper();
        let dir = tempfile::tempdir().unwrap();
        write_team_config(dir.path(), "atm-dev", &[("arch-ctm", "codex")]);

        let json = r#"{"type":"agent-turn-complete","agent":"transient-worker","team":"atm-dev","state":"idle","timestamp":"2026-03-01T00:00:00Z","idempotency_key":"k-non-member"}"#;
        process_hook_line(json, &state, None, Some(dir.path()), &mut deduper);

        assert!(
            state.lock().unwrap().all_states().is_empty(),
            "non-member transient hook should not auto-register tracker state"
        );
    }

    #[test]
    fn test_agent_turn_complete_team_member_bootstraps_tracker_state() {
        let state = make_state();
        let mut deduper = make_deduper();
        let dir = tempfile::tempdir().unwrap();
        write_team_config(dir.path(), "atm-dev", &[("arch-ctm", "codex")]);

        let json = r#"{"type":"agent-turn-complete","agent":"arch-ctm","team":"atm-dev","state":"idle","timestamp":"2026-03-01T00:00:00Z","idempotency_key":"k-member"}"#;
        process_hook_line(json, &state, None, Some(dir.path()), &mut deduper);

        assert_eq!(
            state.lock().unwrap().get_state("arch-ctm"),
            Some(AgentState::Idle)
        );
    }

    #[test]
    fn test_missing_agent_field_does_not_panic() {
        let state = make_state();
        let mut deduper = make_deduper();
        // event_type present but agent field missing
        let json = r#"{"type":"agent-turn-complete","team":"atm-dev","state":"idle","timestamp":"2026-03-01T00:00:00Z","idempotency_key":"k4"}"#;
        process_hook_line(json, &state, None, None, &mut deduper);
        // Nothing should be added to state.
        assert!(state.lock().unwrap().all_states().is_empty());
    }

    #[test]
    fn test_unknown_event_type_ignored() {
        let state = make_state();
        let mut deduper = make_deduper();
        let json = r#"{"type":"after-tool-use","agent":"arch-ctm"}"#;
        process_hook_line(json, &state, None, None, &mut deduper);
        assert!(state.lock().unwrap().all_states().is_empty());
    }

    // ── session-start / session-end events ───────────────────────────────

    #[test]
    fn test_session_start_calls_upsert_on_registry() {
        let state = make_state();
        let mut deduper = make_deduper();
        let registry = new_session_registry();

        let json = r#"{"type":"session-start","agent":"arch-ctm","sessionId":"sess-abc","processId":4242}"#;
        process_hook_line(json, &state, Some(&registry), None, &mut deduper);

        let reg = registry.lock().unwrap();
        let record = reg
            .query("arch-ctm")
            .expect("arch-ctm should be in registry");
        assert_eq!(record.session_id, "sess-abc");
        assert_eq!(record.process_id, 4242);
        use crate::daemon::session_registry::SessionState;
        assert_eq!(record.state, SessionState::Active);
    }

    #[test]
    fn test_session_start_non_member_with_team_config_skips_registry_upsert() {
        let state = make_state();
        let mut deduper = make_deduper();
        let registry = new_session_registry();
        let dir = tempfile::tempdir().unwrap();
        write_team_config(dir.path(), "atm-dev", &[("arch-ctm", "codex")]);

        let json = r#"{"type":"session-start","agent":"transient-worker","team":"atm-dev","sessionId":"sess-transient","processId":4242}"#;
        process_hook_line(
            json,
            &state,
            Some(&registry),
            Some(dir.path()),
            &mut deduper,
        );

        let reg = registry.lock().unwrap();
        assert!(
            reg.query_for_team("atm-dev", "transient-worker").is_none(),
            "non-member transient session-start must not create session registry row"
        );
    }

    #[test]
    fn test_session_end_calls_mark_dead_on_registry() {
        let state = make_state();
        let mut deduper = make_deduper();
        let registry = new_session_registry();

        // First register via session-start in a team-scoped record.
        registry
            .lock()
            .unwrap()
            .upsert_for_team("atm-dev", "arch-ctm", "sess-abc", 4242);

        let json =
            r#"{"type":"session-end","agent":"arch-ctm","team":"atm-dev","sessionId":"sess-abc"}"#;
        process_hook_line(json, &state, Some(&registry), None, &mut deduper);

        let reg = registry.lock().unwrap();
        let record = reg
            .query_for_team("atm-dev", "arch-ctm")
            .expect("arch-ctm should be in registry");
        use crate::daemon::session_registry::SessionState;
        assert_eq!(record.state, SessionState::Dead);
    }

    #[test]
    fn test_session_end_non_member_with_team_config_is_ignored() {
        let state = make_state();
        let mut deduper = make_deduper();
        let registry = new_session_registry();
        let dir = tempfile::tempdir().unwrap();
        write_team_config(dir.path(), "atm-dev", &[("arch-ctm", "codex")]);

        let json = r#"{"type":"session-end","agent":"transient-worker","team":"atm-dev","sessionId":"sess-transient"}"#;
        process_hook_line(
            json,
            &state,
            Some(&registry),
            Some(dir.path()),
            &mut deduper,
        );

        assert!(
            registry
                .lock()
                .unwrap()
                .query_for_team("atm-dev", "transient-worker")
                .is_none(),
            "non-member transient session-end must not create or mutate session rows"
        );
    }

    #[test]
    fn test_session_end_missing_team_skips_unscoped_mark_dead() {
        let state = make_state();
        let mut deduper = make_deduper();
        let registry = new_session_registry();

        registry
            .lock()
            .unwrap()
            .upsert("arch-ctm", "sess-abc", 4242);

        let json = r#"{"type":"session-end","agent":"arch-ctm","sessionId":"sess-abc"}"#;
        process_hook_line(json, &state, Some(&registry), None, &mut deduper);

        let reg = registry.lock().unwrap();
        let record = reg
            .query("arch-ctm")
            .expect("arch-ctm should be in registry");
        use crate::daemon::session_registry::SessionState;
        assert_eq!(
            record.state,
            SessionState::Active,
            "missing-team session-end must not apply unscoped dead mark"
        );
    }

    #[test]
    fn test_session_end_team_scoped_unknown_session_is_noop() {
        let state = make_state();
        let mut deduper = make_deduper();
        let registry = new_session_registry();

        let json = r#"{"type":"session-end","agent":"arch-ctm","team":"atm-dev","sessionId":"sess-unknown"}"#;
        process_hook_line(json, &state, Some(&registry), None, &mut deduper);

        assert!(
            registry
                .lock()
                .unwrap()
                .query_for_team("atm-dev", "arch-ctm")
                .is_none()
        );
    }

    #[test]
    fn test_session_end_team_scoped_mismatch_is_noop() {
        let state = make_state();
        let mut deduper = make_deduper();
        let registry = new_session_registry();

        registry
            .lock()
            .unwrap()
            .upsert_for_team("atm-dev", "arch-ctm", "sess-current", 4242);

        let json = r#"{"type":"session-end","agent":"arch-ctm","team":"atm-dev","sessionId":"sess-other"}"#;
        process_hook_line(json, &state, Some(&registry), None, &mut deduper);

        let reg = registry.lock().unwrap();
        let record = reg
            .query_for_team("atm-dev", "arch-ctm")
            .expect("arch-ctm should be in registry");
        use crate::daemon::session_registry::SessionState;
        assert_eq!(record.state, SessionState::Active);
        assert_eq!(record.session_id, "sess-current");
    }

    #[test]
    fn test_session_end_team_scoped_already_dead_is_noop() {
        let state = make_state();
        let mut deduper = make_deduper();
        let registry = new_session_registry();

        {
            let mut reg = registry.lock().unwrap();
            reg.upsert_for_team("atm-dev", "arch-ctm", "sess-abc", 4242);
            reg.mark_dead_for_team("atm-dev", "arch-ctm");
        }

        let json =
            r#"{"type":"session-end","agent":"arch-ctm","team":"atm-dev","sessionId":"sess-abc"}"#;
        process_hook_line(json, &state, Some(&registry), None, &mut deduper);

        let reg = registry.lock().unwrap();
        let record = reg
            .query_for_team("atm-dev", "arch-ctm")
            .expect("arch-ctm should be in registry");
        use crate::daemon::session_registry::SessionState;
        assert_eq!(record.state, SessionState::Dead);
    }

    #[test]
    fn test_session_end_underscore_alias_marks_dead() {
        let state = make_state();
        let mut deduper = make_deduper();
        let registry = new_session_registry();

        registry
            .lock()
            .unwrap()
            .upsert_for_team("atm-dev", "arch-ctm", "sess-underscore", 4242);

        let json = r#"{"type":"session_end","agent":"arch-ctm","team":"atm-dev","sessionId":"sess-underscore"}"#;
        process_hook_line(json, &state, Some(&registry), None, &mut deduper);

        let reg = registry.lock().unwrap();
        let record = reg
            .query_for_team("atm-dev", "arch-ctm")
            .expect("arch-ctm should be in registry");
        use crate::daemon::session_registry::SessionState;
        assert_eq!(record.state, SessionState::Dead);
    }

    #[test]
    fn test_session_start_without_registry_does_not_panic() {
        let state = make_state();
        let mut deduper = make_deduper();
        // No registry provided — should not panic.
        let json =
            r#"{"type":"session-start","agent":"arch-ctm","sessionId":"sess-abc","processId":1}"#;
        process_hook_line(json, &state, None, None, &mut deduper);
        // State tracker should not be affected.
        assert!(state.lock().unwrap().all_states().is_empty());
    }

    #[test]
    fn test_session_start_missing_session_id_skips() {
        let state = make_state();
        let mut deduper = make_deduper();
        let registry = new_session_registry();
        let json = r#"{"type":"session-start","agent":"arch-ctm"}"#;
        process_hook_line(json, &state, Some(&registry), None, &mut deduper);
        // Registry should remain empty because sessionId is missing.
        assert!(registry.lock().unwrap().is_empty());
    }

    #[test]
    fn test_session_start_missing_agent_skips() {
        let state = make_state();
        let mut deduper = make_deduper();
        let registry = new_session_registry();
        let json = r#"{"type":"session-start","sessionId":"sess-abc","processId":1}"#;
        process_hook_line(json, &state, Some(&registry), None, &mut deduper);
        assert!(registry.lock().unwrap().is_empty());
    }

    // ── incremental file reading ──────────────────────────────────────────

    #[test]
    fn test_read_new_events_empty_file() {
        let state = make_state();
        let mut deduper = make_deduper();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, b"").unwrap();

        let new_offset = read_new_events(&path, 0, &state, None, None, &mut deduper);
        assert_eq!(new_offset, 0);
    }

    #[test]
    fn test_read_new_events_processes_lines() {
        let state = make_state();
        let mut deduper = make_deduper();
        state.lock().unwrap().register_agent("arch-ctm");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let line = "{\"type\":\"agent-turn-complete\",\"agent\":\"arch-ctm\",\"team\":\"atm-dev\",\"state\":\"idle\",\"timestamp\":\"2026-03-01T00:00:00Z\",\"idempotency_key\":\"k5\"}\n";
        std::fs::write(&path, line.as_bytes()).unwrap();

        let new_offset = read_new_events(&path, 0, &state, None, None, &mut deduper);
        assert_eq!(new_offset, line.len() as u64);
        assert_eq!(
            state.lock().unwrap().get_state("arch-ctm"),
            Some(AgentState::Idle)
        );
    }

    #[test]
    fn test_read_new_events_incremental() {
        let state = make_state();
        let mut deduper = make_deduper();
        state.lock().unwrap().register_agent("arch-ctm");
        state.lock().unwrap().register_agent("agent-b");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let line1 = "{\"type\":\"agent-turn-complete\",\"agent\":\"arch-ctm\",\"team\":\"atm-dev\",\"state\":\"idle\",\"timestamp\":\"2026-03-01T00:00:00Z\",\"idempotency_key\":\"k6\"}\n";
        std::fs::write(&path, line1.as_bytes()).unwrap();

        // First read
        let offset1 = read_new_events(&path, 0, &state, None, None, &mut deduper);
        assert_eq!(offset1, line1.len() as u64);
        assert_eq!(
            state.lock().unwrap().get_state("arch-ctm"),
            Some(AgentState::Idle)
        );

        // Append second event
        let line2 = "{\"type\":\"agent-turn-complete\",\"agent\":\"agent-b\",\"team\":\"atm-dev\",\"state\":\"idle\",\"timestamp\":\"2026-03-01T00:00:01Z\",\"idempotency_key\":\"k7\"}\n";
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, line2.as_bytes()).unwrap();
        drop(file);

        // Second read should only process line2
        let offset2 = read_new_events(&path, offset1, &state, None, None, &mut deduper);
        assert_eq!(offset2, (line1.len() + line2.len()) as u64);
        assert_eq!(
            state.lock().unwrap().get_state("agent-b"),
            Some(AgentState::Idle)
        );
    }

    #[test]
    fn test_read_new_events_handles_truncation() {
        let state = make_state();
        let mut deduper = make_deduper();
        state.lock().unwrap().register_agent("arch-ctm");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let line = "{\"type\":\"agent-turn-complete\",\"agent\":\"arch-ctm\",\"team\":\"atm-dev\",\"state\":\"idle\",\"timestamp\":\"2026-03-01T00:00:00Z\",\"idempotency_key\":\"k8\"}\n";
        std::fs::write(&path, line.as_bytes()).unwrap();

        // offset beyond file size (simulating truncation)
        let new_offset = read_new_events(&path, 9999, &state, None, None, &mut deduper);
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
        let mut deduper = make_deduper();
        let path = std::path::PathBuf::from("/nonexistent/path/events.jsonl");
        let new_offset = read_new_events(&path, 42, &state, None, None, &mut deduper);
        // Should return the same offset unchanged
        assert_eq!(new_offset, 42);
    }

    #[test]
    fn test_read_new_events_session_start_updates_registry() {
        let state = make_state();
        let mut deduper = make_deduper();
        let registry = new_session_registry();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let line = "{\"type\":\"session-start\",\"agent\":\"arch-ctm\",\"sessionId\":\"sess-xyz\",\"processId\":999}\n";
        std::fs::write(&path, line.as_bytes()).unwrap();

        let new_offset = read_new_events(&path, 0, &state, Some(&registry), None, &mut deduper);
        assert_eq!(new_offset, line.len() as u64);

        let reg = registry.lock().unwrap();
        let record = reg.query("arch-ctm").expect("agent should be in registry");
        assert_eq!(record.session_id, "sess-xyz");
        assert_eq!(record.process_id, 999);
    }

    #[test]
    fn test_duplicate_availability_event_idempotency_key_is_deduped() {
        let state = make_state();
        let mut deduper = make_deduper();
        state.lock().unwrap().register_agent("arch-ctm");
        state
            .lock()
            .unwrap()
            .set_state("arch-ctm", AgentState::Active);

        let json = r#"{"type":"agent-turn-complete","agent":"arch-ctm","team":"atm-dev","state":"idle","timestamp":"2026-03-01T00:00:00Z","idempotency_key":"dup-key"}"#;
        process_hook_line(json, &state, None, None, &mut deduper);
        assert_eq!(
            state.lock().unwrap().get_state("arch-ctm"),
            Some(AgentState::Idle)
        );

        std::thread::sleep(std::time::Duration::from_millis(20));
        let elapsed_before = state
            .lock()
            .unwrap()
            .time_since_transition("arch-ctm")
            .expect("elapsed should exist");

        process_hook_line(json, &state, None, None, &mut deduper);

        let elapsed_after = state
            .lock()
            .unwrap()
            .time_since_transition("arch-ctm")
            .expect("elapsed should exist");
        assert!(
            elapsed_after >= elapsed_before,
            "duplicate replay should not create a new transition"
        );
    }

    // ── auto_update_member_session_id unit tests ──────────────────────────

    /// Verify that `auto_update_member_session_id` updates the `sessionId`
    /// field in a team config file for a matching external agent.
    ///
    /// This test works entirely with the file system and does not require the
    /// daemon to be running.
    #[test]
    fn test_auto_update_member_session_id_updates_external_member() {
        let dir = tempfile::tempdir().unwrap();

        // Create a minimal `.claude/teams/<team>/` structure.
        let teams_dir = dir.path().join("teams");
        let team_dir = teams_dir.join("atm-dev");
        std::fs::create_dir_all(&team_dir).unwrap();

        // Write a minimal config.json containing one external member with an old sessionId.
        let config_json = serde_json::json!({
            "name": "atm-dev",
            "description": "test team",
            "createdAt": 1739284800000u64,
            "leadAgentId": "team-lead@atm-dev",
            "leadSessionId": "team-session",
            "members": [
                {
                    "agentId": "arch-ctm@atm-dev",
                    "name": "arch-ctm",
                    "agentType": "codex",
                    "model": "gpt5.3-codex",
                    "joinedAt": 1739284800000u64,
                    "cwd": ".",
                    "subscriptions": [],
                    "externalBackendType": "codex",
                    "sessionId": "old-session-id"
                }
            ]
        });

        let config_path = team_dir.join("config.json");
        std::fs::write(
            &config_path,
            serde_json::to_string_pretty(&config_json).unwrap(),
        )
        .unwrap();

        // Call the function under test with the `.claude` directory (parent of `teams/`).
        let claude_root = dir.path();
        auto_update_member_session_id(claude_root, "atm-dev", "arch-ctm", "new-session-id");

        // Read back and assert the sessionId was updated.
        let updated_content = std::fs::read_to_string(&config_path).unwrap();
        let updated: serde_json::Value = serde_json::from_str(&updated_content).unwrap();

        let session_id = updated["members"][0]["sessionId"]
            .as_str()
            .expect("sessionId should be present after update");

        assert_eq!(
            session_id, "new-session-id",
            "sessionId should have been updated from 'old-session-id' to 'new-session-id'"
        );
    }

    /// Verify that `auto_update_member_session_id` does NOT update members
    /// that lack `externalBackendType` (i.e., standard Claude Code members).
    #[test]
    fn test_auto_update_member_session_id_skips_claude_code_members() {
        let dir = tempfile::tempdir().unwrap();
        let teams_dir = dir.path().join("teams");
        let team_dir = teams_dir.join("atm-dev");
        std::fs::create_dir_all(&team_dir).unwrap();

        // A member without externalBackendType — this should NOT be updated.
        let config_json = serde_json::json!({
            "name": "atm-dev",
            "createdAt": 1739284800000u64,
            "leadAgentId": "team-lead@atm-dev",
            "leadSessionId": "team-session",
            "members": [
                {
                    "agentId": "team-lead@atm-dev",
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "claude-opus-4-6",
                    "joinedAt": 1739284800000u64,
                    "cwd": ".",
                    "subscriptions": [],
                    "sessionId": "old-session"
                }
            ]
        });

        let config_path = team_dir.join("config.json");
        std::fs::write(
            &config_path,
            serde_json::to_string_pretty(&config_json).unwrap(),
        )
        .unwrap();

        let claude_root = dir.path();
        auto_update_member_session_id(claude_root, "atm-dev", "team-lead", "new-session");

        // The standard member's sessionId should remain unchanged.
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        let session_id = after["members"][0]["sessionId"]
            .as_str()
            .unwrap_or("old-session");
        assert_eq!(
            session_id, "old-session",
            "Claude Code member sessionId should not be updated by auto_update_member_session_id"
        );
    }
    #[test]
    fn test_auto_update_member_session_id_scoped_to_target_team() {
        let dir = tempfile::tempdir().unwrap();
        let teams_dir = dir.path().join("teams");
        let team_a_dir = teams_dir.join("atm-dev");
        let team_b_dir = teams_dir.join("other-team");
        std::fs::create_dir_all(&team_a_dir).unwrap();
        std::fs::create_dir_all(&team_b_dir).unwrap();

        let config_a = serde_json::json!({
            "name": "atm-dev",
            "createdAt": 1739284800000u64,
            "leadAgentId": "team-lead@atm-dev",
            "leadSessionId": "lead-a",
            "members": [
                {
                    "agentId": "arch-ctm@atm-dev",
                    "name": "arch-ctm",
                    "agentType": "codex",
                    "model": "gpt5.3-codex",
                    "joinedAt": 1739284800000u64,
                    "cwd": ".",
                    "subscriptions": [],
                    "externalBackendType": "codex",
                    "sessionId": "old-a"
                }
            ]
        });
        let config_b = serde_json::json!({
            "name": "other-team",
            "createdAt": 1739284800000u64,
            "leadAgentId": "team-lead@other-team",
            "leadSessionId": "lead-b",
            "members": [
                {
                    "agentId": "arch-ctm@other-team",
                    "name": "arch-ctm",
                    "agentType": "codex",
                    "model": "gpt5.3-codex",
                    "joinedAt": 1739284800000u64,
                    "cwd": ".",
                    "subscriptions": [],
                    "externalBackendType": "codex",
                    "sessionId": "old-b"
                }
            ]
        });

        let config_a_path = team_a_dir.join("config.json");
        let config_b_path = team_b_dir.join("config.json");
        std::fs::write(
            &config_a_path,
            serde_json::to_string_pretty(&config_a).unwrap(),
        )
        .unwrap();
        std::fs::write(
            &config_b_path,
            serde_json::to_string_pretty(&config_b).unwrap(),
        )
        .unwrap();

        auto_update_member_session_id(dir.path(), "atm-dev", "arch-ctm", "new-a");

        let after_a: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_a_path).unwrap()).unwrap();
        let after_b: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_b_path).unwrap()).unwrap();

        assert_eq!(after_a["members"][0]["sessionId"].as_str(), Some("new-a"));
        assert_eq!(after_b["members"][0]["sessionId"].as_str(), Some("old-b"));
    }

    // ── reconcile_tick convergence ────────────────────────────────────────

    /// Verify that calling `read_new_events` directly (the same logic executed
    /// by the 200 ms `reconcile_tick` arm inside `HookWatcher::run`) picks up
    /// an event file and transitions agent state, independently of any
    /// filesystem notification.
    ///
    /// This exercises the polling-fallback path: even when the OS-level
    /// `notify` watcher drops a filesystem event, the periodic tick drives
    /// convergence by re-reading the file at the current offset.
    #[test]
    fn test_reconcile_tick_drives_convergence_without_fs_event() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");

        // Set up state tracker with agent in Busy state.
        let state = make_state();
        state.lock().unwrap().register_agent("arch-ctm");
        state
            .lock()
            .unwrap()
            .set_state("arch-ctm", AgentState::Active);

        // Write an availability event directly — no HookWatcher running,
        // no filesystem notification involved.
        let line = "{\"type\":\"agent-turn-complete\",\"agent\":\"arch-ctm\",\"team\":\"atm-dev\",\
                    \"state\":\"idle\",\"timestamp\":\"2026-03-01T12:00:00Z\",\
                    \"idempotency_key\":\"atm-dev:arch-ctm:reconcile-tick-test\"}\n";
        {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(line.as_bytes()).unwrap();
        }

        // Simulate exactly what the reconcile_tick arm does: call
        // read_new_events with the current offset (0).
        let mut deduper = make_deduper();
        let new_offset = read_new_events(&path, 0, &state, None, None, &mut deduper);

        // Offset should have advanced past the line we wrote.
        assert_eq!(
            new_offset,
            line.len() as u64,
            "reconcile_tick read should consume the full event line"
        );

        // Agent must have transitioned to Idle via the polling path alone.
        assert_eq!(
            state.lock().unwrap().get_state("arch-ctm"),
            Some(AgentState::Idle),
            "reconcile_tick convergence: arch-ctm should be Idle after read_new_events call"
        );

        // A second reconcile_tick call with the advanced offset must be a
        // no-op (idempotency_key deduplication).
        let state_before_second = state.lock().unwrap().get_state("arch-ctm");
        let new_offset2 = read_new_events(&path, new_offset, &state, None, None, &mut deduper);
        assert_eq!(new_offset2, new_offset, "no new bytes at EOF");
        assert_eq!(
            state.lock().unwrap().get_state("arch-ctm"),
            state_before_second,
            "second reconcile_tick call must not alter state"
        );
    }
}
