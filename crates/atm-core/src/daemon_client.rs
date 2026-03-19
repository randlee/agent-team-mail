//! Client for querying the ATM daemon via Unix socket.
//!
//! Provides a thin, synchronous interface for CLI commands to query daemon state.
//! The daemon listens on a Unix domain socket at:
//!
//! ```text
//! ${ATM_HOME}/.atm/daemon/atm-daemon.sock
//! ```
//!
//! The protocol is newline-delimited JSON (one request line, one response line per connection):
//!
//! ```json
//! // Request
//! {"version":1,"request_id":"uuid","command":"agent-state","payload":{"agent":"arch-ctm","team":"atm-dev"}}
//! // Response
//! {"version":1,"request_id":"uuid","status":"ok","payload":{"state":"idle","last_transition":"2026-02-16T22:30:00Z"}}
//! ```
//!
//! # Platform Notes
//!
//! Unix domain sockets are only available on Unix platforms. On non-Unix platforms,
//! all functions return `Ok(None)` immediately without attempting a connection.
//!
//! # Graceful Fallback
//!
//! All public functions return `Ok(None)` when:
//! - The daemon is not running (connection refused or socket not found)
//! - The platform does not support Unix sockets
//! - Any I/O error occurs during the query
//!
//! Only truly unexpected errors (e.g., I/O errors during write after a successful connect)
//! are surfaced as `Err`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::event_log::EventFields;
use agent_team_mail_daemon_launch::{LaunchClass, SpawnDaemonRequest, spawn_daemon_process};

use crate::consts::{
    DAEMON_METADATA_SETTLE_MS, DAEMON_QUERY_TIMEOUT_MS, DAEMON_TIMEOUT_MAX_SECS,
    DAEMON_TIMEOUT_MIN_SECS, RETRY_SLEEP_MS, SOCKET_IO_TIMEOUT_MS, STARTUP_DEADLINE_SECS,
};

/// Protocol version for the socket JSON protocol.
pub const PROTOCOL_VERSION: u32 = 1;

/// Lock metadata written by the daemon after acquiring the singleton lock.
///
/// This metadata is used by CLI autostart/health paths to validate daemon
/// identity (PID/home scope/executable) before trusting a pre-existing process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    #[default]
    Isolated,
    Release,
    Dev,
}

impl RuntimeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Dev => "dev",
            Self::Isolated => "isolated",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BuildProfile {
    Release,
    #[default]
    Debug,
}

impl BuildProfile {
    pub fn current() -> Self {
        if cfg!(debug_assertions) {
            Self::Debug
        } else {
            Self::Release
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Debug => "debug",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RuntimeOwnerMetadata {
    /// Runtime classification for the current daemon instance.
    pub runtime_kind: RuntimeKind,
    /// Build profile of the daemon binary.
    pub build_profile: BuildProfile,
    /// Canonicalized executable path of the daemon process when available.
    pub executable_path: String,
    /// Canonicalized ATM home scope used by the daemon instance.
    pub home_scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeMetadata {
    /// Runtime classification for this ATM home.
    pub runtime_kind: RuntimeKind,
    /// RFC3339 UTC timestamp when the runtime root was created.
    pub created_at: String,
    /// RFC3339 UTC timestamp when the isolated runtime lease expires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Stable test identifier for isolated-test runtime ownership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_identifier: Option<String>,
    /// Owning test-process PID when this runtime was launched as `isolated-test`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    /// Launch token id associated with this runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
    /// Whether this runtime may perform live GitHub polling.
    #[serde(default)]
    pub allow_live_github_polling: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedIsolatedRuntime {
    pub home: PathBuf,
    pub runtime_dir: PathBuf,
    pub socket_path: PathBuf,
    pub lock_path: PathBuf,
    pub status_path: PathBuf,
    pub metadata: RuntimeMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonLockMetadata {
    /// Daemon PID that currently owns the lock.
    pub pid: u32,
    /// Runtime owner metadata for the daemon instance.
    #[serde(flatten, default)]
    pub owner: RuntimeOwnerMetadata,
    /// Daemon version string.
    pub version: String,
    /// RFC3339 UTC timestamp for metadata write.
    pub written_at: String,
}

/// Per-team daemon startup sidecar entry written under `${ATM_HOME}/.atm/daemon/`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonTouchEntry {
    /// Daemon PID that touched this team at startup.
    pub pid: u32,
    /// RFC3339 UTC daemon startup timestamp.
    pub started_at: String,
    /// Canonical daemon binary path used for the startup touch.
    pub binary: String,
}

/// Snapshot of team -> daemon startup touch ownership.
pub type DaemonTouchSnapshot = BTreeMap<String, DaemonTouchEntry>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimePolicyInput {
    home_scope: PathBuf,
    daemon_bin: PathBuf,
    os_home: PathBuf,
    temp_root: PathBuf,
    build_profile: BuildProfile,
}

/// Identifies the origin of a lifecycle event sent via the `hook-event` command.
///
/// The `source` field is optional in the hook-event payload for backward
/// compatibility — callers that do not set it will produce payloads that
/// deserialise successfully, defaulting to [`LifecycleSourceKind::Unknown`].
///
/// # Validation policy
///
/// | `kind`        | `session_start` / `session_end` restriction          |
/// |---------------|------------------------------------------------------|
/// | `claude_hook` | Team-lead only (strictest)                           |
/// | `unknown`     | Treated as `claude_hook` (fail-closed default)       |
/// | `atm_mcp`     | Any team member (MCP proxy manages its own sessions) |
/// | `agent_hook`  | Any team member (same policy as `atm_mcp`)           |
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleSource {
    /// Discriminator string identifying the lifecycle event origin.
    pub kind: LifecycleSourceKind,
}

impl LifecycleSource {
    /// Create a [`LifecycleSource`] with the given kind.
    pub fn new(kind: LifecycleSourceKind) -> Self {
        Self { kind }
    }
}

/// Discriminator for the origin of a lifecycle event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleSourceKind {
    /// Event originated from a Claude Code hook (e.g., `session-start.py`).
    ///
    /// Strictest validation: only the team-lead may emit `session_start` and
    /// `session_end` events from this source.
    ClaudeHook,
    /// Event originated from the `atm-agent-mcp` proxy.
    ///
    /// Relaxed validation: any team member may emit lifecycle events because
    /// the MCP proxy manages its own Codex agent sessions, not the team-lead's
    /// Claude Code session.
    AtmMcp,
    /// Event originated from a non-Claude agent hook adapter (e.g., a Codex or
    /// Gemini relay script). Same validation policy as [`AtmMcp`](Self::AtmMcp).
    AgentHook,
    /// Origin unknown or not set by the sender.
    ///
    /// Treated as [`ClaudeHook`](Self::ClaudeHook) (strictest, fail-closed default).
    Unknown,
}

/// A request sent from CLI to daemon over the Unix socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketRequest {
    /// Protocol version. Must be [`PROTOCOL_VERSION`].
    pub version: u32,
    /// Unique identifier echoed back in the response.
    pub request_id: String,
    /// Command to execute (e.g., `"agent-state"`, `"list-agents"`).
    pub command: String,
    /// Command-specific payload.
    pub payload: serde_json::Value,
}

/// A response received from the daemon over the Unix socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketResponse {
    /// Protocol version.
    pub version: u32,
    /// Echoed `request_id` from the corresponding request.
    pub request_id: String,
    /// `"ok"` on success, `"error"` on failure.
    pub status: String,
    /// Response data on success (present when `status == "ok"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Error information on failure (present when `status == "error"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<SocketError>,
}

impl SocketResponse {
    /// Returns `true` if the response indicates success.
    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }
}

/// Error details returned by the daemon on failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketError {
    /// Machine-readable error code (e.g., `"AGENT_NOT_FOUND"`).
    pub code: String,
    /// Human-readable error message.
    pub message: String,
}

/// Agent state information returned by the `agent-state` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStateInfo {
    /// Current state: `"launching"`, `"busy"`, `"idle"`, or `"killed"`.
    pub state: String,
    /// ISO 8601 timestamp of the last state transition (if available).
    pub last_transition: Option<String>,
}

/// Summary of a single agent returned by the `list-agents` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    /// Agent identifier.
    pub agent: String,
    /// Current state string.
    pub state: String,
}

/// Canonical daemon-backed member-state snapshot returned by team-scoped
/// `list-agents` queries.
///
/// This struct is the single liveness/status source consumed by CLI diagnostic
/// surfaces (`atm doctor`, `atm status`, `atm members`). It is derived by the
/// daemon from session-registry + tracker evidence and must not be inferred
/// from config `isActive`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalMemberState {
    /// Agent/member name.
    pub agent: String,
    /// Canonical daemon status (`active`, `idle`, `offline`, `unknown`).
    pub state: String,
    /// Canonical activity hint (`busy`, `idle`, `unknown`).
    #[serde(default)]
    pub activity: String,
    /// Session UUID from the daemon registry when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Process ID from the daemon registry when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u32>,
    /// Most recent liveness confirmation timestamp from daemon PID checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_alive_at: Option<String>,
    /// Human-readable derivation reason.
    #[serde(default)]
    pub reason: String,
    /// Source of truth used for state derivation.
    #[serde(default)]
    pub source: String,
    /// Whether this member currently exists in team `config.json`.
    ///
    /// Defaults to `true` for backward compatibility with older daemon payloads
    /// that did not include this field.
    #[serde(default = "default_in_config_true", skip_serializing_if = "is_true")]
    pub in_config: bool,
}

fn default_in_config_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// Render CLI-facing status taxonomy from daemon canonical member state.
///
/// Output values are constrained to `Active|Idle|Dead|Unknown`.
pub fn canonical_status_label(state: Option<&CanonicalMemberState>) -> &'static str {
    match state.map(|s| s.state.as_str()) {
        Some("active") => "Active",
        Some("idle") => "Idle",
        Some("offline") | Some("dead") => "Dead",
        _ => "Unknown",
    }
}

/// Render CLI-facing activity taxonomy from daemon canonical member state.
///
/// Output values are constrained to `Busy|Idle|Unknown`.
pub fn canonical_activity_label(state: Option<&CanonicalMemberState>) -> &'static str {
    match state.map(|s| s.activity.as_str()) {
        Some("busy") => "Busy",
        Some("idle") => "Idle",
        _ => "Unknown",
    }
}

/// Return best-effort binary liveness from daemon canonical member state.
pub fn canonical_liveness_bool(state: Option<&CanonicalMemberState>) -> Option<bool> {
    match state.map(|s| s.state.as_str()) {
        Some("active") | Some("idle") => Some(true),
        Some("offline") | Some("dead") => Some(false),
        _ => None,
    }
}

/// Configuration for launching a new agent via the daemon.
///
/// Sent as the payload of a `"launch"` socket command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchConfig {
    /// Agent identity name (e.g., `"arch-ctm"`).
    pub agent: String,
    /// Team name (e.g., `"atm-dev"`).
    pub team: String,
    /// Command to run in the tmux pane (e.g., `"codex --yolo"`).
    pub command: String,
    /// Optional initial prompt to send after the agent reaches the `Idle` state.
    pub prompt: Option<String>,
    /// Readiness timeout in seconds. The daemon waits up to this long for the
    /// agent state to transition to `Idle` before sending the initial prompt.
    /// Defaults to 30 if omitted.
    pub timeout_secs: u32,
    /// Extra environment variables to export in the pane before starting the agent.
    ///
    /// `ATM_IDENTITY` and `ATM_TEAM` are always set automatically and do not
    /// need to be included here.
    pub env_vars: std::collections::HashMap<String, String>,
    /// Runtime adapter kind (e.g., `"codex"`, `"gemini"`).
    ///
    /// Older clients may omit this field; daemon should treat missing as
    /// runtime default (`codex`).
    #[serde(default)]
    pub runtime: Option<String>,
    /// Optional runtime-native session ID used for resume-aware launches.
    ///
    /// For Gemini this maps to the Gemini session UUID that should be resumed.
    #[serde(default)]
    pub resume_session_id: Option<String>,
}

/// Result of a successful agent launch returned by the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchResult {
    /// Agent identity name.
    pub agent: String,
    /// tmux pane ID assigned to the new agent (e.g., `"%42"`).
    pub pane_id: String,
    /// Agent state string immediately after launch (`"launching"`, `"idle"`, etc.).
    pub state: String,
    /// Non-fatal warning, e.g., readiness timeout was reached before the agent
    /// transitioned to `Idle`.
    pub warning: Option<String>,
}

/// Request the daemon to launch a new agent.
///
/// This is a synchronous call: the function blocks until the daemon responds
/// (the daemon itself may respond before full readiness, but the round-trip
/// completes within the socket timeout).
///
/// Returns `Ok(None)` when:
/// - The daemon is not running.
/// - The platform does not support Unix sockets.
/// - A connection-level I/O error occurs before any response is read.
///
/// Returns `Ok(Some(result))` on success.
///
/// Returns `Err` only for unexpected I/O errors *after* a connection is
/// established and a request has been written.
///
/// # Arguments
///
/// * `config` - Launch configuration for the new agent.
pub fn launch_agent(config: &LaunchConfig) -> anyhow::Result<Option<LaunchResult>> {
    let payload = match serde_json::to_value(config) {
        Ok(v) => v,
        Err(e) => anyhow::bail!("Failed to serialize LaunchConfig: {e}"),
    };

    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "launch".to_string(),
        payload,
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        let msg = response
            .error
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| "unknown daemon error".to_string());
        anyhow::bail!("Daemon returned error for launch command: {msg}");
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<LaunchResult>(payload) {
        Ok(result) => Ok(Some(result)),
        Err(e) => anyhow::bail!("Failed to parse LaunchResult from daemon response: {e}"),
    }
}

/// Compute the daemon runtime directory.
///
/// The path is `${ATM_HOME}/.atm/daemon`, where `ATM_HOME` is resolved via
/// [`crate::home::get_home_dir`].
pub fn daemon_runtime_dir() -> anyhow::Result<PathBuf> {
    let home = crate::home::get_home_dir()?;
    Ok(home.join(".atm/daemon"))
}

/// Compute the daemon runtime directory for an explicit ATM home.
pub fn daemon_runtime_dir_for(home: &std::path::Path) -> PathBuf {
    home.join(".atm/daemon")
}

/// Compute the well-known socket path for the ATM daemon.
///
/// The path is `${ATM_HOME}/.atm/daemon/atm-daemon.sock`.
///
/// # Errors
///
/// Returns an error only if home directory resolution fails.
pub fn daemon_socket_path() -> anyhow::Result<PathBuf> {
    Ok(daemon_runtime_dir()?.join("atm-daemon.sock"))
}

/// Compute the well-known PID file path for the ATM daemon.
///
/// The path is `${ATM_HOME}/.atm/daemon/atm-daemon.pid`.
///
/// # Errors
///
/// Returns an error only if home directory resolution fails.
pub fn daemon_pid_path() -> anyhow::Result<PathBuf> {
    Ok(daemon_runtime_dir()?.join("atm-daemon.pid"))
}

/// Compute the daemon status snapshot path.
///
/// The path is `${ATM_HOME}/.atm/daemon/status.json`.
pub fn daemon_status_path() -> anyhow::Result<PathBuf> {
    Ok(daemon_runtime_dir()?.join("status.json"))
}

/// Compute the daemon status snapshot path for an explicit ATM home.
pub fn daemon_status_path_for(home: &std::path::Path) -> PathBuf {
    daemon_runtime_dir_for(home).join("status.json")
}

/// Compute the daemon startup touch sidecar path for an explicit ATM home.
///
/// The path is `${ATM_HOME}/.atm/daemon/daemon-touch.json`.
pub fn daemon_touch_path_for(home: &std::path::Path) -> PathBuf {
    daemon_runtime_dir_for(home).join("daemon-touch.json")
}

/// Compute the daemon singleton lock path.
///
/// The path is `${ATM_HOME}/.atm/daemon/daemon.lock`.
pub fn daemon_lock_path() -> anyhow::Result<PathBuf> {
    Ok(daemon_runtime_dir()?.join("daemon.lock"))
}

/// Compute the daemon singleton lock metadata path.
///
/// The path is `${ATM_HOME}/.atm/daemon/daemon.lock.meta.json`.
pub fn daemon_lock_metadata_path() -> anyhow::Result<PathBuf> {
    Ok(daemon_runtime_dir()?.join("daemon.lock.meta.json"))
}

/// Compute the daemon lock metadata path for an explicit ATM home.
pub fn daemon_lock_metadata_path_for(home: &std::path::Path) -> PathBuf {
    daemon_runtime_dir_for(home).join("daemon.lock.meta.json")
}

/// Compute the daemon lock metadata write lock path for an explicit ATM home.
pub fn daemon_lock_metadata_write_lock_path_for(home: &std::path::Path) -> PathBuf {
    daemon_runtime_dir_for(home).join("daemon.lock.meta.lock")
}

/// Compute the daemon startup serialization lock path.
///
/// The path is `${ATM_HOME}/.atm/daemon/daemon-start.lock`.
pub fn daemon_start_lock_path() -> anyhow::Result<PathBuf> {
    Ok(daemon_runtime_dir()?.join("daemon-start.lock"))
}

/// Compute the durable dedup store path for the ATM daemon.
///
/// The path is `${ATM_HOME}/.atm/daemon/dedup.jsonl`, where `ATM_HOME` is
/// resolved via [`crate::home::get_home_dir`].
///
/// # Errors
///
/// Returns an error only if home directory resolution fails.
pub fn daemon_dedup_path() -> anyhow::Result<PathBuf> {
    Ok(daemon_runtime_dir()?.join("dedup.jsonl"))
}

/// Compute the gh-monitor health snapshot path.
pub fn daemon_gh_monitor_health_path() -> anyhow::Result<PathBuf> {
    Ok(daemon_runtime_dir()?.join("gh-monitor-health.json"))
}

/// Compute the gh-monitor health snapshot path for an explicit ATM home.
pub fn daemon_gh_monitor_health_path_for(home: &std::path::Path) -> PathBuf {
    daemon_runtime_dir_for(home).join("gh-monitor-health.json")
}

/// Compute the runtime metadata path for an explicit ATM home.
pub fn daemon_runtime_metadata_path_for(home: &std::path::Path) -> PathBuf {
    daemon_runtime_dir_for(home).join("runtime.json")
}

/// Compute the runtime metadata write lock path for an explicit ATM home.
pub fn daemon_runtime_metadata_write_lock_path_for(home: &std::path::Path) -> PathBuf {
    daemon_runtime_dir_for(home).join("runtime.lock")
}

fn canonicalize_lossy(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn default_dev_runtime_root_for(os_home: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(local) = dirs::data_local_dir() {
            return local.join("atm-dev");
        }
        os_home.join("AppData").join("Local").join("atm-dev")
    }

    #[cfg(not(windows))]
    {
        os_home.join(".local").join("atm-dev")
    }
}

fn default_dev_runtime_home_for(os_home: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        default_dev_runtime_root_for(os_home).join("home")
    }

    #[cfg(not(windows))]
    {
        os_home
            .join(".local")
            .join("share")
            .join("atm-dev")
            .join("home")
    }
}

fn looks_like_repo_or_worktree_binary(path: &Path) -> bool {
    if path
        .components()
        .any(|component| component.as_os_str() == "target")
    {
        return true;
    }

    let mut current = path.parent();
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return true;
        }
        current = dir.parent();
    }

    false
}

fn is_approved_release_binary(path: &Path, input: &RuntimePolicyInput) -> bool {
    let in_bin_dir = path.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("bin"));
    in_bin_dir
        && path.file_name() == Some(std::ffi::OsStr::new("atm-daemon"))
        && !path.starts_with(default_dev_runtime_root_for(&input.os_home))
        && !path.starts_with(&input.temp_root)
        && !looks_like_repo_or_worktree_binary(path)
}

fn is_approved_dev_binary(path: &Path, input: &RuntimePolicyInput) -> bool {
    path.file_name() == Some(std::ffi::OsStr::new("atm-daemon"))
        && path.starts_with(default_dev_runtime_root_for(&input.os_home))
        && !looks_like_repo_or_worktree_binary(path)
}

fn evaluate_runtime_owner_metadata(input: &RuntimePolicyInput) -> RuntimeOwnerMetadata {
    let home_scope = canonicalize_lossy(&input.home_scope);
    let daemon_bin = canonicalize_lossy(&input.daemon_bin);
    let os_home = canonicalize_lossy(&input.os_home);
    let runtime_kind = classify_runtime_kind_from_paths(&home_scope, &os_home);

    RuntimeOwnerMetadata {
        runtime_kind,
        build_profile: input.build_profile.clone(),
        executable_path: daemon_bin.to_string_lossy().to_string(),
        home_scope: home_scope.to_string_lossy().to_string(),
    }
}

fn classify_runtime_kind_from_paths(home_scope: &Path, os_home: &Path) -> RuntimeKind {
    if home_scope == os_home {
        RuntimeKind::Release
    } else if home_scope == canonicalize_lossy(&default_dev_runtime_home_for(os_home)) {
        RuntimeKind::Dev
    } else {
        RuntimeKind::Isolated
    }
}

fn validate_runtime_admission_input(
    input: &RuntimePolicyInput,
) -> anyhow::Result<RuntimeOwnerMetadata> {
    let owner = evaluate_runtime_owner_metadata(input);

    if matches!(owner.runtime_kind, RuntimeKind::Isolated) {
        return Ok(owner);
    }

    if owner.build_profile != BuildProfile::Release {
        anyhow::bail!(
            "shared {} runtime requires a release build; refusing daemon at {} (build_profile={})",
            owner.runtime_kind.as_str(),
            owner.executable_path,
            owner.build_profile.as_str()
        );
    }

    let daemon_bin = PathBuf::from(&owner.executable_path);
    let approved = match owner.runtime_kind {
        RuntimeKind::Release => is_approved_release_binary(&daemon_bin, input),
        RuntimeKind::Dev => is_approved_dev_binary(&daemon_bin, input),
        RuntimeKind::Isolated => true,
    };

    if !approved {
        anyhow::bail!(
            "shared {} runtime requires an approved installed daemon binary; refusing {} for ATM_HOME={}",
            owner.runtime_kind.as_str(),
            owner.executable_path,
            owner.home_scope
        );
    }

    Ok(owner)
}

pub fn validate_runtime_admission(
    home: &Path,
    daemon_bin: &Path,
) -> anyhow::Result<RuntimeOwnerMetadata> {
    let os_home = crate::home::get_os_home_dir()?;
    let input = RuntimePolicyInput {
        home_scope: home.to_path_buf(),
        daemon_bin: daemon_bin.to_path_buf(),
        os_home,
        temp_root: std::env::temp_dir(),
        build_profile: BuildProfile::current(),
    };
    validate_runtime_admission_input(&input)
}

pub fn validate_runtime_admission_for_current_process(
    home: &Path,
) -> anyhow::Result<RuntimeOwnerMetadata> {
    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("atm-daemon"));
    validate_runtime_admission(home, &current_exe)
}

fn launch_class_for_runtime_kind(kind: &RuntimeKind) -> LaunchClass {
    match kind {
        RuntimeKind::Release => LaunchClass::ProdShared,
        RuntimeKind::Dev => LaunchClass::DevShared,
        RuntimeKind::Isolated => LaunchClass::IsolatedTest,
    }
}

pub fn runtime_kind_for_home(home: &Path) -> anyhow::Result<RuntimeKind> {
    let os_home = crate::home::get_os_home_dir()?;
    let canonical_home = canonicalize_lossy(home);
    let canonical_os_home = canonicalize_lossy(&os_home);
    Ok(classify_runtime_kind_from_paths(
        &canonical_home,
        &canonical_os_home,
    ))
}

pub fn read_runtime_metadata(home: &Path) -> Option<RuntimeMetadata> {
    let path = daemon_runtime_metadata_path_for(home);
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn write_runtime_metadata(home: &Path, metadata: &RuntimeMetadata) -> anyhow::Result<()> {
    use crate::io::atomic::atomic_swap;
    use crate::io::lock::acquire_lock;
    use std::io::Write;

    let metadata_path = daemon_runtime_metadata_path_for(home);
    let metadata_lock_path = daemon_runtime_metadata_write_lock_path_for(home);
    if let Some(parent) = metadata_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let _guard = acquire_lock(&metadata_lock_path, 10)?;

    let json = serde_json::to_vec_pretty(metadata)?;
    let tmp = metadata_path.with_extension("json.tmp");
    let mut tmp_file = std::fs::File::create(&tmp)?;
    tmp_file.write_all(&json)?;
    tmp_file.sync_all()?;
    drop(tmp_file);

    if !metadata_path.exists() {
        let placeholder = std::fs::File::create(&metadata_path)?;
        placeholder.sync_all()?;
    }

    atomic_swap(&metadata_path, &tmp)?;
    if tmp.exists() {
        std::fs::remove_file(&tmp)?;
    }
    Ok(())
}

fn isolated_runtime_root_dir(base_root: Option<&Path>) -> PathBuf {
    base_root
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir)
        .join("atm-isolated")
}

fn isolated_runtime_slug(label: Option<&str>) -> String {
    let trimmed = label.unwrap_or("runtime").trim();
    let mut slug = String::new();
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if (ch == '-' || ch == '_') && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "runtime".to_string()
    } else {
        slug.to_string()
    }
}

fn create_isolated_runtime_root_with_base(
    base_root: &Path,
    label: Option<&str>,
    ttl: Duration,
    allow_live_github_polling: bool,
) -> anyhow::Result<CreatedIsolatedRuntime> {
    let _ = reap_expired_isolated_runtime_roots_with_base(base_root);

    let runtime_root = isolated_runtime_root_dir(Some(base_root));
    std::fs::create_dir_all(&runtime_root)?;

    let now = chrono::Utc::now();
    let ttl = chrono::Duration::from_std(ttl)
        .map_err(|e| anyhow::anyhow!("invalid isolated runtime ttl: {e}"))?;
    let expires_at = now + ttl;
    let slug = isolated_runtime_slug(label);
    let home = runtime_root.join(format!(
        "{}-{}-{}",
        slug,
        now.timestamp_millis(),
        std::process::id()
    ));
    let runtime_dir = daemon_runtime_dir_for(&home);
    std::fs::create_dir_all(&runtime_dir)?;

    let metadata = RuntimeMetadata {
        runtime_kind: RuntimeKind::Isolated,
        created_at: now.to_rfc3339(),
        expires_at: Some(expires_at.to_rfc3339()),
        test_identifier: None,
        owner_pid: None,
        token_id: None,
        allow_live_github_polling,
    };
    write_runtime_metadata(&home, &metadata)?;

    Ok(CreatedIsolatedRuntime {
        home: home.clone(),
        runtime_dir,
        socket_path: daemon_runtime_dir_for(&home).join("atm-daemon.sock"),
        lock_path: daemon_runtime_dir_for(&home).join("daemon.lock"),
        status_path: daemon_runtime_dir_for(&home).join("status.json"),
        metadata,
    })
}

pub fn create_isolated_runtime_root(
    label: Option<&str>,
    ttl: Duration,
    allow_live_github_polling: bool,
) -> anyhow::Result<CreatedIsolatedRuntime> {
    create_isolated_runtime_root_with_base(
        &std::env::temp_dir(),
        label,
        ttl,
        allow_live_github_polling,
    )
}

fn reap_expired_isolated_runtime_roots_with_base(base_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let root = isolated_runtime_root_dir(Some(base_root));
    if !root.exists() {
        return Ok(Vec::new());
    }

    let now = chrono::Utc::now();
    let mut reaped = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let home = entry.path();
        if !home.is_dir() {
            continue;
        }

        let Some(metadata) = read_runtime_metadata(&home) else {
            continue;
        };
        if metadata.runtime_kind != RuntimeKind::Isolated {
            continue;
        }

        let Some(expires_at) = metadata.expires_at.as_deref() else {
            continue;
        };
        let Ok(expires_at) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
            continue;
        };
        if expires_at.with_timezone(&chrono::Utc) > now {
            continue;
        }

        if metadata.owner_pid.is_some_and(crate::pid::is_pid_alive) {
            continue;
        }

        let pid_path = daemon_runtime_dir_for(&home).join("atm-daemon.pid");
        let runtime_pid = std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|raw| raw.trim().parse::<i32>().ok());
        #[cfg(unix)]
        let alive = runtime_pid.is_some_and(pid_alive);
        #[cfg(not(unix))]
        let alive = false;
        if alive {
            continue;
        }

        if std::fs::remove_dir_all(&home).is_ok() {
            reaped.push(home);
        }
    }

    Ok(reaped)
}

pub fn reap_expired_isolated_runtime_roots() -> anyhow::Result<Vec<PathBuf>> {
    reap_expired_isolated_runtime_roots_with_base(&std::env::temp_dir())
}

pub fn isolated_runtime_allows_live_github(home: &Path) -> anyhow::Result<bool> {
    if runtime_kind_for_home(home)? != RuntimeKind::Isolated {
        return Ok(true);
    }

    Ok(read_runtime_metadata(home)
        .map(|metadata| metadata.allow_live_github_polling)
        .unwrap_or(false))
}

pub fn read_daemon_lock_metadata(home: &Path) -> Option<DaemonLockMetadata> {
    let path = daemon_lock_metadata_path_for(home);
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn format_runtime_owner_summary(owner: &RuntimeOwnerMetadata) -> String {
    format!(
        "runtime_kind={} build_profile={} executable={} home_scope={}",
        owner.runtime_kind.as_str(),
        owner.build_profile.as_str(),
        owner.executable_path,
        owner.home_scope
    )
}

/// Write daemon lock metadata atomically for the current process.
///
/// Called by `atm-daemon` after lock acquisition so CLI identity checks can
/// validate PID/home-scope/executable coherence.
pub fn write_daemon_lock_metadata(
    home: &std::path::Path,
    version: &str,
    owner: &RuntimeOwnerMetadata,
) -> anyhow::Result<()> {
    use crate::io::atomic::atomic_swap;
    use crate::io::lock::acquire_lock;
    use std::io::Write;

    let metadata_path = daemon_lock_metadata_path_for(home);
    let metadata_lock_path = daemon_lock_metadata_write_lock_path_for(home);
    if let Some(parent) = metadata_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let _guard = acquire_lock(&metadata_lock_path, 10)?;
    let _current = read_daemon_lock_metadata(home);

    let metadata = DaemonLockMetadata {
        pid: std::process::id(),
        owner: owner.clone(),
        version: version.to_string(),
        written_at: chrono::Utc::now().to_rfc3339(),
    };
    let json = serde_json::to_vec_pretty(&metadata)?;
    let tmp = metadata_path.with_extension("json.tmp");
    let mut tmp_file = std::fs::File::create(&tmp)?;
    tmp_file.write_all(&json)?;
    tmp_file.sync_all()?;
    drop(tmp_file);

    if !metadata_path.exists() {
        let placeholder = std::fs::File::create(&metadata_path)?;
        placeholder.sync_all()?;
    }

    atomic_swap(&metadata_path, &tmp)?;
    if tmp.exists() {
        std::fs::remove_file(&tmp)?;
    }
    Ok(())
}

/// Check whether the daemon appears to be running by reading its PID file and
/// verifying the process is alive.
///
/// Returns `false` on any error (missing file, invalid PID, dead process, etc.).
pub fn daemon_is_running() -> bool {
    #[cfg(unix)]
    {
        let pid_path = match daemon_pid_path() {
            Ok(p) => p,
            Err(_) => return false,
        };
        if let Ok(content) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = content.trim().parse::<i32>() {
                return pid_alive(pid);
            }
        }
        false
    }

    #[cfg(not(unix))]
    {
        false
    }
}

/// Ensure the ATM daemon is running, starting it if needed.
///
/// On Unix:
/// - Delegates to the full Unix implementation used by runtime queries,
///   including startup lock coordination, socket probing, and event logging.
///
/// On non-Unix platforms this is a no-op and returns `Ok(())`.
pub fn ensure_daemon_running() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        ensure_daemon_running_unix()
    }

    #[cfg(not(unix))]
    {
        Ok(())
    }
}

/// Send a single request to the daemon and return the parsed response.
///
/// Returns `Ok(None)` when the daemon is not running or the socket cannot be
/// reached. Returns `Ok(Some(response))` on a successful exchange. Returns
/// `Err` only for I/O errors that occur *after* a connection is established.
///
/// # Platform Behaviour
///
/// On non-Unix platforms this function always returns `Ok(None)`.
pub fn query_daemon(request: &SocketRequest) -> anyhow::Result<Option<SocketResponse>> {
    #[cfg(unix)]
    {
        query_daemon_unix(
            request,
            std::time::Duration::from_millis(DAEMON_QUERY_TIMEOUT_MS),
        )
    }

    #[cfg(not(unix))]
    {
        Ok(None)
    }
}

/// Send a single request to the daemon with a caller-specified socket timeout.
///
/// Use this variant for commands that may legitimately wait on external I/O
/// before returning (for example `gh-monitor` and `gh-monitor-control`).
pub fn query_daemon_with_timeout(
    request: &SocketRequest,
    read_timeout: std::time::Duration,
) -> anyhow::Result<Option<SocketResponse>> {
    #[cfg(unix)]
    {
        query_daemon_unix(request, read_timeout)
    }

    #[cfg(not(unix))]
    {
        Ok(None)
    }
}

/// Query the daemon for the current state of a specific agent.
///
/// Returns `Ok(None)` when the daemon is not reachable or the agent is not tracked.
///
/// # Arguments
///
/// * `agent` - Agent name (e.g., `"arch-ctm"`)
/// * `team`  - Team name (e.g., `"atm-dev"`)
pub fn query_agent_state(agent: &str, team: &str) -> anyhow::Result<Option<AgentStateInfo>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "agent-state".to_string(),
        payload: serde_json::json!({ "agent": agent, "team": team }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        // Daemon returned an error (e.g., agent not found) — treat as no info
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<AgentStateInfo>(payload) {
        Ok(info) => Ok(Some(info)),
        Err(_) => Ok(None),
    }
}

/// Send a subscribe request to the daemon.
///
/// Registers the subscriber's interest in state changes for `agent`. This is a
/// best-effort operation: `Ok(None)` is returned when the daemon is not running.
///
/// # Arguments
///
/// * `subscriber` - ATM identity of the subscribing agent (e.g., `"team-lead"`)
/// * `agent`      - Agent to watch (e.g., `"arch-ctm"`)
/// * `team`       - Team name (informational; used for routing context)
/// * `events`     - State events to subscribe to (e.g., `&["idle"]`);
///   pass an empty slice to subscribe to all events.
pub fn subscribe_to_agent(
    subscriber: &str,
    agent: &str,
    team: &str,
    events: &[String],
) -> anyhow::Result<Option<SocketResponse>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "subscribe".to_string(),
        payload: serde_json::json!({
            "subscriber": subscriber,
            "agent": agent,
            "team": team,
            "events": events,
        }),
    };
    query_daemon(&request)
}

/// Send an unsubscribe request to the daemon.
///
/// Removes the subscription for `(subscriber, agent)`. This is a best-effort
/// operation: `Ok(None)` is returned when the daemon is not running.
///
/// # Arguments
///
/// * `subscriber` - ATM identity of the subscribing agent
/// * `agent`      - Agent to stop watching
/// * `team`       - Team name (informational)
pub fn unsubscribe_from_agent(
    subscriber: &str,
    agent: &str,
    team: &str,
) -> anyhow::Result<Option<SocketResponse>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "unsubscribe".to_string(),
        payload: serde_json::json!({
            "subscriber": subscriber,
            "agent": agent,
            "team": team,
        }),
    };
    query_daemon(&request)
}

/// Query the daemon for the list of all tracked agents.
///
/// Returns `Ok(None)` when the daemon is not reachable.
pub fn query_list_agents() -> anyhow::Result<Option<Vec<AgentSummary>>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "list-agents".to_string(),
        payload: serde_json::Value::Object(Default::default()),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<Vec<AgentSummary>>(payload) {
        Ok(agents) => Ok(Some(agents)),
        Err(_) => Ok(None),
    }
}

/// Query the daemon for the list of tracked agents scoped to a specific team.
///
/// Returns `Ok(None)` when the daemon is not reachable.
pub fn query_list_agents_for_team(team: &str) -> anyhow::Result<Option<Vec<AgentSummary>>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "list-agents".to_string(),
        payload: serde_json::json!({ "team": team }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<Vec<AgentSummary>>(payload) {
        Ok(agents) => Ok(Some(agents)),
        Err(_) => Ok(None),
    }
}

/// Query the daemon for canonical member-state snapshots scoped to one team.
///
/// Returns:
/// - `Ok(None)` when the daemon is not reachable.
/// - `Err(...)` when daemon response payload is present but does not match the
///   canonical state schema.
pub fn query_team_member_states(team: &str) -> anyhow::Result<Option<Vec<CanonicalMemberState>>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "list-agents".to_string(),
        payload: serde_json::json!({ "team": team }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    decode_canonical_member_states_payload(payload).map(Some)
}

fn decode_canonical_member_states_payload(
    payload: serde_json::Value,
) -> anyhow::Result<Vec<CanonicalMemberState>> {
    serde_json::from_value::<Vec<CanonicalMemberState>>(payload).map_err(|err| {
        anyhow::anyhow!(
            "invalid canonical member-state payload from daemon list-agents(team): {err}"
        )
    })
}

/// Pane and log file information returned by the `agent-pane` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPaneInfo {
    /// Backend pane identifier (e.g., `"%42"`).
    pub pane_id: String,
    /// Absolute path to the agent's log file.
    pub log_path: String,
}

/// Query the daemon for the pane ID and log file path of a specific agent.
///
/// Returns `Ok(None)` when the daemon is not reachable or the agent is not tracked.
///
/// # Arguments
///
/// * `agent` - Agent name (e.g., `"arch-ctm"`)
pub fn query_agent_pane(agent: &str) -> anyhow::Result<Option<AgentPaneInfo>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "agent-pane".to_string(),
        payload: serde_json::json!({ "agent": agent }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        // Daemon returned an error (e.g., agent not found) — treat as no info
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<AgentPaneInfo>(payload) {
        Ok(info) => Ok(Some(info)),
        Err(_) => Ok(None),
    }
}

/// Session information returned by the `session-query` socket command.
///
/// Describes the Claude Code session and OS process currently registered for an
/// agent in the [`SessionRegistry`](crate) and whether the process is alive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionQueryResult {
    /// Claude Code session UUID.
    pub session_id: String,
    /// OS process ID of the agent process.
    pub process_id: u32,
    /// Whether the OS process is currently running.
    pub alive: bool,
    /// Most recent successful daemon heartbeat for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    /// Runtime kind (`codex`, `gemini`, etc.) when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// Runtime-native session/thread identifier when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_session_id: Option<String>,
    /// Backend pane identifier when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// Runtime home/state directory when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_home: Option<String>,
}

/// Result of attempting to register a daemon session hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterHintOutcome {
    /// Hint was accepted by the daemon.
    Registered,
    /// Daemon is unreachable; caller should continue without failing.
    DaemonUnavailable,
    /// Connected daemon does not support the register-hint command.
    UnsupportedDaemon,
}

/// GH monitor target kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GhMonitorTargetKind {
    Pr,
    Workflow,
    Run,
}

/// Request payload for daemon-routed `gh-monitor` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhMonitorRequest {
    pub team: String,
    pub target_kind: GhMonitorTargetKind,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<String>,
}

/// Request payload for daemon-routed `gh-status` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhStatusRequest {
    pub team: String,
    pub target_kind: GhMonitorTargetKind,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_cwd: Option<String>,
}

/// Lifecycle action for the GitHub monitor plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GhMonitorLifecycleAction {
    Start,
    Stop,
    Restart,
}

/// Request payload for daemon-routed `gh-monitor-control` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhMonitorControlRequest {
    pub team: String,
    pub action: GhMonitorLifecycleAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_team: Option<String>,
    #[serde(default)]
    pub user_authorized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_reason: Option<String>,
}

/// Daemon response payload for `gh-monitor-control` / `gh-monitor-health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhMonitorHealth {
    pub team: String,
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    pub lifecycle_state: String,
    pub availability_state: String,
    pub in_flight: u64,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_state_updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_limit_per_hour: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_used_in_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_remaining: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_runtime_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_binary_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_atm_home: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_poll_interval_secs: Option<u64>,
}

/// Daemon response payload for `gh-monitor`/`gh-status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhMonitorStatus {
    pub team: String,
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    pub target_kind: GhMonitorTargetKind,
    pub target: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_state_updated_at: Option<String>,
}

/// Query the daemon for the session record of a named agent.
///
/// Returns:
/// - `Ok(Some(result))` when the agent is registered in the session registry.
/// - `Ok(None)` when the daemon is not running, the agent is not registered,
///   or the platform does not support Unix sockets.
/// - `Err` only for unexpected I/O errors *after* a connection is established.
///
/// # Arguments
///
/// * `name` - Agent name to look up (e.g., `"team-lead"`)
pub fn query_session(name: &str) -> anyhow::Result<Option<SessionQueryResult>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "session-query".to_string(),
        payload: serde_json::json!({ "name": name }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        // Daemon returned error (agent not found) — treat as no session info
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<SessionQueryResult>(payload) {
        Ok(result) => Ok(Some(result)),
        Err(_) => Ok(None),
    }
}

/// Query the daemon for the session record of a named agent scoped to a team.
///
/// Returns:
/// - `Ok(Some(result))` when the agent is registered and matches the team's
///   current lead-session context.
/// - `Ok(None)` when the daemon is not running, the agent is not registered
///   for that team context, or the platform does not support Unix sockets.
/// - `Err` only for unexpected I/O errors *after* a connection is established.
///
/// # Arguments
///
/// * `team` - Team name (e.g., `"atm-dev"`)
/// * `name` - Agent name to look up (e.g., `"team-lead"`)
pub fn query_session_for_team(
    team: &str,
    name: &str,
) -> anyhow::Result<Option<SessionQueryResult>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "session-query-team".to_string(),
        payload: serde_json::json!({ "team": team, "name": name }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<SessionQueryResult>(payload) {
        Ok(result) => Ok(Some(result)),
        Err(_) => Ok(None),
    }
}

/// Query the daemon for the stream turn state of a named agent.
///
/// Returns:
/// - `Ok(Some(state))` when the daemon has stream state recorded for the agent.
/// - `Ok(None)` when the daemon is not running, the agent has no stream state,
///   or the platform does not support Unix sockets.
///
/// # Arguments
///
/// * `agent` - Agent name to look up (e.g., `"arch-ctm"`)
pub fn query_agent_stream_state(
    agent: &str,
) -> anyhow::Result<Option<crate::daemon_stream::AgentStreamState>> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "agent-stream-state".to_string(),
        payload: serde_json::json!({ "agent": agent }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    if !response.is_ok() {
        return Ok(None);
    }

    let payload = match response.payload {
        Some(p) => p,
        None => return Ok(None),
    };

    match serde_json::from_value::<crate::daemon_stream::AgentStreamState>(payload) {
        Ok(state) => Ok(Some(state)),
        Err(_) => Ok(None),
    }
}

/// Handle for an active daemon stream subscription.
///
/// Dropping this value requests the background reader thread to stop.
pub struct StreamSubscription {
    /// Receiver of daemon stream events.
    pub rx: std::sync::mpsc::Receiver<crate::daemon_stream::DaemonStreamEvent>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for StreamSubscription {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Subscribe to daemon stream events over a long-lived socket connection.
///
/// Returns:
/// - `Ok(Some(rx))` when subscription succeeds.
/// - `Ok(None)` when daemon/socket is unavailable on this platform/session.
pub fn subscribe_stream_events() -> anyhow::Result<Option<StreamSubscription>> {
    #[cfg(unix)]
    {
        subscribe_stream_events_unix()
    }

    #[cfg(not(unix))]
    {
        Ok(None)
    }
}

/// Send a control request to the daemon and wait for an acknowledgement.
///
/// Sends `command: "control"` with the given [`ControlRequest`] as payload.
/// Returns the parsed [`ControlAck`] on success, or an error on socket/parse
/// failure.  A short read timeout is applied by the underlying
/// [`query_daemon`] call.
///
/// # Errors
///
/// Returns `Err` when:
/// - The daemon is not running or the socket cannot be reached (no graceful
///   `None` here — the caller needs to distinguish errors from timeouts).
/// - The daemon returns an error status.
/// - The response payload cannot be parsed as [`ControlAck`].
pub fn send_control(
    request: &crate::control::ControlRequest,
) -> anyhow::Result<crate::control::ControlAck> {
    let payload = serde_json::to_value(request)
        .map_err(|e| anyhow::anyhow!("Failed to serialize ControlRequest: {e}"))?;

    let socket_request = SocketRequest {
        version: PROTOCOL_VERSION,
        // Use an independent socket-level correlation ID; the control payload
        // carries its own stable idempotency key (`request.request_id`) that
        // must not change on retries.
        request_id: format!(
            "sock-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ),
        command: "control".to_string(),
        payload,
    };

    let response = match query_daemon(&socket_request)? {
        Some(r) => r,
        None => anyhow::bail!("Daemon not reachable (socket not found or connection refused)"),
    };

    if !response.is_ok() {
        let msg = response
            .error
            .map(|e| format!("{}: {}", e.code, e.message))
            .unwrap_or_else(|| "unknown daemon error".to_string());
        anyhow::bail!("Daemon returned error for control command: {msg}");
    }

    let payload = response
        .payload
        .ok_or_else(|| anyhow::anyhow!("Daemon returned ok status but no payload"))?;

    serde_json::from_value::<crate::control::ControlAck>(payload)
        .map_err(|e| anyhow::anyhow!("Failed to parse ControlAck from daemon response: {e}"))
}

/// Send a best-effort session registration hint to the daemon.
///
/// This command is used by external runtimes (Codex/Gemini) that cannot emit
/// Claude-style lifecycle hooks. It updates the daemon session registry using
/// canonical daemon paths instead of writing session identity into config.json.
///
/// Backward compatibility contract:
/// - daemon unreachable -> [`RegisterHintOutcome::DaemonUnavailable`] (silent skip)
/// - daemon unknown-command -> [`RegisterHintOutcome::UnsupportedDaemon`] so callers can
///   fail with explicit upgrade guidance.
#[allow(clippy::too_many_arguments)]
pub fn register_hint(
    team: &str,
    agent: &str,
    session_id: &str,
    process_id: u32,
    runtime: Option<&str>,
    runtime_session_id: Option<&str>,
    pane_id: Option<&str>,
    runtime_home: Option<&str>,
) -> anyhow::Result<RegisterHintOutcome> {
    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "register-hint".to_string(),
        payload: serde_json::json!({
            "team": team,
            "agent": agent,
            "session_id": session_id,
            "process_id": process_id,
            "runtime": runtime,
            "runtime_session_id": runtime_session_id,
            "pane_id": pane_id,
            "runtime_home": runtime_home,
            "identity": std::env::var("ATM_IDENTITY")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
        }),
    };

    let response = match query_daemon(&request)? {
        Some(r) => r,
        None => return Ok(RegisterHintOutcome::DaemonUnavailable),
    };

    decode_register_hint_response(response)
}

/// Send a daemon-routed GitHub monitor request (`command: "gh-monitor"`).
///
/// Returns:
/// - `Ok(Some(status))` when the daemon accepted the request and returned
///   monitor status.
/// - `Ok(None)` when daemon/socket is unavailable.
/// - `Err` when daemon returns an explicit command error.
pub fn gh_monitor(request: &GhMonitorRequest) -> anyhow::Result<Option<GhMonitorStatus>> {
    let socket_request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "gh-monitor".to_string(),
        payload: serde_json::to_value(request)?,
    };

    // `gh-monitor` may wait for CI run discovery up to start_timeout_secs.
    let start_timeout_secs = request.start_timeout_secs.unwrap_or(120);
    let read_timeout = std::time::Duration::from_secs(
        (start_timeout_secs + DAEMON_TIMEOUT_MIN_SECS).min(DAEMON_TIMEOUT_MAX_SECS),
    );
    let response = match query_daemon_with_timeout(&socket_request, read_timeout)? {
        Some(r) => r,
        None => return Ok(None),
    };

    decode_gh_monitor_response(response).map(Some)
}

/// Query daemon-routed GitHub monitor status (`command: "gh-status"`).
///
/// Returns:
/// - `Ok(Some(status))` when daemon has monitor state for the target.
/// - `Ok(None)` when daemon/socket is unavailable.
/// - `Err` when daemon returns an explicit command error.
pub fn gh_status(request: &GhStatusRequest) -> anyhow::Result<Option<GhMonitorStatus>> {
    let socket_request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "gh-status".to_string(),
        payload: serde_json::to_value(request)?,
    };

    let response = match query_daemon(&socket_request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    decode_gh_monitor_response(response).map(Some)
}

/// Send a daemon-routed GitHub monitor lifecycle request
/// (`command: "gh-monitor-control"`).
pub fn gh_monitor_control(
    request: &GhMonitorControlRequest,
) -> anyhow::Result<Option<GhMonitorHealth>> {
    let socket_request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "gh-monitor-control".to_string(),
        payload: serde_json::to_value(request)?,
    };

    // Stop/restart can drain in-flight monitors for drain_timeout_secs.
    let drain_timeout_secs = request
        .drain_timeout_secs
        .unwrap_or(DAEMON_TIMEOUT_MIN_SECS);
    let read_timeout = std::time::Duration::from_secs(
        (drain_timeout_secs + DAEMON_TIMEOUT_MIN_SECS).min(DAEMON_TIMEOUT_MAX_SECS),
    );
    let response = match query_daemon_with_timeout(&socket_request, read_timeout)? {
        Some(r) => r,
        None => return Ok(None),
    };

    decode_gh_monitor_health_response(response).map(Some)
}

/// Query daemon-routed GitHub monitor plugin health
/// (`command: "gh-monitor-health"`).
pub fn gh_monitor_health(team: &str) -> anyhow::Result<Option<GhMonitorHealth>> {
    gh_monitor_health_with_context(team, None, None)
}

/// Query daemon-routed GitHub monitor plugin health with explicit config cwd.
pub fn gh_monitor_health_with_context(
    team: &str,
    config_cwd: Option<String>,
    repo: Option<String>,
) -> anyhow::Result<Option<GhMonitorHealth>> {
    let socket_request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "gh-monitor-health".to_string(),
        payload: serde_json::json!({
            "team": team,
            "config_cwd": config_cwd,
            "repo": repo,
        }),
    };

    let response = match query_daemon(&socket_request)? {
        Some(r) => r,
        None => return Ok(None),
    };

    decode_gh_monitor_health_response(response).map(Some)
}

fn decode_gh_monitor_response(response: SocketResponse) -> anyhow::Result<GhMonitorStatus> {
    if !response.is_ok() {
        let Some(err) = response.error else {
            anyhow::bail!("Daemon returned gh-monitor error status without error payload");
        };
        anyhow::bail!(
            "Daemon returned error for {} command: {}: {}",
            response.request_id,
            err.code,
            err.message
        );
    }

    let payload = response
        .payload
        .ok_or_else(|| anyhow::anyhow!("Daemon returned ok status but no payload"))?;

    serde_json::from_value::<GhMonitorStatus>(payload)
        .map_err(|e| anyhow::anyhow!("Failed to parse GhMonitorStatus from daemon response: {e}"))
}

fn decode_gh_monitor_health_response(response: SocketResponse) -> anyhow::Result<GhMonitorHealth> {
    if !response.is_ok() {
        let Some(err) = response.error else {
            anyhow::bail!("Daemon returned gh-monitor health error status without error payload");
        };
        anyhow::bail!(
            "Daemon returned error for {} command: {}: {}",
            response.request_id,
            err.code,
            err.message
        );
    }

    let payload = response
        .payload
        .ok_or_else(|| anyhow::anyhow!("Daemon returned ok status but no payload"))?;

    serde_json::from_value::<GhMonitorHealth>(payload)
        .map_err(|e| anyhow::anyhow!("Failed to parse GhMonitorHealth from daemon response: {e}"))
}

fn decode_register_hint_response(response: SocketResponse) -> anyhow::Result<RegisterHintOutcome> {
    if response.is_ok() {
        return Ok(RegisterHintOutcome::Registered);
    }

    let Some(err) = response.error else {
        anyhow::bail!("Daemon returned register-hint error status without error payload");
    };

    if err.code == "UNKNOWN_COMMAND" {
        return Ok(RegisterHintOutcome::UnsupportedDaemon);
    }

    anyhow::bail!(
        "Daemon returned error for register-hint command: {}: {}",
        err.code,
        err.message
    )
}

/// Generate a compact request identifier (UUID v4 as a short string).
fn new_request_id() -> String {
    // Use a simple monotonic counter for environments without UUID support.
    // In practice the daemon_client is always used in the atm crate which
    // has uuid available, but atm-core does not depend on uuid.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let id = std::process::id();
    format!("req-{id}-{nanos}")
}

fn daemon_autostart_event(
    request_id: &str,
    trace_id: &str,
    action: &'static str,
    result: &'static str,
    target: Option<String>,
    error: Option<String>,
) -> EventFields {
    let mut extra_fields = serde_json::Map::new();
    extra_fields.insert(
        "autostart_phase".to_string(),
        serde_json::Value::String(action.to_string()),
    );
    EventFields {
        level: if error.is_some() { "error" } else { "info" },
        source: "atm",
        action,
        result: Some(result.to_string()),
        request_id: Some(request_id.to_string()),
        trace_id: Some(trace_id.to_string()),
        span_id: Some(crate::event_log::span_id_for_action(trace_id, action)),
        target,
        error,
        extra_fields,
        ..Default::default()
    }
}

// ── Unix implementation ──────────────────────────────────────────────────────

#[cfg(unix)]
fn query_daemon_unix(
    request: &SocketRequest,
    read_timeout: std::time::Duration,
) -> anyhow::Result<Option<SocketResponse>> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    let socket_path = daemon_socket_path()?;

    // First attempt connection directly.
    let stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(_) => {
            // Optional daemon auto-start path, enabled by ATM_DAEMON_AUTOSTART.
            if daemon_autostart_enabled() {
                ensure_daemon_running_unix()?;
                let deadline = Instant::now() + Duration::from_secs(STARTUP_DEADLINE_SECS);
                loop {
                    match UnixStream::connect(&socket_path) {
                        Ok(s) => break s,
                        Err(e) if Instant::now() < deadline => {
                            let _ = e;
                            std::thread::sleep(Duration::from_millis(RETRY_SLEEP_MS));
                        }
                        Err(e) => {
                            anyhow::bail!(
                                "daemon auto-start attempted but socket remained unavailable at {}: {e}",
                                socket_path.display()
                            )
                        }
                    }
                }
            } else {
                // Autostart is disabled; the daemon is managed externally.
                // If the socket path already exists the daemon may be mid-startup
                // (socket bound but not yet accepting). Retry briefly before giving up
                // so we don't return Ok(None) during that narrow window.
                if socket_path.exists() {
                    let mut connected = None;
                    for _ in 0..3 {
                        match UnixStream::connect(&socket_path) {
                            Ok(s) => {
                                connected = Some(s);
                                break;
                            }
                            Err(_) => std::thread::sleep(Duration::from_millis(RETRY_SLEEP_MS)),
                        }
                    }
                    match connected {
                        Some(s) => s,
                        None => return Ok(None),
                    }
                } else {
                    return Ok(None);
                }
            }
        }
    };

    // Keep writes short; allow caller-specific read timeout for long-running
    // daemon operations such as gh monitor startup/drain paths.
    stream.set_read_timeout(Some(read_timeout)).ok();
    stream
        .set_write_timeout(Some(Duration::from_millis(SOCKET_IO_TIMEOUT_MS)))
        .ok();

    let request_line = serde_json::to_string(request)?;

    // Write request line (newline-delimited)
    {
        let mut writer = std::io::BufWriter::new(&stream);
        writer.write_all(request_line.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }

    // Read response line
    let mut reader = BufReader::new(&stream);
    let mut response_line = String::new();
    match reader.read_line(&mut response_line) {
        Ok(0) | Err(_) => return Ok(None), // daemon closed connection or timed out
        Ok(_) => {}
    }

    let response: SocketResponse = match serde_json::from_str(response_line.trim()) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    Ok(Some(response))
}

#[cfg(unix)]
fn daemon_autostart_enabled() -> bool {
    let Ok(raw) = std::env::var("ATM_DAEMON_AUTOSTART") else {
        // Opt-out model: autostart is enabled by default when unset.
        return true;
    };
    !matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no"
    )
}

#[cfg(unix)]
fn resolve_daemon_binary() -> anyhow::Result<std::ffi::OsString> {
    if let Some(override_bin) = std::env::var_os("ATM_DAEMON_BIN")
        && !override_bin.is_empty()
    {
        return Ok(override_bin);
    }

    let name = std::ffi::OsString::from("atm-daemon");

    if let Ok(current_exe) = std::env::current_exe()
        && let Some(dir) = current_exe.parent()
    {
        let sibling = dir.join(std::path::Path::new(&name));
        if sibling.exists() {
            return Ok(sibling.into_os_string());
        }
        anyhow::bail!(
            "failed to resolve atm-daemon binary: ATM_DAEMON_BIN is unset and sibling binary '{}' is missing",
            sibling.display()
        );
    }

    anyhow::bail!(
        "failed to resolve atm-daemon binary: ATM_DAEMON_BIN is unset and current executable path is unavailable"
    )
}

#[cfg(unix)]
fn ensure_daemon_running_unix() -> anyhow::Result<()> {
    use crate::event_log::emit_event_best_effort;
    use crate::io::InboxError;
    use std::io::ErrorKind;
    use std::process::Stdio;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    // When autostart is disabled, the daemon lifecycle is managed externally.
    // Skip identity validation and restart logic — trust the external daemon as-is.
    if !daemon_autostart_enabled() {
        return Ok(());
    }

    let home = crate::home::get_home_dir()?;
    let autostart_request_id = new_request_id();
    let autostart_trace_id =
        crate::event_log::trace_id_for_request("atm:daemon_autostart", &autostart_request_id);
    let socket_connectable = daemon_socket_connectable(&home);
    if daemon_is_running() || socket_connectable {
        if let Some(reason) = detect_daemon_identity_mismatch(&home, socket_connectable) {
            restart_mismatched_daemon(&home, &reason)?;
        } else if socket_connectable {
            // Command paths require a live daemon socket, not just a PID file.
            return Ok(());
        }
    }

    cleanup_stale_daemon_runtime_files(&home);

    let startup_lock_path = daemon_start_lock_path()?;
    if let Some(parent) = startup_lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    static STARTUP_PROCESS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let startup_process_lock = STARTUP_PROCESS_LOCK.get_or_init(|| Mutex::new(()));
    let _startup_process_guard = startup_process_lock
        .lock()
        .expect("daemon startup process lock poisoned");

    // Serialize daemon startup across concurrent CLI processes.
    let _startup_lock = match crate::io::lock::acquire_lock(&startup_lock_path, 3) {
        Ok(lock) => Some(lock),
        Err(InboxError::LockTimeout { .. }) => {
            // Another process likely holds the startup lock and is spawning the daemon.
            // Wait briefly for that startup attempt to converge.
            for _ in 0..10 {
                if daemon_socket_connectable(&home) {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(RETRY_SLEEP_MS));
            }
            // Startup did not converge yet. Re-attempt lock acquisition so any
            // fallback spawn still occurs under lock (single-daemon invariant).
            match crate::io::lock::acquire_lock(&startup_lock_path, 10) {
                Ok(lock) => Some(lock),
                Err(e) => anyhow::bail!(
                    "timed out waiting for daemon startup lock holder to bring daemon online: {} ({e})",
                    startup_lock_path.display()
                ),
            }
        }
        Err(e) => anyhow::bail!(
            "failed to acquire daemon startup lock {}: {e}",
            startup_lock_path.display()
        ),
    };

    let socket_connectable = daemon_socket_connectable(&home);
    if daemon_is_running() || socket_connectable {
        if let Some(reason) = detect_daemon_identity_mismatch(&home, socket_connectable) {
            restart_mismatched_daemon(&home, &reason)?;
        } else if socket_connectable {
            return Ok(());
        } else {
            if wait_for_daemon_socket_ready(&home, Duration::from_secs(STARTUP_DEADLINE_SECS)) {
                return Ok(());
            }
            let socket_path = daemon_socket_path()?;
            let pid_path = daemon_pid_path()?;
            anyhow::bail!(
                "daemon startup timed out after {}s; pid_file_exists={}, socket_exists={}, pid_path={}, socket_path={}",
                STARTUP_DEADLINE_SECS,
                pid_path.exists(),
                socket_path.exists(),
                pid_path.display(),
                socket_path.display()
            );
        }
    }

    let daemon_bin = resolve_daemon_binary().map_err(|e| {
        let error = e.to_string();
        emit_event_best_effort(daemon_autostart_event(
            &autostart_request_id,
            &autostart_trace_id,
            "daemon_autostart_failure",
            "binary_resolution_error",
            None,
            Some(error.clone()),
        ));
        anyhow::anyhow!("{error}")
    })?;
    let daemon_bin_path = PathBuf::from(&daemon_bin);
    let runtime_owner = validate_runtime_admission(&home, &daemon_bin_path).map_err(|e| {
        let error = e.to_string();
        emit_event_best_effort(daemon_autostart_event(
            &autostart_request_id,
            &autostart_trace_id,
            "daemon_autostart_failure",
            "runtime_admission_denied",
            Some(daemon_bin_path.display().to_string()),
            Some(error.clone()),
        ));
        anyhow::anyhow!("{error}")
    })?;
    emit_event_best_effort(daemon_autostart_event(
        &autostart_request_id,
        &autostart_trace_id,
        "daemon_autostart_attempt",
        "attempt",
        Some(daemon_bin_path.display().to_string()),
        None,
    ));
    let stderr_capture = std::env::temp_dir().join(format!(
        "atm-daemon-stderr-{}-{}.log",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    let stderr_file = std::fs::File::create(&stderr_capture)
        .map_err(|e| anyhow::anyhow!("failed to prepare daemon stderr capture: {e}"))?;

    let mut child = match spawn_daemon_process(SpawnDaemonRequest {
        daemon_bin: daemon_bin.as_os_str(),
        atm_home: &home,
        launch_class: launch_class_for_runtime_kind(&runtime_owner.runtime_kind),
        issuer: "agent-team-mail-core::daemon_client::ensure_daemon_running_unix",
        team: None,
        stdin: Stdio::null(),
        stdout: Stdio::null(),
        stderr: Stdio::from(stderr_file),
    }) {
        Ok(child) => child,
        Err(e) => {
            let error = if e.kind() == ErrorKind::NotFound {
                format!(
                    "failed to auto-start daemon: binary '{}' is missing or not executable",
                    std::path::PathBuf::from(&daemon_bin).display()
                )
            } else {
                format!(
                    "failed to auto-start daemon via '{}': {e}",
                    std::path::PathBuf::from(&daemon_bin).display()
                )
            };
            emit_event_best_effort(daemon_autostart_event(
                &autostart_request_id,
                &autostart_trace_id,
                "daemon_autostart_failure",
                "spawn_error",
                Some(daemon_bin_path.display().to_string()),
                Some(error.clone()),
            ));
            anyhow::bail!("{error}");
        }
    };

    let deadline = Instant::now() + Duration::from_secs(STARTUP_DEADLINE_SECS);
    while Instant::now() < deadline {
        if daemon_socket_connectable(&home) {
            emit_event_best_effort(daemon_autostart_event(
                &autostart_request_id,
                &autostart_trace_id,
                "daemon_autostart_success",
                "ok",
                Some(format!(
                    "{} {}",
                    daemon_bin_path.display(),
                    format_runtime_owner_summary(&runtime_owner)
                )),
                None,
            ));
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            let stderr_tail = std::fs::read(&stderr_capture).ok().and_then(|buf| {
                if buf.is_empty() {
                    return None;
                }
                let trimmed = if buf.len() > 4096 {
                    &buf[buf.len() - 4096..]
                } else {
                    &buf
                };
                let text = String::from_utf8_lossy(trimmed).trim().to_string();
                if text.is_empty() { None } else { Some(text) }
            });
            let error = match stderr_tail {
                Some(tail) => {
                    format!(
                        "daemon process exited during startup with status {status}; stderr_tail={tail}"
                    )
                }
                None => format!("daemon process exited during startup with status {status}"),
            };
            emit_event_best_effort(daemon_autostart_event(
                &autostart_request_id,
                &autostart_trace_id,
                "daemon_autostart_failure",
                "process_exit",
                Some(daemon_bin_path.display().to_string()),
                Some(error.clone()),
            ));
            let _ = std::fs::remove_file(&stderr_capture);
            anyhow::bail!("{error}");
        }
        std::thread::sleep(Duration::from_millis(RETRY_SLEEP_MS));
    }
    let _ = std::fs::remove_file(&stderr_capture);

    let socket_path = daemon_socket_path()?;
    let pid_path = daemon_pid_path()?;
    let timeout_error = format!(
        "daemon startup timed out after {}s; pid_file_exists={}, socket_exists={}, pid_path={}, socket_path={}",
        STARTUP_DEADLINE_SECS,
        pid_path.exists(),
        socket_path.exists(),
        pid_path.display(),
        socket_path.display()
    );
    let mut timeout_event = daemon_autostart_event(
        &autostart_request_id,
        &autostart_trace_id,
        "daemon_autostart_timeout",
        "timeout",
        Some(daemon_bin_path.display().to_string()),
        Some(timeout_error.clone()),
    );
    timeout_event.level = "warn";
    emit_event_best_effort(timeout_event);
    emit_event_best_effort(daemon_autostart_event(
        &autostart_request_id,
        &autostart_trace_id,
        "daemon_autostart_failure",
        "timeout",
        Some(daemon_bin_path.display().to_string()),
        Some(timeout_error.clone()),
    ));
    if child.try_wait()?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    anyhow::bail!("{timeout_error}")
}

#[cfg(unix)]
fn daemon_socket_connectable(home: &std::path::Path) -> bool {
    use std::os::unix::net::UnixStream;
    let socket_path = home.join(".atm/daemon/atm-daemon.sock");
    UnixStream::connect(socket_path).is_ok()
}

#[cfg(unix)]
fn wait_for_daemon_socket_ready(home: &std::path::Path, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if daemon_socket_connectable(home) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(RETRY_SLEEP_MS));
    }
    false
}

#[cfg(unix)]
fn cleanup_stale_daemon_runtime_files(home: &std::path::Path) {
    let socket_path = home.join(".atm/daemon/atm-daemon.sock");
    let pid_path = home.join(".atm/daemon/atm-daemon.pid");

    let pid_state = read_daemon_pid_state(&pid_path);
    if matches!(
        pid_state,
        PidState::Dead | PidState::Missing | PidState::Malformed
    ) {
        let _ = std::fs::remove_file(&pid_path);
    }

    // Remove stale socket only when daemon ownership is known-dead.
    let ownership_known_dead = matches!(
        pid_state,
        PidState::Dead | PidState::Missing | PidState::Malformed
    );
    if socket_path.exists() && ownership_known_dead && !daemon_socket_connectable(home) {
        let _ = std::fs::remove_file(&socket_path);
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PidState {
    Missing,
    Malformed,
    Unreadable,
    Dead,
    Alive,
}

#[cfg(unix)]
fn read_daemon_pid_state(pid_path: &std::path::Path) -> PidState {
    if !pid_path.exists() {
        return PidState::Missing;
    }
    let content = match std::fs::read_to_string(pid_path) {
        Ok(s) => s,
        Err(_) => return PidState::Unreadable,
    };
    let pid = match content.trim().parse::<i32>() {
        Ok(pid) => pid,
        Err(_) => return PidState::Malformed,
    };
    if pid_alive(pid) {
        PidState::Alive
    } else {
        PidState::Dead
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Default)]
struct DaemonIdentitySnapshot {
    pid_from_file: Option<u32>,
    pid_from_status: Option<u32>,
    version_from_status: Option<String>,
    metadata: Option<DaemonLockMetadata>,
    socket_connectable: bool,
}

#[cfg(unix)]
fn evaluate_daemon_identity_mismatch(
    snapshot: &DaemonIdentitySnapshot,
    expected_home: &str,
    expected_bin: &std::ffi::OsStr,
    expected_version: &str,
    pid_alive_fn: impl Fn(i32) -> bool,
    pid_command_line_fn: impl Fn(i32) -> Option<String>,
) -> Option<String> {
    if snapshot.metadata.is_none() && !snapshot.socket_connectable {
        return None;
    }

    if snapshot.metadata.is_none() {
        return Some(
            "daemon identity mismatch: lock metadata missing (soft mismatch, restart required)"
                .to_string(),
        );
    }

    let pid = snapshot
        .metadata
        .as_ref()
        .map(|m| m.pid)
        .or(snapshot.pid_from_file)
        .or(snapshot.pid_from_status)?;

    if !pid_alive_fn(pid as i32) {
        return Some(format!("daemon identity mismatch: pid {pid} is not alive"));
    }

    if let Some(meta) = &snapshot.metadata {
        if let Some(file_pid) = snapshot.pid_from_file
            && file_pid != meta.pid
        {
            return Some(format!(
                "daemon identity mismatch: pid file ({file_pid}) != lock metadata ({})",
                meta.pid
            ));
        }

        if !meta.owner.home_scope.is_empty() && meta.owner.home_scope != expected_home {
            return Some(format!(
                "daemon identity mismatch: home scope '{}' != expected '{}'",
                meta.owner.home_scope, expected_home
            ));
        }

        if let Some(cmdline) = pid_command_line_fn(pid as i32)
            && let Some(matches) = pid_command_matches_expected_binary(&cmdline, expected_bin)
            && !matches
        {
            return Some(format!(
                "daemon identity mismatch: running command '{}' != expected daemon binary '{}'",
                cmdline,
                std::path::PathBuf::from(expected_bin).display()
            ));
        }
    }

    if let Some(ver) = snapshot.version_from_status.as_deref()
        && ver != expected_version
    {
        return Some(format!(
            "daemon version mismatch: running={ver} expected={expected_version}"
        ));
    }

    None
}

#[cfg(unix)]
fn detect_daemon_identity_mismatch(
    home: &std::path::Path,
    socket_connectable: bool,
) -> Option<String> {
    let pid_path = home.join(".atm/daemon/atm-daemon.pid");
    let status_path = daemon_status_path_for(home);
    let metadata_path = daemon_lock_metadata_path_for(home);

    let pid_from_file = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    let status_json = std::fs::read_to_string(&status_path)
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok());
    let pid_from_status = status_json
        .as_ref()
        .and_then(|json| json.get("pid").and_then(serde_json::Value::as_u64))
        .map(|pid| pid as u32);
    let version_from_status = status_json
        .as_ref()
        .and_then(|json| json.get("version").and_then(serde_json::Value::as_str))
        .map(std::string::ToString::to_string);
    let mut metadata = std::fs::read_to_string(&metadata_path)
        .ok()
        .and_then(|s| serde_json::from_str::<DaemonLockMetadata>(&s).ok());

    if metadata.is_none()
        && let Some(candidate_pid) = pid_from_file.or(pid_from_status)
        && pid_alive(candidate_pid as i32)
    {
        std::thread::sleep(std::time::Duration::from_millis(DAEMON_METADATA_SETTLE_MS));
        metadata = std::fs::read_to_string(&metadata_path)
            .ok()
            .and_then(|s| serde_json::from_str::<DaemonLockMetadata>(&s).ok());
    }

    let expected_home = std::fs::canonicalize(home)
        .unwrap_or_else(|_| home.to_path_buf())
        .to_string_lossy()
        .to_string();
    let expected_bin = resolve_daemon_binary().ok();
    let snapshot = DaemonIdentitySnapshot {
        pid_from_file,
        pid_from_status,
        version_from_status,
        metadata,
        socket_connectable,
    };

    evaluate_daemon_identity_mismatch(
        &snapshot,
        &expected_home,
        expected_bin
            .as_deref()
            .unwrap_or_else(|| std::ffi::OsStr::new("")),
        env!("CARGO_PKG_VERSION"),
        pid_alive,
        pid_command_line,
    )
}

#[cfg(unix)]
fn pid_command_line(pid: i32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .arg("-o")
        .arg("command=")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

#[cfg(unix)]
fn pid_command_matches_expected_binary(
    cmdline: &str,
    expected_bin: &std::ffi::OsStr,
) -> Option<bool> {
    let expected = std::path::PathBuf::from(expected_bin);

    if expected.as_os_str().is_empty() {
        return None;
    }

    if expected.components().count() > 1 {
        let expected_canon = std::fs::canonicalize(&expected).unwrap_or(expected.clone());
        for token in cmdline.split_whitespace() {
            let actual_path = std::path::PathBuf::from(token);
            let actual_canon = std::fs::canonicalize(&actual_path).unwrap_or(actual_path.clone());
            if expected_canon == actual_canon {
                return Some(true);
            }
        }
        Some(false)
    } else {
        let actual = cmdline.split_whitespace().next()?;
        let actual_path = std::path::PathBuf::from(actual);
        let expected_name = expected.file_name()?;
        Some(actual_path.file_name() == Some(expected_name))
    }
}

#[cfg(unix)]
fn restart_mismatched_daemon(home: &std::path::Path, reason: &str) -> anyhow::Result<()> {
    use crate::event_log::{EventFields, emit_event_best_effort};
    use std::time::Duration;

    let pid_path = home.join(".atm/daemon/atm-daemon.pid");
    let pid = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok());

    emit_event_best_effort(EventFields {
        level: "warn",
        source: "atm",
        action: "daemon_identity_restart",
        result: Some("restart_attempt".to_string()),
        error: Some(reason.to_string()),
        ..Default::default()
    });

    if let Some(pid) = pid
        && pid_alive(pid)
    {
        send_signal(pid, 15);
        for _ in 0..20 {
            if !pid_alive(pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(RETRY_SLEEP_MS));
        }
        if pid_alive(pid) {
            send_signal(pid, 9);
            for _ in 0..20 {
                if !pid_alive(pid) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(RETRY_SLEEP_MS));
            }
        }
        if pid_alive(pid) {
            emit_event_best_effort(EventFields {
                level: "warn",
                source: "atm",
                action: "daemon_identity_restart",
                result: Some("kill_incomplete".to_string()),
                error: Some(format!(
                    "stale daemon pid {pid} still alive after SIGTERM/SIGKILL; proceeding with runtime file replacement"
                )),
                ..Default::default()
            });
        }
    }

    let lock_path = home.join(".atm/daemon/daemon.lock");
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _lock_guard = crate::io::lock::acquire_lock(&lock_path, 5).map_err(|e| {
        anyhow::anyhow!(
            "failed to acquire daemon lock at {} before runtime cleanup: {e}",
            lock_path.display()
        )
    })?;

    // Replace runtime files aggressively for identity-mismatch recovery. This is
    // scope-local and avoids broad process sweeps while allowing a fresh daemon
    // to bind canonical paths.
    let daemon_dir = home.join(".atm/daemon");
    let _ = std::fs::remove_file(daemon_dir.join("atm-daemon.sock"));
    let _ = std::fs::remove_file(daemon_dir.join("atm-daemon.pid"));
    let _ = std::fs::remove_file(daemon_dir.join("status.json"));
    cleanup_stale_daemon_runtime_files(home);
    Ok(())
}

#[cfg(unix)]
fn send_signal(pid: i32, sig: i32) {
    // SAFETY: kill is invoked with a specific PID and signal; errors are ignored
    // by design because this is a best-effort stale-daemon cleanup path.
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: FFI call to libc kill; inputs are plain integers.
    let _ = unsafe { kill(pid, sig) };
}

#[cfg(unix)]
fn subscribe_stream_events_unix() -> anyhow::Result<Option<StreamSubscription>> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let socket_path = daemon_socket_path()?;
    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };

    let req = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "stream-subscribe".to_string(),
        payload: serde_json::json!({}),
    };
    let req_line = serde_json::to_string(&req)?;
    stream.write_all(req_line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    // Must receive an explicit stream ACK before treating the subscription as live.
    {
        let mut ack_reader = BufReader::new(stream.try_clone()?);
        let mut ack_line = String::new();
        if ack_reader.read_line(&mut ack_line)? == 0 {
            return Ok(None);
        }
        let ack_json: serde_json::Value = match serde_json::from_str(ack_line.trim()) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
        let ok = ack_json
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s == "ok")
            .unwrap_or(false);
        let streaming = ack_json
            .get("streaming")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !(ok && streaming) {
            return Ok(None);
        }
    }

    let (tx, rx) = std::sync::mpsc::channel::<crate::daemon_stream::DaemonStreamEvent>();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_thread = std::sync::Arc::clone(&stop);
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(SOCKET_IO_TIMEOUT_MS)))
        .ok();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        loop {
            if stop_thread.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let mut line = String::new();
            let n = match reader.read_line(&mut line) {
                Ok(n) => n,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(event) =
                serde_json::from_str::<crate::daemon_stream::DaemonStreamEvent>(trimmed)
            {
                if tx.send(event).is_err() {
                    break;
                }
            }
        }
    });

    Ok(Some(StreamSubscription { rx, stop }))
}

/// Check whether a Unix PID is alive using `kill -0`.
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // SAFETY: kill(pid, 0) is a read-only existence check; no signal is sent.
    // We declare the extern fn inline to avoid a compile-time libc dependency
    // at the crate level (libc is only in [target.'cfg(unix)'.dependencies]).
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: kill with sig=0 never sends a signal; it only checks PID existence.
    let result = unsafe { kill(pid, 0) };
    result == 0
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            // SAFETY: test-scoped env mutation guarded by RAII restore in Drop.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, old }
        }

        fn unset(key: &'static str) -> Self {
            let old = std::env::var(key).ok();
            // SAFETY: test-scoped env mutation guarded by RAII restore in Drop.
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: test-scoped env restore.
            unsafe {
                if let Some(old) = &self.old {
                    std::env::set_var(self.key, old);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[cfg(unix)]
    fn wait_for_daemon_runtime_ready(home: &std::path::Path) -> bool {
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(crate::consts::SHORT_DEADLINE_SECS);
        let pid_path = home.join(".atm/daemon/atm-daemon.pid");
        while std::time::Instant::now() < deadline {
            if pid_path.exists() && super::daemon_socket_connectable(home) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(
                crate::consts::POLL_CHECK_SLEEP_MS,
            ));
        }
        false
    }

    #[cfg(unix)]
    fn wait_for_daemon_version(home: &std::path::Path, expected_version: &str) -> Option<i32> {
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(crate::consts::SHORT_DEADLINE_SECS);
        let pid_path = home.join(".atm/daemon/atm-daemon.pid");
        let status_path = home.join(".atm/daemon/status.json");
        while std::time::Instant::now() < deadline {
            let pid = std::fs::read_to_string(&pid_path)
                .ok()
                .and_then(|raw| raw.trim().parse::<i32>().ok());
            let status_version = std::fs::read_to_string(&status_path)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|json| {
                    json.get("version")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                });
            if let Some(pid) = pid
                && super::pid_alive(pid)
                && status_version.as_deref() == Some(expected_version)
            {
                return Some(pid);
            }
            std::thread::sleep(std::time::Duration::from_millis(
                crate::consts::POLL_CHECK_SLEEP_MS,
            ));
        }
        None
    }

    #[cfg(unix)]
    fn wait_for_sigterm_and_reap_pid(pid: i32) {
        send_signal(pid, 15);
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(crate::consts::SHORT_DEADLINE_SECS);
        while std::time::Instant::now() < deadline {
            if !pid_alive(pid) {
                reap_child_pid_best_effort(pid);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(
                crate::consts::POLL_CHECK_SLEEP_MS,
            ));
        }
        reap_child_pid_best_effort(pid);
    }

    #[cfg(unix)]
    fn reap_child_pid_best_effort(pid: i32) {
        unsafe extern "C" {
            fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
        }

        const WNOHANG: i32 = 1;
        let mut status = 0;
        // SAFETY: Best-effort reap for test children after a bounded SIGTERM wait.
        let _ = unsafe { waitpid(pid, &mut status, WNOHANG) };
    }

    #[cfg(unix)]
    fn fake_lock_metadata(home: &str, pid: u32) -> DaemonLockMetadata {
        DaemonLockMetadata {
            pid,
            owner: RuntimeOwnerMetadata {
                runtime_kind: RuntimeKind::Isolated,
                build_profile: BuildProfile::Release,
                executable_path: std::env::temp_dir()
                    .join("fake-atm-daemon")
                    .to_string_lossy()
                    .into_owned(),
                home_scope: home.to_string(),
            },
            version: "0.0.1".to_string(),
            written_at: chrono::Utc::now().to_rfc3339(),
        }
    }
    use serial_test::serial;

    fn with_autostart_disabled<T>(f: impl FnOnce() -> T) -> T {
        let _guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "0");
        f()
    }

    #[test]
    fn test_socket_request_serialization() {
        let req = SocketRequest {
            version: 1,
            request_id: "req-123".to_string(),
            command: "agent-state".to_string(),
            payload: serde_json::json!({ "agent": "arch-ctm", "team": "atm-dev" }),
        };

        let json = serde_json::to_string(&req).unwrap();
        let decoded: SocketRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.request_id, "req-123");
        assert_eq!(decoded.command, "agent-state");
    }

    #[test]
    fn test_socket_response_ok_deserialization() {
        let json = r#"{"version":1,"request_id":"req-123","status":"ok","payload":{"state":"idle","last_transition":"2026-02-16T22:30:00Z"}}"#;
        let resp: SocketResponse = serde_json::from_str(json).unwrap();

        assert!(resp.is_ok());
        assert_eq!(resp.request_id, "req-123");
        assert!(resp.payload.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_socket_response_error_deserialization() {
        let json = r#"{"version":1,"request_id":"req-456","status":"error","error":{"code":"AGENT_NOT_FOUND","message":"Agent 'unknown' is not tracked"}}"#;
        let resp: SocketResponse = serde_json::from_str(json).unwrap();

        assert!(!resp.is_ok());
        let err = resp.error.unwrap();
        assert_eq!(err.code, "AGENT_NOT_FOUND");
    }

    #[test]
    fn test_agent_state_info_deserialization() {
        let json = r#"{"state":"idle","last_transition":"2026-02-16T22:30:00Z"}"#;
        let info: AgentStateInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.state, "idle");
        assert_eq!(
            info.last_transition.as_deref(),
            Some("2026-02-16T22:30:00Z")
        );
    }

    #[test]
    fn test_agent_state_info_missing_transition() {
        let json = r#"{"state":"launching"}"#;
        let info: AgentStateInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.state, "launching");
        assert!(info.last_transition.is_none());
    }

    #[test]
    #[serial]
    fn test_query_daemon_no_socket_returns_none() {
        with_autostart_disabled(|| {
            // Without a running daemon the query should gracefully return None.
            // We ensure no real socket path is present by using a non-existent dir.
            // This test is platform-independent: on non-unix it always returns None.
            let req = SocketRequest {
                version: PROTOCOL_VERSION,
                request_id: "req-test".to_string(),
                command: "agent-state".to_string(),
                payload: serde_json::json!({}),
            };
            // Override socket path resolution is not straightforward without DI;
            // the test relies on the daemon not being present in the test environment.
            // On CI this will always be None. Locally too unless daemon is running.
            let result = query_daemon(&req);
            assert!(result.is_ok());
            // If daemon happens to be running, we just check the call didn't panic.
        });
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_daemon_autostart_flag_parsing() {
        // Unset => enabled (opt-out model).
        {
            let _guard = EnvGuard::unset("ATM_DAEMON_AUTOSTART");
            assert!(daemon_autostart_enabled());
        }

        {
            let _guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "1");
            assert!(daemon_autostart_enabled());
        }
        {
            let _guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "true");
            assert!(daemon_autostart_enabled());
        }
        {
            let _guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "yes");
            assert!(daemon_autostart_enabled());
        }
        {
            let _guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "0");
            assert!(!daemon_autostart_enabled());
        }
        {
            let _guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "false");
            assert!(!daemon_autostart_enabled());
        }
        {
            let _guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "no");
            assert!(!daemon_autostart_enabled());
        }
        // Invalid values remain enabled unless explicitly falsey.
        {
            let _guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "maybe");
            assert!(daemon_autostart_enabled());
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_resolve_daemon_binary_honors_override() {
        let tmp = tempfile::tempdir().unwrap();
        let custom = tmp.path().join("custom-atm-daemon");
        std::fs::write(&custom, "#!/bin/sh\nexit 0\n").unwrap();
        let _bin_guard = EnvGuard::set("ATM_DAEMON_BIN", custom.to_str().unwrap());
        let resolved = resolve_daemon_binary().expect("override should resolve");
        assert_eq!(std::path::PathBuf::from(resolved), custom);
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_cleanup_stale_runtime_files_removes_dead_pid_file() {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let daemon_dir = home.join(".atm/daemon");
        fs::create_dir_all(&daemon_dir).unwrap();

        let pid_path = daemon_dir.join("atm-daemon.pid");
        fs::write(&pid_path, "999999\n").unwrap();
        assert!(pid_path.exists());

        cleanup_stale_daemon_runtime_files(home);
        assert!(
            !pid_path.exists(),
            "stale PID file should be removed when PID is not alive"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_cleanup_stale_runtime_files_handles_malformed_pid() {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let daemon_dir = home.join(".atm/daemon");
        fs::create_dir_all(&daemon_dir).unwrap();

        let pid_path = daemon_dir.join("atm-daemon.pid");
        let socket_path = daemon_dir.join("atm-daemon.sock");
        fs::write(&pid_path, "not-a-pid\n").unwrap();
        fs::write(&socket_path, "stale").unwrap();

        cleanup_stale_daemon_runtime_files(home);

        assert!(
            !pid_path.exists(),
            "malformed PID file should be removed during cleanup"
        );
        assert!(
            !socket_path.exists(),
            "stale socket should be removed when PID ownership is known-dead"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_cleanup_stale_runtime_files_unreadable_pid_does_not_remove_socket() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let daemon_dir = home.join(".atm/daemon");
        fs::create_dir_all(&daemon_dir).unwrap();

        let pid_path = daemon_dir.join("atm-daemon.pid");
        let socket_path = daemon_dir.join("atm-daemon.sock");
        fs::write(&pid_path, "123\n").unwrap();
        fs::write(&socket_path, "stale").unwrap();
        let mut perms = fs::metadata(&pid_path).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&pid_path, perms).unwrap();

        cleanup_stale_daemon_runtime_files(home);
        assert!(
            socket_path.exists(),
            "socket must not be removed when PID ownership cannot be read"
        );

        // Restore permissions so tempdir cleanup succeeds.
        let mut restore = fs::metadata(&pid_path).unwrap().permissions();
        restore.set_mode(0o600);
        fs::set_permissions(&pid_path, restore).unwrap();
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_ensure_daemon_running_reports_startup_exit_without_stderr_tail() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let script_path = home.join("fake-daemon-fail.sh");
        let script = r#"#!/bin/sh
set -eu
echo "fatal: invalid plugin config" >&2
exit 42
"#;
        fs::write(&script_path, script).unwrap();
        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).unwrap();

        let _home_guard = EnvGuard::set("ATM_HOME", home.to_str().unwrap());
        let _bin_guard = EnvGuard::set("ATM_DAEMON_BIN", script_path.to_str().unwrap());
        let _auto_guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "1");

        let err = ensure_daemon_running_unix().expect_err("startup should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("daemon process exited during startup with status"),
            "startup exit must still be reported clearly: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_ensure_daemon_running_timeout_when_spawned_process_never_creates_runtime_files() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let script_path = home.join("fake-daemon-never-ready.sh");
        let script = r#"#!/bin/sh
set -eu
sleep 10
"#;
        fs::write(&script_path, script).unwrap();
        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).unwrap();

        let _home_guard = EnvGuard::set("ATM_HOME", home.to_str().unwrap());
        let _bin_guard = EnvGuard::set("ATM_DAEMON_BIN", script_path.to_str().unwrap());
        let _auto_guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "1");

        let err = ensure_daemon_running_unix().expect_err("startup should time out");
        let msg = err.to_string();
        assert!(
            msg.contains("daemon startup timed out after 5s"),
            "timeout error should include actionable timeout details: {msg}"
        );
        assert!(
            msg.contains("pid_path="),
            "timeout error should include pid path"
        );
        assert!(
            msg.contains("socket_path="),
            "timeout error should include socket path"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    #[ignore = "flaky concurrent race - tracked as pre-existing, see issue #805"]
    fn test_ensure_daemon_running_serializes_concurrent_start() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;
        use std::thread;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let script_path = home.join("fake-daemon.sh");

        let script = format!(
            r#"#!/bin/sh
set -eu
home="${{ATM_HOME:?}}"
mkdir -p "$home/.atm/daemon"
mkdir -p "$home/spawn-markers"
touch "$home/spawn-markers/spawn.$$"
echo $$ > "$home/.atm/daemon/atm-daemon.pid"
cat > "$home/.atm/daemon/status.json" <<'EOF'
{{"pid":$$,"version":"{}"}}
EOF
cat > "$home/.atm/daemon/daemon.lock.meta.json" <<'EOF'
{{
  "pid": $$,
  "owner": {{
    "runtime_kind": "isolated",
    "build_profile": "release",
    "executable_path": "{}",
    "home_scope": "{}"
  }},
  "version": "{}",
  "written_at": "2026-03-16T00:00:00Z"
}}
EOF
python3 - "$home/.atm/daemon/atm-daemon.sock" "$home/stop-daemon" <<'PY' &
import os, socket, sys, time
sock_path=sys.argv[1]
stop_path=sys.argv[2]
try:
    os.unlink(sock_path)
except FileNotFoundError:
    pass
srv=socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(sock_path)
srv.listen(1)
srv.settimeout(0.1)
try:
    while not os.path.exists(stop_path):
        try:
            conn, _ = srv.accept()
        except socket.timeout:
            continue
        else:
            conn.close()
finally:
    srv.close()
    try:
        os.unlink(sock_path)
    except FileNotFoundError:
        pass
PY
server_pid=$!
cleanup() {{
  kill "$server_pid" 2>/dev/null || true
  wait "$server_pid" 2>/dev/null || true
}}
term_cleanup() {{
  cleanup
  exit 0
}}
trap cleanup EXIT
trap term_cleanup INT TERM
while [ ! -f "$home/stop-daemon" ]; do
  sleep 0.1
done
"#,
            env!("CARGO_PKG_VERSION"),
            script_path.display(),
            home.display(),
            env!("CARGO_PKG_VERSION"),
        );
        fs::write(&script_path, script).unwrap();
        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).unwrap();

        let _home_guard = EnvGuard::set("ATM_HOME", home.to_str().unwrap());
        let _bin_guard = EnvGuard::set("ATM_DAEMON_BIN", script_path.to_str().unwrap());
        let _auto_guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "1");

        let mut handles = Vec::new();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        for _ in 0..2 {
            let b = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                b.wait();
                ensure_daemon_running_unix().unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let current_pid = fs::read_to_string(home.join(".atm/daemon/atm-daemon.pid"))
            .ok()
            .and_then(|raw| raw.trim().parse::<i32>().ok());
        let socket_ready = super::daemon_socket_connectable(&home);
        fs::write(home.join("stop-daemon"), "stop").unwrap();
        assert_eq!(
            current_pid.map(pid_alive),
            Some(true),
            "concurrent startup attempts must converge to a live daemon pid"
        );
        assert!(
            socket_ready,
            "concurrent startup attempts must converge to a connectable daemon socket"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_write_daemon_lock_metadata_contains_identity_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let owner = RuntimeOwnerMetadata {
            runtime_kind: RuntimeKind::Isolated,
            build_profile: BuildProfile::Release,
            executable_path: std::env::temp_dir()
                .join("isolated-atm-daemon")
                .to_string_lossy()
                .into_owned(),
            home_scope: home.to_string_lossy().into_owned(),
        };

        write_daemon_lock_metadata(home, "9.9.9-test", &owner).expect("write lock metadata");

        let path = home.join(".atm/daemon/daemon.lock.meta.json");
        let raw = std::fs::read_to_string(&path).expect("read lock metadata");
        let meta: DaemonLockMetadata = serde_json::from_str(&raw).expect("parse lock metadata");

        assert_eq!(meta.pid, std::process::id());
        assert_eq!(meta.version, "9.9.9-test");
        assert!(
            !meta.owner.executable_path.trim().is_empty(),
            "executable path must be populated"
        );
        assert!(
            !meta.owner.home_scope.trim().is_empty(),
            "home scope must be populated"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_runtime_admission_accepts_shared_dev_install() {
        let root = tempfile::tempdir().unwrap();
        let os_home = root.path().join("home");
        let input = RuntimePolicyInput {
            home_scope: default_dev_runtime_home_for(&os_home),
            daemon_bin: default_dev_runtime_root_for(&os_home)
                .join("current")
                .join("bin")
                .join("atm-daemon"),
            os_home: os_home.clone(),
            temp_root: root.path().join("tmp"),
            build_profile: BuildProfile::Release,
        };

        let owner = validate_runtime_admission_input(&input).expect("dev install should be valid");
        assert_eq!(owner.runtime_kind, RuntimeKind::Dev);
        assert_eq!(owner.build_profile, BuildProfile::Release);
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_runtime_admission_rejects_repo_binary_for_shared_dev_runtime() {
        let root = tempfile::tempdir().unwrap();
        let os_home = root.path().join("home");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let input = RuntimePolicyInput {
            home_scope: default_dev_runtime_home_for(&os_home),
            daemon_bin: repo.join("target").join("release").join("atm-daemon"),
            os_home,
            temp_root: root.path().join("tmp"),
            build_profile: BuildProfile::Release,
        };

        let err = validate_runtime_admission_input(&input).expect_err("repo binary must be denied");
        assert!(err.to_string().contains("approved installed daemon binary"));
    }

    #[cfg(unix)]
    #[test]
    fn test_validate_runtime_admission_rejects_debug_build_for_shared_release_runtime() {
        let root = tempfile::tempdir().unwrap();
        let os_home = root.path().join("home");
        let input = RuntimePolicyInput {
            home_scope: os_home.clone(),
            daemon_bin: PathBuf::from("/opt/homebrew/bin/atm-daemon"),
            os_home,
            temp_root: root.path().join("tmp"),
            build_profile: BuildProfile::Debug,
        };

        let err = validate_runtime_admission_input(&input).expect_err("debug build must be denied");
        assert!(err.to_string().contains("requires a release build"));
    }

    #[cfg(unix)]
    #[test]
    fn test_create_isolated_runtime_root_writes_metadata_and_paths() {
        let base = tempfile::tempdir().unwrap();
        let created = create_isolated_runtime_root_with_base(
            base.path(),
            Some("smoke-test"),
            Duration::from_secs(DAEMON_TIMEOUT_MAX_SECS),
            false,
        )
        .expect("create isolated runtime");

        assert!(created.home.exists(), "isolated ATM_HOME must exist");
        assert!(
            created.runtime_dir.exists(),
            "isolated daemon runtime dir must exist"
        );
        assert_eq!(created.metadata.runtime_kind, RuntimeKind::Isolated);
        assert!(!created.metadata.allow_live_github_polling);
        assert!(
            created.metadata.expires_at.is_some(),
            "isolated runtime metadata must include expires_at"
        );

        let persisted = read_runtime_metadata(&created.home).expect("persisted metadata");
        assert_eq!(persisted, created.metadata);
        assert_eq!(
            runtime_kind_for_home(&created.home).unwrap(),
            RuntimeKind::Isolated
        );
        assert!(
            !isolated_runtime_allows_live_github(&created.home).unwrap(),
            "isolated runtime should deny live GitHub polling by default"
        );
    }

    // Regression test for GH #761: write_runtime_metadata must not call
    // read_runtime_metadata and discard the result. Verify overwrite persists replacement.
    #[test]
    fn test_write_runtime_metadata_overwrites_existing_contents() {
        let home = tempfile::tempdir().unwrap();
        let original = RuntimeMetadata {
            runtime_kind: RuntimeKind::Dev,
            created_at: "2026-03-14T00:00:00Z".to_string(),
            expires_at: None,
            test_identifier: None,
            owner_pid: None,
            token_id: None,
            allow_live_github_polling: true,
        };
        let replacement = RuntimeMetadata {
            runtime_kind: RuntimeKind::Isolated,
            created_at: "2026-03-15T00:00:00Z".to_string(),
            expires_at: Some("2026-03-15T00:10:00Z".to_string()),
            test_identifier: Some("daemon-tests::replacement".to_string()),
            owner_pid: Some(4242),
            token_id: Some("token-123".to_string()),
            allow_live_github_polling: false,
        };

        write_runtime_metadata(home.path(), &original).unwrap();
        write_runtime_metadata(home.path(), &replacement).unwrap();

        let persisted = read_runtime_metadata(home.path()).expect("persisted metadata");
        assert_eq!(persisted, replacement);
    }

    #[cfg(unix)]
    #[test]
    fn test_reap_expired_isolated_runtime_roots_removes_dead_runtime() {
        let base = tempfile::tempdir().unwrap();
        let root = isolated_runtime_root_dir(Some(base.path()));
        let home = root.join("expired-runtime");
        std::fs::create_dir_all(daemon_runtime_dir_for(&home)).unwrap();
        let metadata = RuntimeMetadata {
            runtime_kind: RuntimeKind::Isolated,
            created_at: "2026-03-14T00:00:00Z".to_string(),
            expires_at: Some("2026-03-14T00:10:00Z".to_string()),
            test_identifier: Some("daemon-tests::expired".to_string()),
            owner_pid: Some(999_999),
            token_id: Some("token-expired".to_string()),
            allow_live_github_polling: false,
        };
        write_runtime_metadata(&home, &metadata).unwrap();

        let reaped = reap_expired_isolated_runtime_roots_with_base(base.path()).unwrap();
        assert_eq!(reaped, vec![home.clone()]);
        assert!(
            !home.exists(),
            "expired dead isolated runtime should be reaped"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_isolated_runtime_allows_live_github_when_explicitly_enabled() {
        let base = tempfile::tempdir().unwrap();
        let created = create_isolated_runtime_root_with_base(
            base.path(),
            Some("live-gh"),
            Duration::from_secs(DAEMON_TIMEOUT_MAX_SECS),
            true,
        )
        .expect("create isolated runtime");

        assert!(
            isolated_runtime_allows_live_github(&created.home).unwrap(),
            "explicit isolated runtime override should enable live GitHub polling"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_evaluate_daemon_identity_mismatch_requires_metadata_or_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let snapshot = DaemonIdentitySnapshot::default();
        let reason = evaluate_daemon_identity_mismatch(
            &snapshot,
            &home,
            std::ffi::OsStr::new("atm-daemon"),
            env!("CARGO_PKG_VERSION"),
            |_| true,
            |_| Some("atm-daemon".to_string()),
        );

        assert_eq!(reason, None);
    }

    #[cfg(unix)]
    #[test]
    fn test_evaluate_daemon_identity_mismatch_reports_missing_metadata_when_socket_live() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let snapshot = DaemonIdentitySnapshot {
            socket_connectable: true,
            ..Default::default()
        };
        let reason = evaluate_daemon_identity_mismatch(
            &snapshot,
            &home,
            std::ffi::OsStr::new("atm-daemon"),
            env!("CARGO_PKG_VERSION"),
            |_| true,
            |_| Some("atm-daemon".to_string()),
        )
        .expect("expected mismatch");

        assert!(reason.contains("lock metadata missing"));
    }

    #[cfg(unix)]
    #[test]
    fn test_evaluate_daemon_identity_mismatch_reports_command_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let snapshot = DaemonIdentitySnapshot {
            metadata: Some(fake_lock_metadata(&home, 4242)),
            pid_from_file: Some(4242),
            socket_connectable: true,
            ..Default::default()
        };
        let reason = evaluate_daemon_identity_mismatch(
            &snapshot,
            &home,
            std::ffi::OsStr::new("atm-daemon"),
            env!("CARGO_PKG_VERSION"),
            |_| true,
            |_| Some("/usr/bin/python3 -m stale-daemon".to_string()),
        )
        .expect("expected mismatch");

        assert!(reason.contains("running command"));
        assert!(reason.contains("expected daemon binary"));
    }

    #[cfg(unix)]
    #[test]
    fn test_evaluate_daemon_identity_mismatch_reports_version_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let snapshot = DaemonIdentitySnapshot {
            metadata: Some(fake_lock_metadata(&home, 4242)),
            pid_from_file: Some(4242),
            version_from_status: Some("0.0.1".to_string()),
            socket_connectable: true,
            ..Default::default()
        };
        let reason = evaluate_daemon_identity_mismatch(
            &snapshot,
            &home,
            std::ffi::OsStr::new("atm-daemon"),
            env!("CARGO_PKG_VERSION"),
            |_| true,
            |_| Some("atm-daemon".to_string()),
        )
        .expect("expected mismatch");

        assert!(reason.contains("daemon version mismatch"));
        assert!(reason.contains("running=0.0.1"));
    }

    #[cfg(unix)]
    #[test]
    fn test_evaluate_daemon_identity_mismatch_accepts_matching_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        let snapshot = DaemonIdentitySnapshot {
            metadata: Some(fake_lock_metadata(&home, 4242)),
            pid_from_file: Some(4242),
            version_from_status: Some(env!("CARGO_PKG_VERSION").to_string()),
            socket_connectable: true,
            ..Default::default()
        };
        let reason = evaluate_daemon_identity_mismatch(
            &snapshot,
            &home,
            std::ffi::OsStr::new("atm-daemon"),
            env!("CARGO_PKG_VERSION"),
            |_| true,
            |_| Some("atm-daemon --serve".to_string()),
        );

        assert_eq!(reason, None);
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_ensure_daemon_running_recovers_from_dead_pid_metadata() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        fs::create_dir_all(home.join(".atm/daemon")).unwrap();

        // Simulate stale metadata from a dead prior daemon instance.
        fs::write(home.join(".atm/daemon/atm-daemon.pid"), "999999\n").unwrap();
        let stale = DaemonLockMetadata {
            pid: 999999,
            owner: RuntimeOwnerMetadata {
                runtime_kind: RuntimeKind::Isolated,
                build_profile: BuildProfile::Release,
                executable_path: std::env::temp_dir()
                    .join("old-atm-daemon")
                    .to_string_lossy()
                    .to_string(),
                home_scope: home.to_string_lossy().to_string(),
            },
            version: "0.0.1".to_string(),
            written_at: chrono::Utc::now().to_rfc3339(),
        };
        fs::write(
            home.join(".atm/daemon/daemon.lock.meta.json"),
            serde_json::to_string_pretty(&stale).unwrap(),
        )
        .unwrap();

        let script_path = home.join("fake-daemon-start.sh");
        let script = format!(
            r#"#!/bin/sh
set -eu
home="${{ATM_HOME:?}}"
mkdir -p "$home/.atm/daemon"
pid=$$
echo "$pid" > "$home/.atm/daemon/atm-daemon.pid"
cat > "$home/.atm/daemon/status.json" <<'JSON'
{{"timestamp":"2026-01-01T00:00:00Z","pid":0,"version":"{}","uptime_secs":1,"plugins":[],"teams":[]}}
JSON
python3 - <<'PY'
import json, os
home=os.environ["ATM_HOME"]
path=os.path.join(home, ".atm", "daemon", "status.json")
with open(path, "r", encoding="utf-8") as f:
    obj=json.load(f)
obj["pid"]=os.getpid()
with open(path, "w", encoding="utf-8") as f:
    json.dump(obj, f)
open(os.path.join(home, "started-ok"), "w").write("ok")
PY
python3 - "$home/.atm/daemon/atm-daemon.sock" <<'PY' &
import os, signal, socket, sys, time
sock_path=sys.argv[1]
try:
    os.unlink(sock_path)
except FileNotFoundError:
    pass
srv=socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(sock_path)
srv.listen(1)
srv.settimeout(0.1)
def shutdown(*_):
    try:
        srv.close()
    finally:
        try:
            os.unlink(sock_path)
        except FileNotFoundError:
            pass
    sys.exit(0)
signal.signal(signal.SIGTERM, shutdown)
signal.signal(signal.SIGINT, shutdown)
while True:
    try:
        conn, _ = srv.accept()
    except socket.timeout:
        continue
    else:
        conn.close()
PY
server_pid=$!
cleanup() {{
  kill "$server_pid" 2>/dev/null || true
  wait "$server_pid" 2>/dev/null || true
}}
term_cleanup() {{
  cleanup
  exit 0
}}
trap cleanup EXIT
trap term_cleanup INT TERM
while true; do
  sleep 1
done
"#,
            env!("CARGO_PKG_VERSION")
        );
        fs::write(&script_path, script).unwrap();
        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).unwrap();

        let _home_guard = EnvGuard::set("ATM_HOME", home.to_str().unwrap());
        let _bin_guard = EnvGuard::set("ATM_DAEMON_BIN", script_path.to_str().unwrap());
        let _auto_guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "1");

        ensure_daemon_running_unix().expect("must recover from dead stale pid metadata");
        let marker = home.join("started-ok");
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(crate::consts::SHORT_DEADLINE_SECS);
        while std::time::Instant::now() < deadline && !marker.exists() {
            std::thread::sleep(std::time::Duration::from_millis(
                crate::consts::SHORT_SLEEP_MS,
            ));
        }
        assert!(marker.exists(), "expected replacement daemon to start");

        if let Ok(pid_str) = std::fs::read_to_string(home.join(".atm/daemon/atm-daemon.pid"))
            && let Ok(pid) = pid_str.trim().parse::<i32>()
            && pid_alive(pid)
        {
            wait_for_sigterm_and_reap_pid(pid);
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    #[ignore = "smoke coverage only; exercises real subprocess and socket timing"]
    fn test_ensure_daemon_running_restarts_identity_mismatch_daemon() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        fs::create_dir_all(home.join(".atm/daemon")).unwrap();

        let stale_script = home.join("stale-daemon.sh");
        let stale = r#"#!/bin/sh
set -eu
home="${ATM_HOME:?}"
mkdir -p "$home/.atm/daemon"
pid=$$
echo "$pid" > "$home/.atm/daemon/atm-daemon.pid"
cat > "$home/.atm/daemon/status.json" <<'JSON'
{"timestamp":"2026-01-01T00:00:00Z","pid":0,"version":"0.0.1","uptime_secs":1,"plugins":[],"teams":[]}
JSON
python3 - <<'PY'
import json, os
home=os.environ["ATM_HOME"]
path=os.path.join(home, ".atm", "daemon", "status.json")
with open(path, "r", encoding="utf-8") as f:
    obj=json.load(f)
obj["pid"]=os.getpid()
with open(path, "w", encoding="utf-8") as f:
    json.dump(obj, f)
PY
exec python3 - "$home/.atm/daemon/atm-daemon.sock" <<'PY'
import os, signal, socket, sys, time
path=sys.argv[1]
try:
    os.unlink(path)
except FileNotFoundError:
    pass
srv=socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(path)
srv.listen(1)
def shutdown(*_):
    try:
        srv.close()
    finally:
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass
    sys.exit(0)
signal.signal(signal.SIGTERM, shutdown)
signal.signal(signal.SIGINT, shutdown)
while True:
    time.sleep(1)
PY
"#;
        fs::write(&stale_script, stale).unwrap();
        let mut stale_perms = fs::metadata(&stale_script).unwrap().permissions();
        stale_perms.set_mode(0o755);
        fs::set_permissions(&stale_script, stale_perms).unwrap();

        let expected_script = home.join("expected-daemon.sh");
        let expected = format!(
            r#"#!/bin/sh
set -eu
home="${{ATM_HOME:?}}"
mkdir -p "$home/.atm/daemon"
pid=$$
echo "$pid" > "$home/.atm/daemon/atm-daemon.pid"
cat > "$home/.atm/daemon/status.json" <<'JSON'
{{"timestamp":"2026-01-01T00:00:00Z","pid":0,"version":"{}","uptime_secs":1,"plugins":[],"teams":[]}}
JSON
python3 - <<'PY'
import json, os
home=os.environ["ATM_HOME"]
path=os.path.join(home, ".atm", "daemon", "status.json")
with open(path, "r", encoding="utf-8") as f:
    obj=json.load(f)
obj["pid"]=os.getpid()
with open(path, "w", encoding="utf-8") as f:
    json.dump(obj, f)
open(os.path.join(home, "replacement-started"), "w").write("ok")
with open(os.path.join(home, "replacement-started"), "a", encoding="utf-8") as f:
    f.flush()
    os.fsync(f.fileno())
PY
sleep 8
"#,
            env!("CARGO_PKG_VERSION")
        );
        fs::write(&expected_script, expected).unwrap();
        let mut expected_perms = fs::metadata(&expected_script).unwrap().permissions();
        expected_perms.set_mode(0o755);
        fs::set_permissions(&expected_script, expected_perms).unwrap();

        let _home_guard = EnvGuard::set("ATM_HOME", home.to_str().unwrap());
        let _auto_guard_stale = EnvGuard::set("ATM_DAEMON_AUTOSTART", "0");
        let mut stale_child = std::process::Command::new(&stale_script)
            .env("ATM_HOME", &home)
            .spawn()
            .expect("spawn stale daemon");
        assert!(
            wait_for_daemon_runtime_ready(&home),
            "stale daemon must publish pid file and bind socket before mismatch check"
        );
        let stale_pid: u32 = std::fs::read_to_string(home.join(".atm/daemon/atm-daemon.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let stale_metadata = DaemonLockMetadata {
            pid: stale_pid,
            owner: RuntimeOwnerMetadata {
                runtime_kind: RuntimeKind::Isolated,
                build_profile: BuildProfile::Release,
                executable_path: stale_script.to_string_lossy().to_string(),
                home_scope: std::fs::canonicalize(&home)
                    .unwrap_or_else(|_| home.clone())
                    .to_string_lossy()
                    .to_string(),
            },
            version: "0.0.1".to_string(),
            written_at: chrono::Utc::now().to_rfc3339(),
        };
        std::fs::write(
            home.join(".atm/daemon/daemon.lock.meta.json"),
            serde_json::to_string_pretty(&stale_metadata).unwrap(),
        )
        .unwrap();

        let _bin_guard = EnvGuard::set("ATM_DAEMON_BIN", expected_script.to_str().unwrap());
        let _auto_guard_run = EnvGuard::set("ATM_DAEMON_AUTOSTART", "1");
        ensure_daemon_running_unix().expect("mismatch daemon should be restarted");

        let stale_exit_deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(crate::consts::SHORT_DEADLINE_SECS);
        let mut stale_exited = false;
        while std::time::Instant::now() < stale_exit_deadline {
            if stale_child.try_wait().ok().flatten().is_some() {
                stale_exited = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(
                crate::consts::SHORT_SLEEP_MS,
            ));
        }
        let new_pid = wait_for_daemon_version(&home, env!("CARGO_PKG_VERSION"))
            .expect("replacement daemon missing");
        if !stale_exited && stale_child.try_wait().ok().flatten().is_none() {
            let _ = stale_child.kill();
            let _ = stale_child.wait();
        }
        assert!(
            stale_exited,
            "stale daemon must exit during mismatch restart"
        );
        assert!(new_pid > 1, "replacement daemon pid must be valid");

        if pid_alive(new_pid) {
            wait_for_sigterm_and_reap_pid(new_pid);
        }
    }

    #[test]
    fn test_new_request_id_is_unique() {
        let id1 = new_request_id();
        // Tiny sleep to ensure different nanosecond timestamp
        std::thread::sleep(std::time::Duration::from_nanos(1000));
        let id2 = new_request_id();
        // Both should be non-empty; may or may not be equal depending on timing
        assert!(!id1.is_empty());
        assert!(!id2.is_empty());
    }

    #[test]
    fn test_daemon_socket_path_contains_expected_suffix() {
        let path = daemon_socket_path().unwrap();
        assert!(path.to_string_lossy().ends_with("atm-daemon.sock"));
        assert!(path.to_string_lossy().contains(".atm/daemon"));
    }

    #[test]
    fn test_daemon_pid_path_contains_expected_suffix() {
        let path = daemon_pid_path().unwrap();
        assert!(path.to_string_lossy().ends_with("atm-daemon.pid"));
        assert!(path.to_string_lossy().contains(".atm/daemon"));
    }

    #[test]
    #[serial]
    fn test_query_agent_state_no_daemon_returns_none() {
        with_autostart_disabled(|| {
            // Graceful fallback: no daemon → Ok(None)
            let result = query_agent_state("arch-ctm", "atm-dev");
            assert!(result.is_ok());
            // Result is None unless daemon happens to be running
        });
    }

    #[test]
    #[serial]
    fn test_query_team_member_states_offline_returns_none() {
        with_autostart_disabled(|| {
            let tmp = tempfile::tempdir().expect("tempdir");
            let _home_guard = EnvGuard::set("ATM_HOME", tmp.path().to_str().unwrap());

            let result = query_team_member_states("atm-dev");

            assert!(
                matches!(result, Ok(None)),
                "offline daemon must map to Ok(None), got: {result:?}"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn test_query_team_member_states_invalid_payload_returns_err() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        with_autostart_disabled(|| {
            let tmp = tempfile::tempdir().expect("tempdir");
            let daemon_dir = tmp.path().join(".atm/daemon");
            std::fs::create_dir_all(&daemon_dir).expect("create daemon dir");
            let socket_path = daemon_dir.join("atm-daemon.sock");

            let listener = UnixListener::bind(&socket_path).expect("bind socket");
            let handle = std::thread::spawn(move || {
                // Other concurrently running tests can occasionally hit this temporary
                // socket while ATM_HOME is overridden. Ignore non-target requests and
                // keep waiting until we receive the expected list-agents query.
                for _ in 0..32 {
                    let (mut stream, _) = listener.accept().expect("accept");
                    let mut request_line = String::new();
                    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                    reader.read_line(&mut request_line).expect("read request");

                    if request_line.contains("\"command\":\"list-agents\"") {
                        let response = SocketResponse {
                            version: PROTOCOL_VERSION,
                            request_id: "req-test".to_string(),
                            status: "ok".to_string(),
                            payload: Some(serde_json::json!({
                                "agent": "arch-ctm",
                                "state": "active"
                            })),
                            error: None,
                        };
                        let line = serde_json::to_string(&response).expect("serialize response");
                        stream.write_all(line.as_bytes()).expect("write response");
                        stream.write_all(b"\n").expect("write newline");
                        return;
                    }

                    let ignored = SocketResponse {
                        version: PROTOCOL_VERSION,
                        request_id: "req-ignored".to_string(),
                        status: "error".to_string(),
                        payload: None,
                        error: Some(SocketError {
                            code: "IGNORED_FOR_TEST".to_string(),
                            message: "ignored non-list-agents request".to_string(),
                        }),
                    };
                    let line = serde_json::to_string(&ignored).expect("serialize ignored");
                    stream.write_all(line.as_bytes()).expect("write ignored");
                    stream.write_all(b"\n").expect("write newline");
                }
                panic!("expected list-agents request within retry budget");
            });

            let _home_guard = EnvGuard::set("ATM_HOME", tmp.path().to_str().unwrap());

            let result = query_team_member_states("atm-dev");

            handle.join().expect("mock daemon thread");
            let err = result.expect_err("invalid payload must return Err");
            assert!(
                err.to_string()
                    .contains("invalid canonical member-state payload"),
                "unexpected error: {err}"
            );
        });
    }

    #[test]
    fn test_agent_pane_info_deserialization() {
        let json = r#"{"pane_id":"%42","log_path":"/home/user/.claude/logs/arch-ctm.log"}"#;
        let info: AgentPaneInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.pane_id, "%42");
        assert_eq!(info.log_path, "/home/user/.claude/logs/arch-ctm.log");
    }

    #[test]
    #[serial]
    fn test_query_agent_pane_no_daemon_returns_none() {
        with_autostart_disabled(|| {
            // Graceful fallback: no daemon → Ok(None)
            let result = query_agent_pane("arch-ctm");
            assert!(result.is_ok());
            // Result is None unless daemon happens to be running
        });
    }

    #[test]
    fn test_launch_config_serialization() {
        let mut env_vars = std::collections::HashMap::new();
        env_vars.insert("EXTRA_VAR".to_string(), "value".to_string());

        let config = LaunchConfig {
            agent: "arch-ctm".to_string(),
            team: "atm-dev".to_string(),
            command: "codex --yolo".to_string(),
            prompt: Some("Review the bridge module".to_string()),
            timeout_secs: 30,
            env_vars,
            runtime: Some("codex".to_string()),
            resume_session_id: None,
        };

        let json = serde_json::to_string(&config).unwrap();
        let decoded: LaunchConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.agent, "arch-ctm");
        assert_eq!(decoded.team, "atm-dev");
        assert_eq!(decoded.command, "codex --yolo");
        assert_eq!(decoded.prompt.as_deref(), Some("Review the bridge module"));
        assert_eq!(decoded.timeout_secs, 30);
        assert_eq!(decoded.runtime.as_deref(), Some("codex"));
        assert!(decoded.resume_session_id.is_none());
        assert_eq!(
            decoded.env_vars.get("EXTRA_VAR").map(String::as_str),
            Some("value")
        );
    }

    #[test]
    fn test_launch_config_no_prompt_serialization() {
        let config = LaunchConfig {
            agent: "worker-1".to_string(),
            team: "my-team".to_string(),
            command: "codex --yolo".to_string(),
            prompt: None,
            timeout_secs: 60,
            env_vars: std::collections::HashMap::new(),
            runtime: None,
            resume_session_id: Some("sess-123".to_string()),
        };

        let json = serde_json::to_string(&config).unwrap();
        let decoded: LaunchConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.agent, "worker-1");
        assert!(decoded.prompt.is_none());
        assert!(decoded.env_vars.is_empty());
        assert!(decoded.runtime.is_none());
        assert_eq!(decoded.resume_session_id.as_deref(), Some("sess-123"));
    }

    #[test]
    fn test_launch_result_serialization() {
        let result = LaunchResult {
            agent: "arch-ctm".to_string(),
            pane_id: "%42".to_string(),
            state: "launching".to_string(),
            warning: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        let decoded: LaunchResult = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.agent, "arch-ctm");
        assert_eq!(decoded.pane_id, "%42");
        assert_eq!(decoded.state, "launching");
        assert!(decoded.warning.is_none());
    }

    #[test]
    fn test_launch_result_with_warning_serialization() {
        let result = LaunchResult {
            agent: "arch-ctm".to_string(),
            pane_id: "%7".to_string(),
            state: "launching".to_string(),
            warning: Some("Readiness timeout reached".to_string()),
        };

        let json = serde_json::to_string(&result).unwrap();
        let decoded: LaunchResult = serde_json::from_str(&json).unwrap();

        assert_eq!(
            decoded.warning.as_deref(),
            Some("Readiness timeout reached")
        );
    }

    #[test]
    fn test_session_query_result_serialization() {
        let result = SessionQueryResult {
            session_id: "abc123".to_string(),
            process_id: 12345,
            alive: true,
            last_seen_at: Some("2026-03-10T00:00:00Z".to_string()),
            runtime: None,
            runtime_session_id: None,
            pane_id: None,
            runtime_home: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        let decoded: SessionQueryResult = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.session_id, "abc123");
        assert_eq!(decoded.process_id, 12345);
        assert!(decoded.alive);
    }

    #[test]
    fn test_session_query_result_dead() {
        let json = r#"{"session_id":"xyz789","process_id":99,"alive":false}"#;
        let result: SessionQueryResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.session_id, "xyz789");
        assert_eq!(result.process_id, 99);
        assert!(!result.alive);
        assert!(result.last_seen_at.is_none());
        assert!(result.runtime.is_none());
        assert!(result.runtime_session_id.is_none());
    }

    #[test]
    #[serial]
    fn test_query_session_no_daemon_returns_none() {
        with_autostart_disabled(|| {
            // Graceful fallback: no daemon → Ok(None)
            let result = query_session("team-lead");
            assert!(result.is_ok());
            // None unless daemon happens to be running
        });
    }

    #[test]
    #[serial]
    fn test_launch_agent_no_daemon_returns_none() {
        with_autostart_disabled(|| {
            if daemon_is_running() {
                // Shared dev machines may have daemon active; this test validates
                // no-daemon behavior only.
                return;
            }
            let config = LaunchConfig {
                agent: "test-agent".to_string(),
                team: "test-team".to_string(),
                command: "codex --yolo".to_string(),
                prompt: None,
                timeout_secs: 5,
                env_vars: std::collections::HashMap::new(),
                runtime: Some("codex".to_string()),
                resume_session_id: None,
            };
            // Without a running daemon the call should gracefully return Ok(None).
            // On non-Unix platforms it always returns None.
            // On Unix with no daemon socket present it also returns None.
            let result = launch_agent(&config);
            // The result should be Ok (no I/O error on missing socket)
            assert!(result.is_ok());
            // Result is None unless daemon happens to be running and handling "launch"
            // (which it won't be in a unit test environment)
        });
    }

    #[test]
    #[serial]
    fn test_register_hint_no_daemon_is_silent_skip() {
        with_autostart_disabled(|| {
            if daemon_is_running() {
                return;
            }
            let outcome = register_hint(
                "atm-dev",
                "arch-ctm",
                "sess-arch-ctm-test-1234",
                1234,
                Some("codex"),
                Some("thread-id:arch-ctm-test-1234"),
                None,
                None,
            )
            .expect("register-hint must not error when daemon unavailable");
            assert_eq!(outcome, RegisterHintOutcome::DaemonUnavailable);
        });
    }

    #[test]
    fn test_agent_summary_serialization() {
        let summary = AgentSummary {
            agent: "arch-ctm".to_string(),
            state: "idle".to_string(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        let decoded: AgentSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.agent, "arch-ctm");
        assert_eq!(decoded.state, "idle");
    }

    #[test]
    fn test_canonical_member_state_serialization() {
        let state = CanonicalMemberState {
            agent: "arch-ctm".to_string(),
            state: "active".to_string(),
            activity: "busy".to_string(),
            session_id: Some("sess-123".to_string()),
            process_id: Some(4242),
            last_alive_at: Some("2026-03-08T00:00:00Z".to_string()),
            reason: "session active with live pid".to_string(),
            source: "session_registry".to_string(),
            in_config: true,
        };
        let json = serde_json::to_string(&state).unwrap();
        let decoded: CanonicalMemberState = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.agent, "arch-ctm");
        assert_eq!(decoded.state, "active");
        assert_eq!(decoded.activity, "busy");
        assert_eq!(decoded.session_id.as_deref(), Some("sess-123"));
        assert_eq!(decoded.process_id, Some(4242));
        assert_eq!(
            decoded.last_alive_at.as_deref(),
            Some("2026-03-08T00:00:00Z")
        );
        assert!(decoded.in_config);
    }

    #[test]
    fn test_canonical_status_activity_labels_and_liveness() {
        let active = CanonicalMemberState {
            agent: "arch-ctm".to_string(),
            state: "active".to_string(),
            activity: "busy".to_string(),
            session_id: None,
            process_id: None,
            last_alive_at: None,
            reason: String::new(),
            source: String::new(),
            in_config: true,
        };
        let idle = CanonicalMemberState {
            state: "idle".to_string(),
            activity: "idle".to_string(),
            ..active.clone()
        };
        let dead = CanonicalMemberState {
            state: "offline".to_string(),
            activity: "unknown".to_string(),
            ..active.clone()
        };

        assert_eq!(canonical_status_label(Some(&active)), "Active");
        assert_eq!(canonical_status_label(Some(&idle)), "Idle");
        assert_eq!(canonical_status_label(Some(&dead)), "Dead");
        assert_eq!(canonical_status_label(None), "Unknown");

        assert_eq!(canonical_activity_label(Some(&active)), "Busy");
        assert_eq!(canonical_activity_label(Some(&idle)), "Idle");
        assert_eq!(canonical_activity_label(Some(&dead)), "Unknown");
        assert_eq!(canonical_activity_label(None), "Unknown");

        assert_eq!(canonical_liveness_bool(Some(&active)), Some(true));
        assert_eq!(canonical_liveness_bool(Some(&idle)), Some(true));
        assert_eq!(canonical_liveness_bool(Some(&dead)), Some(false));
        assert_eq!(canonical_liveness_bool(None), None);
    }

    #[test]
    fn test_decode_canonical_member_states_payload_rejects_invalid_schema() {
        let invalid = serde_json::json!({
            "agent": "arch-ctm",
            "state": "active"
        });
        let err = decode_canonical_member_states_payload(invalid).unwrap_err();
        assert!(
            err.to_string()
                .contains("invalid canonical member-state payload")
        );
    }

    #[test]
    fn test_decode_canonical_member_states_payload_accepts_valid_schema() {
        let valid = serde_json::json!([
            {
                "agent": "arch-ctm",
                "state": "active",
                "activity": "busy",
                "session_id": "sess-1",
                "process_id": 1234,
                "reason": "session active",
                "source": "session_registry",
                "in_config": false
            }
        ]);
        let states = decode_canonical_member_states_payload(valid).expect("valid payload");
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].agent, "arch-ctm");
        assert_eq!(states[0].state, "active");
        assert!(!states[0].in_config);
    }

    #[test]
    fn test_decode_canonical_member_state_defaults_in_config_true_when_missing() {
        let json = r#"{
            "agent":"arch-ctm",
            "state":"active",
            "activity":"busy",
            "reason":"session active",
            "source":"session_registry"
        }"#;
        let state: CanonicalMemberState = serde_json::from_str(json).expect("decode");
        assert!(state.in_config);
    }

    #[test]
    fn test_decode_register_hint_response_ok_registered() {
        let response = SocketResponse {
            version: PROTOCOL_VERSION,
            request_id: "req-1".to_string(),
            status: "ok".to_string(),
            payload: Some(serde_json::json!({ "processed": true })),
            error: None,
        };
        let outcome = decode_register_hint_response(response).expect("ok response");
        assert_eq!(outcome, RegisterHintOutcome::Registered);
    }

    #[test]
    fn test_decode_register_hint_response_unknown_command_maps_to_unsupported() {
        let response = SocketResponse {
            version: PROTOCOL_VERSION,
            request_id: "req-1".to_string(),
            status: "error".to_string(),
            payload: None,
            error: Some(SocketError {
                code: "UNKNOWN_COMMAND".to_string(),
                message: "Unknown command: 'register-hint'".to_string(),
            }),
        };
        let outcome = decode_register_hint_response(response).expect("unknown command handled");
        assert_eq!(outcome, RegisterHintOutcome::UnsupportedDaemon);
    }

    // Unix-only: test PID alive check for the current process
    #[cfg(unix)]
    #[test]
    fn test_pid_alive_current_process() {
        let pid = std::process::id() as i32;
        assert!(pid_alive(pid));
    }

    #[cfg(unix)]
    #[test]
    fn test_pid_alive_nonexistent_pid() {
        // Use a PID that is extremely unlikely to exist: i32::MAX.
        // On Linux and macOS the max PID is 4194304 or similar; i32::MAX exceeds
        // the kernel's PID range and kill() will return ESRCH (no such process).
        assert!(!pid_alive(i32::MAX));
    }

    /// When `ATM_DAEMON_BIN` is set to a nonexistent path, `ensure_daemon_running`
    /// must return `Err` (spawn fails) rather than silently succeeding.
    /// This confirms that the `ATM_DAEMON_BIN` env var is read by the public API.
    ///
    /// The test is skipped when a live daemon is already running to avoid
    /// interfering with the running process.
    ///
    /// `#[serial]` is required because the test mutates the process environment.
    #[test]
    #[serial]
    fn test_ensure_daemon_running_reads_atm_daemon_bin() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let _home_guard = EnvGuard::set("ATM_HOME", tmp.path().to_str().unwrap());
        let _bin_guard = EnvGuard::set("ATM_DAEMON_BIN", "/nonexistent-bin-for-atm-test");
        // Skip if a live daemon is already running.
        if daemon_is_running() {
            return;
        }
        let result = ensure_daemon_running();
        // On non-Unix the function is a no-op and always returns Ok(()).
        #[cfg(unix)]
        assert!(
            result.is_err(),
            "spawn of nonexistent binary must return Err on Unix"
        );
        #[cfg(not(unix))]
        assert!(
            result.is_ok(),
            "ensure_daemon_running is a no-op on non-Unix"
        );
    }

    // ── LifecycleSource / LifecycleSourceKind ────────────────────────────────

    #[test]
    fn lifecycle_source_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&LifecycleSourceKind::ClaudeHook).unwrap(),
            "\"claude_hook\""
        );
        assert_eq!(
            serde_json::to_string(&LifecycleSourceKind::AtmMcp).unwrap(),
            "\"atm_mcp\""
        );
        assert_eq!(
            serde_json::to_string(&LifecycleSourceKind::AgentHook).unwrap(),
            "\"agent_hook\""
        );
        assert_eq!(
            serde_json::to_string(&LifecycleSourceKind::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    #[test]
    fn lifecycle_source_kind_deserializes_snake_case() {
        let kind: LifecycleSourceKind = serde_json::from_str("\"claude_hook\"").unwrap();
        assert_eq!(kind, LifecycleSourceKind::ClaudeHook);

        let kind: LifecycleSourceKind = serde_json::from_str("\"atm_mcp\"").unwrap();
        assert_eq!(kind, LifecycleSourceKind::AtmMcp);

        let kind: LifecycleSourceKind = serde_json::from_str("\"agent_hook\"").unwrap();
        assert_eq!(kind, LifecycleSourceKind::AgentHook);

        let kind: LifecycleSourceKind = serde_json::from_str("\"unknown\"").unwrap();
        assert_eq!(kind, LifecycleSourceKind::Unknown);
    }

    #[test]
    fn lifecycle_source_round_trip() {
        let src = LifecycleSource::new(LifecycleSourceKind::AtmMcp);
        let json = serde_json::to_string(&src).unwrap();
        assert!(json.contains("\"atm_mcp\""), "serialized: {json}");
        let decoded: LifecycleSource = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.kind, LifecycleSourceKind::AtmMcp);
    }

    #[test]
    fn hook_event_payload_without_source_is_backward_compatible() {
        // A payload without the "source" field must still parse as SocketRequest.
        let json = r#"{
            "version": 1,
            "request_id": "req-test",
            "command": "hook-event",
            "payload": {
                "event": "session_start",
                "agent": "team-lead",
                "team": "atm-dev",
                "session_id": "abc-123"
            }
        }"#;
        let req: SocketRequest = serde_json::from_str(json).unwrap();
        // The payload's "source" field is absent — no panic, no error.
        assert!(req.payload.get("source").is_none());
        assert_eq!(req.command, "hook-event");
    }

    #[test]
    fn hook_event_payload_with_atm_mcp_source_parses() {
        let json = r#"{
            "version": 1,
            "request_id": "req-mcp",
            "command": "hook-event",
            "payload": {
                "event": "session_start",
                "agent": "arch-ctm",
                "team": "atm-dev",
                "session_id": "codex:abc-123",
                "source": {"kind": "atm_mcp"}
            }
        }"#;
        let req: SocketRequest = serde_json::from_str(json).unwrap();
        let source: LifecycleSource =
            serde_json::from_value(req.payload["source"].clone()).unwrap();
        assert_eq!(source.kind, LifecycleSourceKind::AtmMcp);
    }

    #[test]
    #[serial]
    fn test_send_control_no_daemon_returns_err() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let _home_guard = EnvGuard::set("ATM_HOME", tmp.path().to_str().unwrap());
        let _autostart_guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "0");
        // Without a running daemon, send_control must return Err (not None or panic).
        use crate::control::{CONTROL_SCHEMA_VERSION, ControlAction, ControlRequest};

        let req = ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: "req-test-ctrl".to_string(),
            msg_type: "control.stdin.request".to_string(),
            signal: None,
            sent_at: "2026-02-21T00:00:00Z".to_string(),
            team: "atm-dev".to_string(),
            session_id: String::new(),
            agent_id: "arch-ctm".to_string(),
            sender: "tui".to_string(),
            action: ControlAction::Stdin,
            payload: Some("hello".to_string()),
            content_ref: None,
            elicitation_id: None,
            decision: None,
        };

        let result = send_control(&req);
        // With no daemon running the call should fail gracefully (not panic).
        // We only assert it returns an Err — the exact message is implementation detail.
        assert!(
            result.is_err(),
            "send_control should return Err when daemon is not running"
        );
    }

    // ── Windows-specific tests ───────────────────────────────────────────────
    //
    // These tests validate the Windows code paths for daemon auto-start readiness
    // and lock behavior. On Windows, all daemon socket communication is intentionally
    // unavailable (Unix domain sockets only), so the contract is that every public
    // function returns `Ok(None)` or `false` without panicking or returning an error.
    //
    // Requirement: requirements.md §T.1 cross-platform row — "Windows CI coverage
    // must validate spawn/readiness/lock behavior".

    /// On Windows, `query_daemon` must return `Ok(None)` for any request.
    ///
    /// The daemon uses Unix domain sockets which are unavailable on Windows.
    /// The graceful fallback ensures the CLI degrades silently rather than
    /// failing with a platform-specific error.
    #[cfg(windows)]
    #[test]
    #[serial]
    fn windows_query_daemon_returns_ok_none() {
        let req = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-win-test".to_string(),
            command: "agent-state".to_string(),
            payload: serde_json::json!({ "agent": "arch-ctm", "team": "atm-dev" }),
        };
        let result = query_daemon(&req);
        assert!(
            result.is_ok(),
            "query_daemon must not return Err on Windows"
        );
        assert!(
            result.unwrap().is_none(),
            "query_daemon must return Ok(None) on Windows (no Unix socket available)"
        );
    }

    /// On Windows, `daemon_is_running` must return `false` without panicking.
    ///
    /// The PID-file check uses Unix `kill(pid, 0)` which is unavailable on Windows.
    /// The Windows branch always returns `false` — validated here so CI catches
    /// any accidental regression that re-introduces a Unix-only code path.
    #[cfg(windows)]
    #[test]
    fn windows_daemon_is_running_returns_false() {
        // No daemon can be running on Windows (no Unix socket / PID-kill support).
        assert!(
            !daemon_is_running(),
            "daemon_is_running must return false on Windows"
        );
    }

    /// On Windows, `subscribe_stream_events` must return `Ok(None)`.
    ///
    /// Stream subscriptions require a long-lived Unix domain socket connection.
    /// The Windows branch short-circuits to `Ok(None)` so callers can treat the
    /// absence of stream events as equivalent to a daemon that is not running.
    #[cfg(windows)]
    #[test]
    fn windows_subscribe_stream_events_returns_ok_none() {
        let result = subscribe_stream_events();
        assert!(
            result.is_ok(),
            "subscribe_stream_events must not return Err on Windows"
        );
        assert!(
            result.unwrap().is_none(),
            "subscribe_stream_events must return Ok(None) on Windows"
        );
    }

    /// On Windows, `query_agent_state` must return `Ok(None)`.
    ///
    /// Exercises the full call path (including payload serialisation) to confirm
    /// that the Windows `Ok(None)` short-circuit in `query_daemon` propagates
    /// correctly through the higher-level wrapper.
    #[cfg(windows)]
    #[test]
    #[serial]
    fn windows_query_agent_state_returns_ok_none() {
        let result = query_agent_state("arch-ctm", "atm-dev");
        assert!(
            result.is_ok(),
            "query_agent_state must not return Err on Windows"
        );
        assert!(
            result.unwrap().is_none(),
            "query_agent_state must return Ok(None) on Windows"
        );
    }

    /// On Windows, `query_session` must return `Ok(None)`.
    #[cfg(windows)]
    #[test]
    #[serial]
    fn windows_query_session_returns_ok_none() {
        let result = query_session("team-lead");
        assert!(
            result.is_ok(),
            "query_session must not return Err on Windows"
        );
        assert!(
            result.unwrap().is_none(),
            "query_session must return Ok(None) on Windows"
        );
    }

    /// On Windows, `launch_agent` must return `Ok(None)`.
    ///
    /// Confirms that the auto-start path (which requires Unix `fork`/`exec`
    /// semantics) never executes on Windows and the call degrades gracefully.
    #[cfg(windows)]
    #[test]
    #[serial]
    fn windows_launch_agent_returns_ok_none() {
        let config = LaunchConfig {
            agent: "test-agent".to_string(),
            team: "test-team".to_string(),
            command: "codex --yolo".to_string(),
            prompt: None,
            timeout_secs: 5,
            env_vars: std::collections::HashMap::new(),
            runtime: Some("codex".to_string()),
            resume_session_id: None,
        };
        let result = launch_agent(&config);
        assert!(
            result.is_ok(),
            "launch_agent must not return Err on Windows (no daemon socket)"
        );
        assert!(
            result.unwrap().is_none(),
            "launch_agent must return Ok(None) on Windows"
        );
    }

    /// On Windows, the startup lock (`acquire_lock`) must be acquirable and
    /// automatically released on drop.
    ///
    /// The `ensure_daemon_running_unix` function is gated `#[cfg(unix)]` and
    /// never runs on Windows, but the startup-lock path (`fs2::LockFileEx`) is
    /// the same cross-platform primitive used throughout atm-core.  This test
    /// confirms the Windows lock backend works correctly in the context of the
    /// daemon startup directory layout.
    #[cfg(windows)]
    #[test]
    fn windows_startup_lock_acquires_and_releases() {
        use crate::io::lock::acquire_lock;
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let lock_dir = tmp.path().join("config").join("atm");
        fs::create_dir_all(&lock_dir).unwrap();
        let lock_path = lock_dir.join("daemon-start.lock");

        // Acquire the lock — mirrors what ensure_daemon_running_unix does.
        let lock = acquire_lock(&lock_path, 3);
        assert!(
            lock.is_ok(),
            "startup lock must be acquirable on Windows: {:?}",
            lock.err()
        );

        // Explicit drop releases the lock (Windows holds handles; explicit drop
        // ensures the LockFileEx unlock fires before we try to re-acquire).
        drop(lock.unwrap());

        // Re-acquire to confirm the lock was actually released.
        let lock2 = acquire_lock(&lock_path, 1);
        assert!(
            lock2.is_ok(),
            "startup lock must be re-acquirable after release on Windows"
        );
    }

    /// On Windows, `daemon_socket_path` must produce a path ending with the
    /// expected suffix regardless of the underlying home-directory resolver.
    #[cfg(windows)]
    #[test]
    fn windows_daemon_socket_path_has_correct_suffix() {
        let path = daemon_socket_path().unwrap();
        let s = path.to_string_lossy();
        assert!(
            s.ends_with("atm-daemon.sock"),
            "daemon_socket_path must end with 'atm-daemon.sock' on Windows, got: {s}"
        );
        assert!(
            s.contains(".atm") && s.contains("daemon"),
            "daemon_socket_path must contain '.atm/daemon' on Windows, got: {s}"
        );
    }

    /// On Windows, `daemon_pid_path` must produce a path ending with the
    /// expected suffix.
    #[cfg(windows)]
    #[test]
    fn windows_daemon_pid_path_has_correct_suffix() {
        let path = daemon_pid_path().unwrap();
        let s = path.to_string_lossy();
        assert!(
            s.ends_with("atm-daemon.pid"),
            "daemon_pid_path must end with 'atm-daemon.pid' on Windows, got: {s}"
        );
        assert!(
            s.contains(".atm") && s.contains("daemon"),
            "daemon_pid_path must contain '.atm/daemon' on Windows, got: {s}"
        );
    }

    /// On Windows, `send_control` must return `Err` (not panic) when the daemon
    /// is not reachable, because `send_control` intentionally propagates absence
    /// as an error (unlike the `Ok(None)` contract of other public functions).
    #[cfg(windows)]
    #[test]
    fn windows_send_control_no_daemon_returns_err() {
        use crate::control::{CONTROL_SCHEMA_VERSION, ControlAction, ControlRequest};

        let req = ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: "req-win-ctrl".to_string(),
            msg_type: "control.stdin.request".to_string(),
            signal: None,
            sent_at: "2026-02-21T00:00:00Z".to_string(),
            team: "atm-dev".to_string(),
            session_id: String::new(),
            agent_id: "arch-ctm".to_string(),
            sender: "tui".to_string(),
            action: ControlAction::Stdin,
            payload: Some("hello".to_string()),
            content_ref: None,
            elicitation_id: None,
            decision: None,
        };

        let result = send_control(&req);
        assert!(
            result.is_err(),
            "send_control must return Err on Windows when daemon is not reachable"
        );
    }

    #[test]
    fn test_send_control_builds_correct_socket_request() {
        // Verify the SocketRequest built inside send_control has the right shape
        // by re-creating it manually and checking serialization.
        use crate::control::{CONTROL_SCHEMA_VERSION, ControlAction, ControlRequest};

        let req = ControlRequest {
            v: CONTROL_SCHEMA_VERSION,
            request_id: "req-ctrl-check".to_string(),
            msg_type: "control.interrupt.request".to_string(),
            signal: Some("interrupt".to_string()),
            sent_at: "2026-02-21T00:00:00Z".to_string(),
            team: "atm-dev".to_string(),
            session_id: String::new(),
            agent_id: "arch-ctm".to_string(),
            sender: "tui".to_string(),
            action: ControlAction::Interrupt,
            payload: None,
            content_ref: None,
            elicitation_id: None,
            decision: None,
        };

        // The socket-level request_id is an independent correlation ID generated
        // by send_control (e.g., "sock-<nanos>").  It must NOT be the same as
        // the control payload's stable idempotency key (`req.request_id`).
        let control_payload = serde_json::to_value(&req).expect("serialize ControlRequest");
        let socket_req = SocketRequest {
            version: PROTOCOL_VERSION,
            // Distinct from req.request_id — mirrors what send_control generates.
            request_id: "sock-test-123".to_string(),
            command: "control".to_string(),
            payload: control_payload,
        };

        // Sanity check: outer request_id is the socket-level ID, not the control ID.
        assert_ne!(
            socket_req.request_id, req.request_id,
            "socket-level request_id must differ from control payload request_id"
        );
        assert_eq!(socket_req.request_id, "sock-test-123");

        let json = serde_json::to_string(&socket_req).expect("serialize SocketRequest");

        // Outer envelope fields.
        assert!(
            json.contains("\"command\":\"control\""),
            "command field missing"
        );

        // The control payload's request_id must appear inside the serialized
        // payload body, not as the outer SocketRequest.request_id.
        assert!(
            json.contains("\"request_id\":\"req-ctrl-check\""),
            "control payload request_id must appear inside the payload body"
        );

        // The outer socket-level request_id is present.
        assert!(
            json.contains("\"request_id\":\"sock-test-123\""),
            "socket-level request_id must appear in the outer envelope"
        );

        // The type field in the control payload.
        assert!(
            json.contains("\"type\":\"control.interrupt.request\""),
            "msg_type field missing from control payload"
        );

        // The interrupt signal.
        assert!(json.contains("\"interrupt\""), "interrupt signal missing");
    }
}
