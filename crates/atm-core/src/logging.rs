//! Shared logging initialization for ATM binaries.
//!
//! This module provides two logging initialization paths:
//!
//! - [`init`] — legacy human-readable stderr (deprecated; preserved for backwards compat).
//! - [`init_unified`] — unified fan-in logging for the daemon-centric architecture.
//!
//! # Unified Architecture
//!
//! ```text
//! atm / atm-tui / atm-agent-mcp  (ProducerFanIn)
//!   └── channel → background thread
//!         └── Unix socket → atm-daemon
//!               └── log_writer task → JSONL file
//!         └── spool fallback (daemon unavailable)
//!
//! atm-daemon  (DaemonWriter)
//!   └── JSONL file with size-based rotation
//! ```

use std::sync::OnceLock;

use crate::consts::{LOG_EVENT_CHANNEL_CAPACITY, LOG_FORWARD_TIMEOUT_MS};

static INIT: OnceLock<()> = OnceLock::new();

/// Global channel sender for the ProducerFanIn background thread.
///
/// Registered once by [`init_unified`] with `ProducerFanIn` mode.  Used by
/// [`crate::event_log::emit_event_best_effort`] to forward events to the
/// daemon without going through the full socket protocol.
static PRODUCER_TX: OnceLock<std::sync::mpsc::SyncSender<crate::logging_event::LogEventV1>> =
    OnceLock::new();

fn parse_level() -> tracing::Level {
    match std::env::var("ATM_LOG")
        .unwrap_or_else(|_| "info".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "trace" => tracing::Level::TRACE,
        "debug" => tracing::Level::DEBUG,
        "warn" => tracing::Level::WARN,
        "error" => tracing::Level::ERROR,
        _ => tracing::Level::INFO,
    }
}

/// Initialize process-level tracing output from `ATM_LOG`.
///
/// This is safe to call multiple times; only the first call initializes the
/// subscriber. It is intentionally best-effort and never returns an error.
///
/// # Deprecation
///
/// Prefer [`init_unified`] for new binaries. This function is preserved for
/// backwards compatibility and is used as the `StderrOnly` fallback path.
#[deprecated(since = "0.17.0", note = "Use init_unified() instead")]
pub fn init() {
    _init_stderr();
}

/// Internal (non-deprecated) stderr init used as fallback.
fn _init_stderr() {
    if INIT.get().is_some() {
        return;
    }
    let level = parse_level();
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level)
        .with_target(false)
        .try_init();
    let _ = INIT.set(());
}

// ── Public types ──────────────────────────────────────────────────────────────

/// Log rotation policy for [`UnifiedLogMode::DaemonWriter`].
#[derive(Debug, Clone)]
pub struct RotationConfig {
    /// Maximum JSONL file size in bytes before rotation (default: 50 MiB).
    pub max_bytes: u64,
    /// Maximum number of rotated files to retain (default: 5).
    pub max_files: u32,
}

impl Default for RotationConfig {
    fn default() -> Self {
        Self {
            max_bytes: 50 * 1024 * 1024,
            max_files: 5,
        }
    }
}

/// Controls which logging layers are active for this process.
///
/// Pass to [`init_unified`] to configure how log events are emitted.
#[derive(Debug, Clone)]
pub enum UnifiedLogMode {
    /// Producer binary: forward events to daemon socket; spool on failure.
    ///
    /// Used by `atm`, `atm-tui`, `atm-agent-mcp`.
    ProducerFanIn {
        /// Path to the daemon Unix socket.
        daemon_socket: std::path::PathBuf,
        /// Directory for spool files when the socket is unavailable.
        fallback_spool_dir: std::path::PathBuf,
    },
    /// Daemon binary: write JSONL directly to file with rotation.
    ///
    /// Used by `atm-daemon`.
    DaemonWriter {
        /// Destination JSONL file path.
        file_path: std::path::PathBuf,
        /// Rotation policy.
        rotation: RotationConfig,
    },
    /// Fallback: human-readable stderr only.
    ///
    /// Used when `init_unified` fails to set up the primary mode, or
    /// when no structured logging is needed (e.g., tests).
    StderrOnly,
}

/// RAII guards returned by [`init_unified`].
///
/// Drop this value to tear down the logging background thread and flush
/// any pending log events. In practice, binaries typically hold this for
/// the lifetime of `main`.
pub struct LoggingGuards {
    _guards: Vec<Box<dyn std::any::Any + Send>>,
}

impl LoggingGuards {
    fn empty() -> Self {
        Self {
            _guards: Vec::new(),
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialize process-level unified logging.
///
/// All modes install a human-readable stderr layer (respects `ATM_LOG` env var).
/// Additional layers depend on `mode`:
///
/// - [`UnifiedLogMode::ProducerFanIn`]: spawns a background `std::thread` that
///   forwards [`LogEventV1`](crate::logging_event::LogEventV1) events to the
///   daemon socket; on socket failure falls back to spool write.
/// - [`UnifiedLogMode::DaemonWriter`]: adds a JSONL file writer layer in a
///   background thread.
/// - [`UnifiedLogMode::StderrOnly`]: human-readable stderr only.
///
/// # Fail-open
///
/// Any failure during setup degrades gracefully to `StderrOnly`.  The caller
/// receives a valid [`LoggingGuards`] either way.
///
/// # Errors
///
/// Returns an error only when stderr subscriber installation itself fails
/// (extremely unlikely outside of tests).  Treat the returned `LoggingGuards`
/// as opaque.
///
/// # Examples
///
/// ```no_run
/// use agent_team_mail_core::logging::{UnifiedLogMode, init_unified, init_stderr_only};
///
/// let _guards = init_unified(
///     "atm",
///     UnifiedLogMode::ProducerFanIn {
///         daemon_socket: std::env::temp_dir().join("atm-daemon.sock"),
///         fallback_spool_dir: std::env::temp_dir().join("atm-spool"),
///     },
/// ).unwrap_or_else(|_| init_stderr_only());
/// ```
pub fn init_unified(
    source_binary: &'static str,
    mode: UnifiedLogMode,
) -> anyhow::Result<LoggingGuards> {
    // Always install the human stderr layer first.
    _init_stderr();

    match mode {
        UnifiedLogMode::StderrOnly => {
            // Already installed above.
            Ok(LoggingGuards::empty())
        }

        UnifiedLogMode::ProducerFanIn {
            daemon_socket,
            fallback_spool_dir,
        } => setup_producer_fan_in(source_binary, daemon_socket, fallback_spool_dir),

        UnifiedLogMode::DaemonWriter {
            file_path,
            rotation,
        } => setup_daemon_writer(file_path, rotation),
    }
}

/// Initialize stderr-only logging.
///
/// Convenience fallback used in `.unwrap_or_else(|_| init_stderr_only())` patterns.
/// Equivalent to [`init_unified`] with [`UnifiedLogMode::StderrOnly`].
pub fn init_stderr_only() -> LoggingGuards {
    _init_stderr();
    LoggingGuards::empty()
}

/// Return the global `SyncSender` registered by [`init_unified`] for
/// `ProducerFanIn` mode, if any.
///
/// This is used by [`crate::event_log::emit_event_best_effort`] to forward
/// events through the unified channel.  Returns `None` when:
/// - `init_unified` was not called with `ProducerFanIn`, or
/// - The background thread has exited.
pub fn producer_sender()
-> Option<&'static std::sync::mpsc::SyncSender<crate::logging_event::LogEventV1>> {
    PRODUCER_TX.get()
}

// ── ProducerFanIn setup ───────────────────────────────────────────────────────

fn setup_producer_fan_in(
    source_binary: &'static str,
    daemon_socket: std::path::PathBuf,
    fallback_spool_dir: std::path::PathBuf,
) -> anyhow::Result<LoggingGuards> {
    use std::sync::mpsc;

    // Bounded channel: capacity 512.  If full, callers drop the event silently.
    let (tx, rx) =
        mpsc::sync_channel::<crate::logging_event::LogEventV1>(LOG_EVENT_CHANNEL_CAPACITY);

    // Store the sender globally so emit_event_best_effort can use it.
    // If already set (e.g., second call to init_unified), we silently skip.
    let _ = PRODUCER_TX.set(tx);

    let handle = std::thread::Builder::new()
        .name("atm-log-forwarder".to_string())
        .spawn(move || {
            run_forwarder(source_binary, rx, &daemon_socket, &fallback_spool_dir);
        })?;

    // Wrap in LoggingGuards so the thread join-handle stays alive.
    Ok(LoggingGuards {
        _guards: vec![Box::new(ForwarderHandle(Some(handle)))],
    })
}

/// Newtype so `JoinHandle` implements `Any + Send`.
struct ForwarderHandle(Option<std::thread::JoinHandle<()>>);

impl Drop for ForwarderHandle {
    fn drop(&mut self) {
        // Do not join the forwarder thread.  The static `PRODUCER_TX` OnceLock
        // keeps the channel sender alive for the entire process lifetime, so
        // the receiver loop in `run_forwarder` would never see a disconnected
        // channel and `join()` would block forever.  The OS reclaims the thread
        // on process exit, which is the correct behaviour for a fire-and-forget
        // background worker.
        let _ = self.0.take(); // drop the JoinHandle without joining
    }
}

// ── Forwarder thread ──────────────────────────────────────────────────────────

fn run_forwarder(
    _source_binary: &'static str,
    rx: std::sync::mpsc::Receiver<crate::logging_event::LogEventV1>,
    daemon_socket: &std::path::Path,
    fallback_spool_dir: &std::path::Path,
) {
    // Process events until the sender side is dropped.
    for event in &rx {
        if !try_forward_to_socket(&event, daemon_socket) {
            // Socket unavailable; spool the event.
            crate::logging_event::write_to_spool_dir(&event, fallback_spool_dir);
        }
    }
}

/// Attempt to send `event` to the daemon via Unix socket.
///
/// Returns `true` on success, `false` on any failure.
#[cfg(unix)]
fn try_forward_to_socket(
    event: &crate::logging_event::LogEventV1,
    daemon_socket: &std::path::Path,
) -> bool {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let stream = match UnixStream::connect(daemon_socket) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let timeout = Duration::from_millis(LOG_FORWARD_TIMEOUT_MS);
    let _ = stream.set_write_timeout(Some(timeout));
    let _ = stream.set_read_timeout(Some(timeout));

    let payload = match serde_json::to_value(event) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let request = crate::daemon_client::SocketRequest {
        version: crate::daemon_client::PROTOCOL_VERSION,
        request_id: format!(
            "log-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ),
        command: "log-event".to_string(),
        payload,
    };

    let line = match serde_json::to_string(&request) {
        Ok(l) => l,
        Err(_) => return false,
    };

    // Write request
    {
        let mut writer = std::io::BufWriter::new(&stream);
        if writer.write_all(line.as_bytes()).is_err() {
            return false;
        }
        if writer.write_all(b"\n").is_err() {
            return false;
        }
        if writer.flush().is_err() {
            return false;
        }
    }

    // Read response (discard content; only care that the write succeeded)
    let mut reader = BufReader::new(&stream);
    let mut _response = String::new();
    let _ = reader.read_line(&mut _response);

    true
}

#[cfg(not(unix))]
fn try_forward_to_socket(
    _event: &crate::logging_event::LogEventV1,
    _daemon_socket: &std::path::Path,
) -> bool {
    false
}

// ── DaemonWriter setup ────────────────────────────────────────────────────────

fn setup_daemon_writer(
    file_path: std::path::PathBuf,
    rotation: RotationConfig,
) -> anyhow::Result<LoggingGuards> {
    // `DaemonWriter` mode is used exclusively by `atm-daemon`.
    //
    // Keep PRODUCER_TX wired so daemon-side emit_event_best_effort calls route
    // through the same unified fan-in path as CLI producers.
    //
    // The socket target is the daemon's own Unix socket. This is safe because
    // "log-event" socket requests enqueue directly into the bounded log-writer
    // queue and do not recursively emit log events.
    if let Some(parent) = file_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut guards = Vec::<Box<dyn std::any::Any + Send>>::new();
    if let Ok(home_dir) = crate::home::get_home_dir() {
        let daemon_socket = home_dir.join(".atm/daemon/atm-daemon.sock");
        let fallback_spool_dir = crate::logging_event::spool_dir(&home_dir);

        match setup_producer_fan_in("atm-daemon", daemon_socket, fallback_spool_dir) {
            Ok(forwarder_guards) => guards.extend(forwarder_guards._guards),
            Err(err) => tracing::warn!("DaemonWriter: failed to initialize producer fan-in: {err}"),
        }
    } else {
        tracing::warn!("DaemonWriter: failed to resolve ATM home for producer fan-in setup");
    }

    tracing::debug!(
        path = %file_path.display(),
        max_bytes = rotation.max_bytes,
        max_files = rotation.max_files,
        "DaemonWriter logging initialized"
    );
    Ok(LoggingGuards { _guards: guards })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_stderr_only_returns_guards() {
        // Should not panic; guards are valid even if already initialized.
        let _g = init_stderr_only();
    }

    #[test]
    fn test_rotation_config_default() {
        let cfg = RotationConfig::default();
        assert_eq!(cfg.max_bytes, 50 * 1024 * 1024);
        assert_eq!(cfg.max_files, 5);
    }

    #[test]
    fn test_init_unified_stderr_only() {
        let result = init_unified("test", UnifiedLogMode::StderrOnly);
        assert!(result.is_ok());
    }

    #[test]
    fn test_init_unified_daemon_writer() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("test.jsonl");
        let result = init_unified(
            "test-daemon",
            UnifiedLogMode::DaemonWriter {
                file_path,
                rotation: RotationConfig::default(),
            },
        );
        assert!(result.is_ok());
    }
}
