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
use std::path::{Path, PathBuf};

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

/// Get the OS-level home directory, always bypassing `ATM_HOME`.
///
/// Unlike [`get_home_dir`], this function ignores the `ATM_HOME` environment
/// variable and returns the platform default home directory directly.
///
/// This is intentionally distinct from [`get_home_dir`] and is reserved for
/// locations that must be stable regardless of `ATM_HOME` — specifically the
/// daemon socket pointer file at `~/.config/atm/daemon-socket.path`, which
/// needs to be reachable by any CLI invocation even when `ATM_HOME` points to
/// a different directory.
///
/// # Errors
///
/// Returns an error if the platform cannot determine a home directory.
pub fn get_os_home_dir() -> Result<PathBuf> {
    dirs::home_dir().context("Could not determine OS home directory")
}

/// Return the canonical `{ATM_HOME}/.claude` root for ATM-managed state.
pub fn claude_root_dir() -> Result<PathBuf> {
    Ok(claude_root_dir_for(&get_home_dir()?))
}

/// Return the canonical `{ATM_HOME}/.claude` root for the provided home path.
pub fn claude_root_dir_for(home: &Path) -> PathBuf {
    home.join(".claude")
}

/// Return the canonical `{ATM_HOME}/.claude/teams` root for ATM team state.
pub fn teams_root_dir() -> Result<PathBuf> {
    Ok(teams_root_dir_for(&get_home_dir()?))
}

/// Return the canonical `{ATM_HOME}/.claude/teams` root for the provided home path.
pub fn teams_root_dir_for(home: &Path) -> PathBuf {
    claude_root_dir_for(home).join("teams")
}

/// Return the canonical team directory for the provided home and team name.
pub fn team_dir_for(home: &Path, team: &str) -> PathBuf {
    teams_root_dir_for(home).join(team)
}

/// Return the canonical team config path for the provided home and team name.
pub fn team_config_path_for(home: &Path, team: &str) -> PathBuf {
    team_dir_for(home, team).join("config.json")
}

/// Return the canonical inbox JSON path for the provided home, team, and agent.
pub fn inbox_path_for(home: &Path, team: &str, agent: &str) -> PathBuf {
    team_dir_for(home, team)
        .join("inboxes")
        .join(format!("{agent}.json"))
}

/// Return the canonical Claude settings path for the provided home.
pub fn claude_settings_path_for(home: &Path) -> PathBuf {
    claude_root_dir_for(home).join("settings.json")
}

/// Return the canonical Claude scripts directory for the provided home.
pub fn claude_scripts_dir_for(home: &Path) -> PathBuf {
    claude_root_dir_for(home).join("scripts")
}

/// Return the canonical Claude agents directory for the provided home.
pub fn claude_agents_dir_for(home: &Path) -> PathBuf {
    claude_root_dir_for(home).join("agents")
}

/// Return the canonical `{ATM_HOME}/.config/atm` directory for the provided home.
pub fn atm_config_dir_for(home: &Path) -> PathBuf {
    home.join(".config").join("atm")
}

/// Return the canonical agent session directory for the provided home.
pub fn sessions_dir_for(home: &Path) -> PathBuf {
    atm_config_dir_for(home).join("agent-sessions")
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

    #[test]
    fn test_path_helpers_build_canonical_paths() {
        let home = PathBuf::from("/tmp/atm-home");

        assert_eq!(claude_root_dir_for(&home), home.join(".claude"));
        assert_eq!(
            teams_root_dir_for(&home),
            home.join(".claude").join("teams")
        );
        assert_eq!(
            team_dir_for(&home, "atm-dev"),
            home.join(".claude").join("teams").join("atm-dev")
        );
        assert_eq!(
            team_config_path_for(&home, "atm-dev"),
            home.join(".claude")
                .join("teams")
                .join("atm-dev")
                .join("config.json")
        );
        assert_eq!(
            inbox_path_for(&home, "atm-dev", "arch-ctm"),
            home.join(".claude")
                .join("teams")
                .join("atm-dev")
                .join("inboxes")
                .join("arch-ctm.json")
        );
        assert_eq!(
            claude_settings_path_for(&home),
            home.join(".claude").join("settings.json")
        );
        assert_eq!(
            claude_scripts_dir_for(&home),
            home.join(".claude").join("scripts")
        );
        assert_eq!(
            claude_agents_dir_for(&home),
            home.join(".claude").join("agents")
        );
        assert_eq!(atm_config_dir_for(&home), home.join(".config").join("atm"));
        assert_eq!(
            sessions_dir_for(&home),
            home.join(".config").join("atm").join("agent-sessions")
        );
    }

    #[test]
    #[serial]
    fn test_root_dir_helpers_respect_atm_home() {
        let original = env::var("ATM_HOME").ok();
        unsafe { env::set_var("ATM_HOME", "/atm-home") };

        assert_eq!(
            claude_root_dir().unwrap(),
            PathBuf::from("/atm-home/.claude")
        );
        assert_eq!(
            teams_root_dir().unwrap(),
            PathBuf::from("/atm-home/.claude/teams")
        );

        unsafe {
            match original {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }
}
