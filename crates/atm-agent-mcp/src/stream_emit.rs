//! Best-effort [`DaemonStreamEvent`] emission to the ATM daemon.
//!
//! Mirrors the design of [`crate::lifecycle_emit`]:
//! - Emissions are **best-effort** (warn and return on error, never panic)
//! - Unix-only; no-op stubs on non-Unix platforms
//! - Uses the daemon socket at `${ATM_HOME}/.atm/daemon/atm-daemon.sock`
//! - Short 200 ms timeout so the proxy is never blocked by a slow daemon
//!
//! # Usage
//!
//! ```rust,ignore
//! use agent_team_mail_core::daemon_stream::DaemonStreamEvent;
//! use crate::stream_emit::emit_stream_event;
//!
//! emit_stream_event(&DaemonStreamEvent::TurnStarted {
//!     agent: "arch-ctm".to_string(),
//!     thread_id: "th-1".to_string(),
//!     turn_id: "turn-abc".to_string(),
//!     transport: "app-server".to_string(),
//! }).await;
//! ```

use agent_team_mail_core::daemon_stream::DaemonStreamEvent;

/// Emit a [`DaemonStreamEvent`] to the ATM daemon via the Unix socket.
///
/// Best-effort: errors are logged at `warn` level and never propagated.
///
/// On non-Unix platforms this function is a no-op.
pub async fn emit_stream_event(event: &DaemonStreamEvent) {
    #[cfg(unix)]
    {
        if let Err(e) = emit_stream_event_unix(event).await {
            tracing::warn!(
                event = ?event,
                "stream_emit: failed to notify daemon (non-fatal): {e}"
            );
        }
    }

    #[cfg(not(unix))]
    {
        let _ = event;
    }
}

// ── Unix implementation ─────────────────────────────────────────────────────

#[cfg(unix)]
async fn emit_stream_event_unix(event: &DaemonStreamEvent) -> anyhow::Result<()> {
    use agent_team_mail_core::daemon_client::{PROTOCOL_VERSION, SocketRequest, SocketResponse};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let socket_path = agent_team_mail_core::daemon_client::daemon_socket_path()?;

    // Attempt connection — return early (without error) if daemon not running.
    let stream = match UnixStream::connect(&socket_path).await {
        Ok(s) => s,
        Err(_) => {
            // Daemon not running — expected in CI / development without a daemon.
            return Ok(());
        }
    };

    let payload =
        serde_json::to_value(event).map_err(|e| anyhow::anyhow!("serialize event: {e}"))?;

    let request = SocketRequest {
        version: PROTOCOL_VERSION,
        request_id: new_request_id(),
        command: "stream-event".to_string(),
        payload,
    };

    let request_line = serde_json::to_string(&request)?;

    let (reader_half, mut writer_half) = stream.into_split();

    // Write request (newline-delimited protocol).
    writer_half.write_all(request_line.as_bytes()).await?;
    writer_half.write_all(b"\n").await?;
    writer_half.flush().await?;

    // Read response with a short timeout so we do not block the proxy.
    let read_result = tokio::time::timeout(tokio::time::Duration::from_millis(200), async {
        let mut reader = BufReader::new(reader_half);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        Ok::<String, std::io::Error>(line)
    })
    .await;

    match read_result {
        Ok(Ok(line)) if !line.trim().is_empty() => {
            if let Ok(resp) = serde_json::from_str::<SocketResponse>(line.trim()) {
                if !resp.is_ok() {
                    tracing::debug!(
                        event = ?event,
                        "stream_emit: daemon rejected event: {:?}",
                        resp.error
                    );
                }
            }
        }
        Ok(Err(_)) | Err(_) => {
            tracing::debug!("stream_emit: no response from daemon within timeout");
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
    format!("mcp-stream-{pid}-{nanos}")
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Emitting with no daemon running must not panic or return an error.
    #[tokio::test]
    #[serial]
    async fn emit_stream_event_no_daemon_is_noop() {
        let dir = tempfile::tempdir().expect("temp dir");
        // SAFETY: test-only env mutation; single-threaded tokio test.
        unsafe {
            std::env::set_var("ATM_HOME", dir.path());
        }

        emit_stream_event(&DaemonStreamEvent::TurnStarted {
            agent: "test-agent".to_string(),
            thread_id: "th-1".to_string(),
            turn_id: "turn-1".to_string(),
            transport: "app-server".to_string(),
        })
        .await;

        emit_stream_event(&DaemonStreamEvent::TurnIdle {
            agent: "test-agent".to_string(),
            turn_id: "turn-1".to_string(),
            transport: "cli-json".to_string(),
        })
        .await;

        unsafe {
            std::env::remove_var("ATM_HOME");
        }
    }
}
