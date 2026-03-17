//! Shared `atm-core` constants used across daemon-client and hook identity paths.

/// Maximum age for hook/session identity files before they are considered stale.
pub const SESSION_FILE_TTL_SECS: f64 = 86_400.0;

/// Timeout for short daemon query/response socket exchanges.
pub const DAEMON_QUERY_TIMEOUT_MS: u64 = 500;

/// Best-effort timeout for one-way log forwarding to the daemon socket.
pub const LOG_FORWARD_TIMEOUT_MS: u64 = 100;

/// Maximum time to wait for a freshly started daemon to become reachable.
pub const STARTUP_DEADLINE_SECS: u64 = 5;

/// Socket wait budget used by integration tests that start fake daemons.
pub const WAIT_FOR_DAEMON_SOCKET_SECS: u64 = 10;

/// Minimum timeout budget added around daemon start and drain requests.
pub const DAEMON_TIMEOUT_MIN_SECS: u64 = 30;

/// Maximum timeout budget for long-running daemon operations in tests and CLI waits.
pub const DAEMON_TIMEOUT_MAX_SECS: u64 = 600;

/// Retry delay for daemon startup/connect polling loops.
pub const RETRY_SLEEP_MS: u64 = 100;

/// Read/write timeout for daemon socket I/O once a connection is established.
pub const SOCKET_IO_TIMEOUT_MS: u64 = 500;

/// Polling sleep used when checking for daemon readiness.
pub const POLL_CHECK_SLEEP_MS: u64 = 25;

/// Short sleep used in test-only daemon readiness loops.
pub const SHORT_SLEEP_MS: u64 = 50;

/// Short deadline used in test-only daemon wait loops.
pub const SHORT_DEADLINE_SECS: u64 = 2;

/// Brief settle delay before re-reading daemon metadata written asynchronously after startup.
pub const DAEMON_METADATA_SETTLE_MS: u64 = 150;

/// Default TTL for explicitly created isolated runtimes.
pub const ISOLATED_RUNTIME_DEFAULT_TTL_SECS: u64 = 600;

/// Default graceful drain timeout for gh monitor stop/restart operations.
///
/// Keep this below common interactive command timeouts so lifecycle control
/// returns promptly even when an in-flight monitor needs to drain first.
pub const GH_MONITOR_DEFAULT_DRAIN_TIMEOUT_SECS: u64 = 10;

/// Team-level GitHub API budget per hour for shared monitor polling.
pub const GH_BUDGET_LIMIT_PER_HOUR: u64 = 100;

/// Warning threshold for the shared GitHub API budget window.
pub const GH_WARNING_THRESHOLD: u64 = 50;

/// TTL for cached per-repo shared monitor state.
pub const GH_REPO_STATE_TTL_SECS: i64 = 300;

/// Active shared-poller cadence while at least one monitor subscription exists.
pub const GH_ACTIVE_POLL_INTERVAL_SECS: u64 = 60;

/// Idle shared-poller cadence when no monitor subscription exists.
pub const GH_IDLE_POLL_INTERVAL_SECS: u64 = 300;

/// Capacity for the producer fan-in logging channel.
pub const LOG_EVENT_CHANNEL_CAPACITY: usize = 512;
