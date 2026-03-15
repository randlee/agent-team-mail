//! Shared daemon runtime constants.

/// Interval between spool drain passes in the daemon event loop.
pub const SPOOL_DRAIN_INTERVAL_SECS: u64 = 10;

/// Capacity of the watcher-to-dispatch inbox event channel.
pub const EVENT_CHANNEL_CAPACITY: usize = 100;

/// Grace period for background task and plugin shutdown during daemon exit.
pub const GRACEFUL_SHUTDOWN_TIMEOUT_SECS: u64 = 5;

/// Interval between reconcile passes in the daemon event loop.
pub const RECONCILE_INTERVAL_SECS: u64 = 5;

/// Interval between status.json writes.
pub const STATUS_WRITE_INTERVAL_SECS: u64 = 30;

/// Backoff delay after an accept error on the daemon socket.
pub const SOCKET_RETRY_DELAY_MS: u64 = 100;

/// Delay between config-visibility retries in the socket server.
pub const STREAM_CHECK_SLEEP_MS: u64 = 25;

/// Threshold used for elapsed timestamp assertions in hook dedupe tests.
pub const MIN_ELAPSED_CHECK_MS: u64 = 20;

/// Maximum allowed skew for control-request timestamps when no env override is set.
pub const CONTROL_TIMESTAMP_WINDOW_SECS: i64 = 300;

/// Warning rate limit for a full daemon log-event queue.
pub const LOG_WARNING_RATE_LIMIT_SECS: u64 = 5;
