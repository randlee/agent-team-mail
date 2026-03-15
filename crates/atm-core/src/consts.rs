//! Shared `atm-core` constants used across daemon-client and hook identity paths.

/// Maximum age for hook/session identity files before they are considered stale.
pub const SESSION_FILE_TTL_SECS: f64 = 86_400.0;

/// Timeout for short daemon query/response socket exchanges.
pub const DAEMON_QUERY_TIMEOUT_MS: u64 = 500;

/// Best-effort timeout for one-way log forwarding to the daemon socket.
pub const LOG_FORWARD_TIMEOUT_MS: u64 = 100;

/// Maximum time to wait for a freshly started daemon to become reachable.
pub const STARTUP_DEADLINE_SECS: u64 = 5;

/// Retry delay for daemon startup/connect polling loops.
pub const RETRY_SLEEP_MS: u64 = 100;

/// Read/write timeout for daemon socket I/O once a connection is established.
pub const SOCKET_IO_TIMEOUT_MS: u64 = 500;

/// Polling sleep used when checking for daemon readiness.
pub const POLL_CHECK_SLEEP_MS: u64 = 25;

/// Short sleep used in test-only daemon readiness loops.
pub const SHORT_SLEEP_MS: u64 = 50;

/// Capacity for the producer fan-in logging channel.
pub const LOG_EVENT_CHANNEL_CAPACITY: usize = 512;
