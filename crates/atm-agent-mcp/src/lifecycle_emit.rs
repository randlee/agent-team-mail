//! Best-effort lifecycle event emission to the ATM daemon.
//!
//! This module provides [`emit_lifecycle_event`], which sends a `hook-event`
//! socket command to the ATM daemon whenever a Codex agent session transitions
//! between lifecycle states (session open, idle, close).
//!
//! ## Design
//!
//! - Emissions are **best-effort**: if the daemon is not running or an I/O
//!   error occurs, the function logs a warning and returns — it never panics
//!   or propagates errors that would crash the proxy.
//! - Events are tagged with `source.kind = "atm_mcp"` so the daemon applies
//!   the relaxed validation policy for MCP-sourced events (any team member
//!   may emit lifecycle signals, not just the team-lead).
//! - The implementation is Unix-only.  On non-Unix platforms all functions
//!   compile to no-ops.
//!
//! ## Usage
//!
//! ```rust,ignore
//! // After a session is registered:
//! lifecycle_emit::emit_lifecycle_event(
//!     EventKind::SessionStart,
//!     &entry.identity,
//!     &team,
//!     &entry.agent_id,
//!     None,
//! ).await;
//!
//! // After a turn completes (thread → Idle):
//! lifecycle_emit::emit_lifecycle_event(
//!     EventKind::TeammateIdle,
//!     &identity,
//!     &team,
//!     &agent_id,
//!     None,
//! ).await;
//!
//! // After a session is closed:
//! lifecycle_emit::emit_lifecycle_event(
//!     EventKind::SessionEnd,
//!     &identity,
//!     &team,
//!     &agent_id,
//!     None,
//! ).await;
//! ```

/// The kind of lifecycle event to emit to the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A new Codex session has been established.
    SessionStart,
    /// A Codex thread completed its turn and is now idle.
    TeammateIdle,
    /// A Codex session has been closed or torn down.
    SessionEnd,
}

impl EventKind {
    /// Return the event string used in the `hook-event` payload.
    fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::TeammateIdle => "teammate_idle",
            Self::SessionEnd => "session_end",
        }
    }
}

/// Emit a lifecycle event to the ATM daemon via the Unix socket.
///
/// This function is **best-effort**: errors are logged at `warn` level and
/// never propagated to the caller.  The proxy continues normally whether or
/// not the daemon is reachable.
///
/// # Arguments
///
/// * `kind`       - The lifecycle transition to report.
/// * `identity`   - ATM identity of the agent (e.g., `"arch-ctm"`).
/// * `team`       - ATM team name (e.g., `"atm-dev"`).
/// * `session_id` - The `agent_id` from the [`crate::session::SessionEntry`],
///   format `"codex:<uuid>"`.
/// * `process_id` - Optional OS process ID; pass `None` when not applicable.
///
/// # Platform behaviour
///
/// On non-Unix platforms this function is a no-op and returns immediately.
pub async fn emit_lifecycle_event(
    kind: EventKind,
    identity: &str,
    team: &str,
    session_id: &str,
    process_id: Option<u32>,
) {
    #[cfg(unix)]
    {
        if let Err(e) =
            emit_lifecycle_event_unix(kind, identity, team, session_id, process_id).await
        {
            tracing::warn!(
                event = kind.as_str(),
                agent = identity,
                team = team,
                session_id = session_id,
                "lifecycle_emit: failed to notify daemon (non-fatal): {e}"
            );
        }
    }

    // Suppress unused-variable warnings on non-Unix platforms.
    #[cfg(not(unix))]
    {
        let _ = (kind, identity, team, session_id, process_id);
    }
}

// ── Unix implementation ───────────────────────────────────────────────────────

#[cfg(unix)]
async fn emit_lifecycle_event_unix(
    kind: EventKind,
    identity: &str,
    team: &str,
    session_id: &str,
    process_id: Option<u32>,
) -> anyhow::Result<()> {
    use agent_team_mail_core::daemon_client::{
        PROTOCOL_VERSION, SocketRequest, SocketResponse,
        LifecycleSource, LifecycleSourceKind,
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let socket_path = agent_team_mail_core::daemon_client::daemon_socket_path()?;

    // Attempt connection — return early (without error) if daemon not running.
    let stream = match UnixStream::connect(&socket_path).await {
        Ok(s) => s,
        Err(_) => {
            // Daemon not running — this is the expected steady state in CI
            // and development environments without a daemon.  Treat as no-op.
            return Ok(());
        }
    };

    let mut payload = serde_json::json!({
        "event": kind.as_str(),
        "agent": identity,
        "team": team,
        "session_id": session_id,
        "source": LifecycleSource::new(LifecycleSourceKind::AtmMcp),
    });

    // Include process_id for session_start so the daemon can record it.
    if let Some(pid) = process_id {
        payload["process_id"] = serde_json::json!(pid);
    }

    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "hook-event".to_string(),
        payload,
    };

    let request_line = serde_json::to_string(&request)?;

    let (reader_half, mut writer_half) = stream.into_split();

    // Write request (newline-delimited protocol).
    writer_half.write_all(request_line.as_bytes()).await?;
    writer_half.write_all(b"\n").await?;
    writer_half.flush().await?;

    // Read response with a short timeout so we do not block the proxy.
    let read_result = tokio::time::timeout(
        tokio::time::Duration::from_millis(500),
        async {
            let mut reader = BufReader::new(reader_half);
            let mut line = String::new();
            reader.read_line(&mut line).await?;
            Ok::<String, std::io::Error>(line)
        },
    )
    .await;

    match read_result {
        Ok(Ok(line)) if !line.trim().is_empty() => {
            // Parse the response for diagnostic logging only; errors here are non-fatal.
            if let Ok(resp) = serde_json::from_str::<SocketResponse>(line.trim()) {
                if !resp.is_ok() {
                    // Daemon rejected the event (e.g., agent not in team).
                    // Log at debug — this may be expected during tests.
                    tracing::debug!(
                        event = kind.as_str(),
                        agent = identity,
                        "lifecycle_emit: daemon rejected event: {:?}",
                        resp.error
                    );
                }
            }
        }
        Ok(Err(_)) | Err(_) => {
            // Read error or timeout — non-fatal, daemon may have been slow.
            tracing::debug!(
                event = kind.as_str(),
                agent = identity,
                "lifecycle_emit: no response from daemon within timeout"
            );
        }
        Ok(Ok(_)) => {} // empty line — ignore
    }

    Ok(())
}

/// Generate a short unique request ID for the socket envelope.
#[cfg(unix)]
fn new_request_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let pid = std::process::id();
    format!("mcp-lifecycle-{pid}-{nanos}")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── EventKind helpers ─────────────────────────────────────────────────────

    #[test]
    fn event_kind_as_str_matches_daemon_protocol() {
        assert_eq!(EventKind::SessionStart.as_str(), "session_start");
        assert_eq!(EventKind::TeammateIdle.as_str(), "teammate_idle");
        assert_eq!(EventKind::SessionEnd.as_str(), "session_end");
    }

    /// Verify that `EventKind::SessionStart` maps to the exact daemon protocol
    /// string used by the `proxy.rs` session-registration call site.
    #[test]
    fn event_kind_session_start_maps_to_protocol_string() {
        assert_eq!(EventKind::SessionStart.as_str(), "session_start");
    }

    /// Verify that `EventKind::TeammateIdle` maps to the exact daemon protocol
    /// string used by the `proxy.rs` thread-idle call site.
    #[test]
    fn event_kind_teammate_idle_maps_to_protocol_string() {
        assert_eq!(EventKind::TeammateIdle.as_str(), "teammate_idle");
    }

    /// Verify that `EventKind::SessionEnd` maps to the exact daemon protocol
    /// string used by the `atm_tools.rs` session-close call site.
    #[test]
    fn event_kind_session_end_maps_to_protocol_string() {
        assert_eq!(EventKind::SessionEnd.as_str(), "session_end");
    }

    /// Emitting with no daemon running must not panic or return an error.
    #[tokio::test]
    async fn emit_lifecycle_event_no_daemon_is_noop() {
        // Override ATM_HOME to a temp dir where no daemon socket exists.
        let dir = tempfile::tempdir().expect("temp dir");
        // SAFETY: test-only env mutation; no parallelism with other tests
        // that use ATM_HOME because tokio tests are single-threaded by default.
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        // None of these should panic.
        emit_lifecycle_event(
            EventKind::SessionStart,
            "arch-ctm",
            "atm-dev",
            "codex:test-1234",
            Some(9999),
        )
        .await;

        emit_lifecycle_event(
            EventKind::TeammateIdle,
            "arch-ctm",
            "atm-dev",
            "codex:test-1234",
            None,
        )
        .await;

        emit_lifecycle_event(
            EventKind::SessionEnd,
            "arch-ctm",
            "atm-dev",
            "codex:test-1234",
            None,
        )
        .await;

        // Clean up env.
        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }

    /// Verify the session_start payload includes `source.kind = "atm_mcp"`.
    #[test]
    fn session_start_payload_has_atm_mcp_source() {
        use agent_team_mail_core::daemon_client::{
            PROTOCOL_VERSION, SocketRequest, LifecycleSource, LifecycleSourceKind,
        };

        // Build the same payload that emit_lifecycle_event_unix would build.
        let payload = serde_json::json!({
            "event": EventKind::SessionStart.as_str(),
            "agent": "arch-ctm",
            "team": "atm-dev",
            "session_id": "codex:abc-123",
            "source": LifecycleSource::new(LifecycleSourceKind::AtmMcp),
            "process_id": 1234u32,
        });

        let req = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-test".to_string(),
            command: "hook-event".to_string(),
            payload: payload.clone(),
        };

        let json = serde_json::to_string(&req).unwrap();

        // Verify the source field is present and has kind "atm_mcp".
        assert!(
            json.contains("\"atm_mcp\""),
            "payload must include source.kind = atm_mcp; got: {json}"
        );
        assert!(
            json.contains("\"session_start\""),
            "payload must include event = session_start; got: {json}"
        );

        // Deserialize the source back and verify kind.
        let source: LifecycleSource =
            serde_json::from_value(payload["source"].clone()).unwrap();
        assert_eq!(source.kind, LifecycleSourceKind::AtmMcp);
    }

    /// Verify that the `teammate_idle` payload (as built by `proxy.rs`) includes
    /// `source.kind = "atm_mcp"` and the correct event type string.
    ///
    /// This test mirrors the call site in `proxy.rs` that uses
    /// `EventKind::TeammateIdle` when a Codex thread completes its turn.
    #[test]
    fn teammate_idle_payload_structure_is_correct() {
        use agent_team_mail_core::daemon_client::{
            LifecycleSource, LifecycleSourceKind, PROTOCOL_VERSION,
            SocketRequest,
        };

        // Build the same payload that emit_lifecycle_event_unix constructs for
        // the TeammateIdle variant (process_id is None at the idle call site).
        let payload = serde_json::json!({
            "event": EventKind::TeammateIdle.as_str(),
            "agent": "arch-ctm",
            "team": "atm-dev",
            "session_id": "codex:abc-456",
            "source": LifecycleSource::new(LifecycleSourceKind::AtmMcp),
        });

        let req = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-idle-test".to_string(),
            command: "hook-event".to_string(),
            payload: payload.clone(),
        };

        let json = serde_json::to_string(&req).unwrap();

        assert!(
            json.contains("\"atm_mcp\""),
            "teammate_idle payload must include source.kind = atm_mcp; got: {json}"
        );
        assert!(
            json.contains("\"teammate_idle\""),
            "teammate_idle payload must include event = teammate_idle; got: {json}"
        );

        // Deserialize the source back and verify kind.
        let source: LifecycleSource =
            serde_json::from_value(payload["source"].clone()).unwrap();
        assert_eq!(source.kind, LifecycleSourceKind::AtmMcp);
    }

    /// Verify that the `session_end` payload (as built by `atm_tools.rs`) includes
    /// `source.kind = "atm_mcp"` and the correct event type string.
    ///
    /// This test mirrors the call site in `atm_tools.rs` that uses
    /// `EventKind::SessionEnd` when a Codex session is closed or torn down.
    #[test]
    fn session_end_payload_structure_is_correct() {
        use agent_team_mail_core::daemon_client::{
            LifecycleSource, LifecycleSourceKind, PROTOCOL_VERSION,
            SocketRequest,
        };

        // Build the same payload that emit_lifecycle_event_unix constructs for
        // the SessionEnd variant (process_id is None at the session-end call site).
        let payload = serde_json::json!({
            "event": EventKind::SessionEnd.as_str(),
            "agent": "arch-ctm",
            "team": "atm-dev",
            "session_id": "codex:abc-789",
            "source": LifecycleSource::new(LifecycleSourceKind::AtmMcp),
        });

        let req = SocketRequest {
            version: PROTOCOL_VERSION,
            request_id: "req-end-test".to_string(),
            command: "hook-event".to_string(),
            payload: payload.clone(),
        };

        let json = serde_json::to_string(&req).unwrap();

        assert!(
            json.contains("\"atm_mcp\""),
            "session_end payload must include source.kind = atm_mcp; got: {json}"
        );
        assert!(
            json.contains("\"session_end\""),
            "session_end payload must include event = session_end; got: {json}"
        );

        // Deserialize the source back and verify kind.
        let source: LifecycleSource =
            serde_json::from_value(payload["source"].clone()).unwrap();
        assert_eq!(source.kind, LifecycleSourceKind::AtmMcp);
    }
}
