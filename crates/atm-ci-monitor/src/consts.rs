//! Shared CI-monitor budgeting constants.

/// Maximum shared-poller GitHub calls allowed per active monitor before the cycle backs off.
pub const GH_MONITOR_PER_ACTIVE_MONITOR_MAX_CALLS: u64 = 6;

/// Global GitHub quota floor below which shared polling must pause.
pub const GH_MONITOR_HEADROOM_FLOOR: u64 = 200;

/// GitHub quota floor required before a headroom-paused shared poller may resume.
pub const GH_MONITOR_HEADROOM_RECOVERY_FLOOR: u64 = 300;
