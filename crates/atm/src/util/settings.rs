//! Settings resolution helpers

use anyhow::Result;
use std::path::PathBuf;

/// Get user's home directory
pub fn get_home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))
}
