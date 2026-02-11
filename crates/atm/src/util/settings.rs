//! Settings resolution helpers

use anyhow::Result;
use std::path::PathBuf;

/// Get user's home directory.
///
/// Checks `ATM_HOME` env var first (useful for testing and custom deployments),
/// then falls back to `dirs::home_dir()`.
pub fn get_home_dir() -> Result<PathBuf> {
    if let Ok(home) = std::env::var("ATM_HOME") {
        return Ok(PathBuf::from(home));
    }
    dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))
}
