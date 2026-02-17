//! Canonical home directory resolution for ATM
//!
//! Provides a single source of truth for home directory resolution across all ATM crates.
//! This module ensures consistent behavior on all platforms (Linux, macOS, Windows) and
//! supports custom deployments and testing via the `ATM_HOME` environment variable.
//!
//! # Platform Behavior
//!
//! - **Linux/macOS**: `dirs::home_dir()` uses `$HOME` environment variable
//! - **Windows**: `dirs::home_dir()` uses Windows API (`SHGetKnownFolderPath`), which ignores
//!   both `HOME` and `USERPROFILE` environment variables
//!
//! # Precedence
//!
//! 1. `ATM_HOME` environment variable (if set and non-empty)
//! 2. `dirs::home_dir()` platform default
//!
//! # Usage
//!
//! ```
//! use agent_team_mail_core::home::get_home_dir;
//! use std::path::PathBuf;
//!
//! # fn example() -> anyhow::Result<()> {
//! let home = get_home_dir()?;
//! let config_dir = home.join(".config/atm");
//! # Ok(())
//! # }
//! # example().unwrap();
//! ```
//!
//! # Testing
//!
//! Integration tests MUST use `ATM_HOME` to override the home directory:
//!
//! ```ignore
//! use assert_cmd::Command;
//! use tempfile::TempDir;
//!
//! let temp_dir = TempDir::new().unwrap();
//! let mut cmd = Command::cargo_bin("atm").unwrap();
//! cmd.env("ATM_HOME", temp_dir.path());
//! ```

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Get the home directory for ATM operations
///
/// This is the canonical home directory resolution function used by all ATM crates.
///
/// # Precedence
///
/// 1. `ATM_HOME` environment variable (if set and non-empty)
/// 2. `dirs::home_dir()` platform default
///
/// # Returns
///
/// Returns the home directory as a `PathBuf`. Trailing slashes are normalized.
///
/// # Errors
///
/// Returns an error if:
/// - `ATM_HOME` is not set AND
/// - Platform home directory cannot be determined via `dirs::home_dir()`
///
/// # Examples
///
/// ```
/// use agent_team_mail_core::home::get_home_dir;
///
/// # fn example() -> anyhow::Result<()> {
/// // Use platform default
/// let home = get_home_dir()?;
/// println!("Home: {}", home.display());
///
/// // Override with ATM_HOME (requires unsafe)
/// unsafe { std::env::set_var("ATM_HOME", "/custom/home") };
/// let custom_home = get_home_dir()?;
/// assert_eq!(custom_home.to_str().unwrap(), "/custom/home");
/// unsafe { std::env::remove_var("ATM_HOME") };
/// # Ok(())
/// # }
/// # example().unwrap();
/// ```
pub fn get_home_dir() -> Result<PathBuf> {
    // Check ATM_HOME first (useful for testing and custom deployments)
    if let Ok(home) = std::env::var("ATM_HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            // Normalize trailing slashes
            let path = PathBuf::from(trimmed);
            return Ok(path);
        }
    }

    // Fall back to platform default
    dirs::home_dir().context("Could not determine home directory")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    #[test]
    #[serial]
    fn test_atm_home_set() {
        // Save and set
        let original = env::var("ATM_HOME").ok();
        unsafe { env::set_var("ATM_HOME", "/custom/home") };

        let home = get_home_dir().unwrap();
        assert_eq!(home, PathBuf::from("/custom/home"));

        // Restore
        unsafe {
            match original {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_atm_home_not_set_uses_platform_default() {
        // Save and remove
        let original = env::var("ATM_HOME").ok();
        unsafe { env::remove_var("ATM_HOME") };

        let home = get_home_dir().unwrap();
        // Should match platform default
        assert_eq!(home, dirs::home_dir().unwrap());

        // Restore
        unsafe {
            if let Some(v) = original {
                env::set_var("ATM_HOME", v);
            }
        }
    }

    #[test]
    #[serial]
    fn test_atm_home_empty_string_uses_platform_default() {
        let original = env::var("ATM_HOME").ok();
        unsafe { env::set_var("ATM_HOME", "") };

        let home = get_home_dir().unwrap();
        // Empty string should fall back to platform default
        assert_eq!(home, dirs::home_dir().unwrap());

        // Restore
        unsafe {
            match original {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_atm_home_whitespace_only_uses_platform_default() {
        let original = env::var("ATM_HOME").ok();
        unsafe { env::set_var("ATM_HOME", "   ") };

        let home = get_home_dir().unwrap();
        // Whitespace-only should fall back to platform default
        assert_eq!(home, dirs::home_dir().unwrap());

        // Restore
        unsafe {
            match original {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_atm_home_with_trailing_slash() {
        let original = env::var("ATM_HOME").ok();
        unsafe { env::set_var("ATM_HOME", "/custom/home/") };

        let home = get_home_dir().unwrap();
        // PathBuf normalizes trailing slashes automatically
        let expected = PathBuf::from("/custom/home/");
        assert_eq!(home, expected);

        // Restore
        unsafe {
            match original {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_atm_home_with_spaces_in_path() {
        let original = env::var("ATM_HOME").ok();
        unsafe { env::set_var("ATM_HOME", "/path with spaces/home") };

        let home = get_home_dir().unwrap();
        assert_eq!(home, PathBuf::from("/path with spaces/home"));

        // Restore
        unsafe {
            match original {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_atm_home_relative_path() {
        let original = env::var("ATM_HOME").ok();
        unsafe { env::set_var("ATM_HOME", "relative/path") };

        let home = get_home_dir().unwrap();
        assert_eq!(home, PathBuf::from("relative/path"));

        // Restore
        unsafe {
            match original {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_atm_home_with_leading_trailing_whitespace() {
        let original = env::var("ATM_HOME").ok();
        unsafe { env::set_var("ATM_HOME", "  /custom/home  ") };

        let home = get_home_dir().unwrap();
        // Should trim whitespace
        assert_eq!(home, PathBuf::from("/custom/home"));

        // Restore
        unsafe {
            match original {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_multiple_calls_consistent() {
        let original = env::var("ATM_HOME").ok();
        unsafe { env::set_var("ATM_HOME", "/test/home") };

        let home1 = get_home_dir().unwrap();
        let home2 = get_home_dir().unwrap();
        assert_eq!(home1, home2);

        // Restore
        unsafe {
            match original {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }
}
