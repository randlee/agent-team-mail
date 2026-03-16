//! Shared plugin-level constants for `atm-daemon`.

/// Interval between worker-adapter inactivity checks.
pub const INACTIVITY_CHECK_INTERVAL_SECS: u64 = 30;

/// Interval between worker-adapter log rotation checks.
pub const LOG_ROTATION_INTERVAL_SECS: u64 = 300;

/// Interval between worker-adapter idle/nudge scans.
pub const NUDGE_SCAN_INTERVAL_SECS: u64 = 5;
