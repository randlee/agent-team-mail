/// Windows compile-check for daemon autostart.
///
/// The full autostart integration tests live in `integration_daemon_autostart.rs`
/// and are gated with `#![cfg(unix)]` because they rely on a Python fake daemon
/// and Unix domain sockets. This file confirms that `ensure_daemon_running` is
/// callable on Windows (compile-time check only).
#[test]
#[cfg(windows)]
fn test_autostart_compiles_on_windows() {
    // Confirms ensure_daemon_running is callable on Windows.
    // Actual daemon spawn is not tested (requires Unix socket + Python).
    let _ = agent_team_mail_core::daemon_client::ensure_daemon_running;
}
