//! Shared logging initialization for ATM binaries.

use std::sync::OnceLock;

static INIT: OnceLock<()> = OnceLock::new();

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
pub fn init() {
    if INIT.get().is_some() {
        return;
    }
    let level = parse_level();
    let _ = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .try_init();
    let _ = INIT.set(());
}

