//! Canonical runtime-root and config-root resolution for ATM.
//!
//! ATM now distinguishes between:
//! - a runtime root, controlled by `ATM_HOME`, for daemon sockets, logs, and
//!   other ephemeral runtime state
//! - a canonical config root under the OS home directory, for team state under
//!   `~/.claude` and global ATM config under `~/.config/atm`
//!
//! This split keeps test/dev runtime overrides from redirecting the canonical
//! team config tree away from the operator's real `~/.claude/teams`.
//!
//! # Platform Behavior
//!
//! - **Linux/macOS**: `get_os_home_dir()` honors `ATM_CONFIG_HOME`, then `$HOME`
//! - **Windows**: `get_os_home_dir()` honors `ATM_CONFIG_HOME`, then falls back to the
//!   Windows API (`SHGetKnownFolderPath`) via `dirs::home_dir()`, which ignores both
//!   `HOME` and `USERPROFILE`
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
//!
//! # fn example() -> anyhow::Result<()> {
//! let runtime_root = get_home_dir()?;
//! let daemon_socket = runtime_root.join(".config/atm/atm-daemon.sock");
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

/// Get the runtime home directory for ATM operations.
///
/// This is the canonical runtime-root resolution function used by ATM crates
/// for daemon sockets, logs, and other runtime-scoped state.
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
    // Check ATM_HOME first (useful for testing and runtime isolation)
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

/// Get the canonical config-root home directory, bypassing `ATM_HOME`.
///
/// Unlike [`get_home_dir`], this function ignores the `ATM_HOME` runtime-root
/// override. Tests may override the canonical config root with
/// `ATM_CONFIG_HOME`; otherwise this falls back to the OS-level home directory.
///
/// This is intentionally distinct from [`get_home_dir`] and is reserved for
/// locations that must remain config-root relative even when `ATM_HOME` points
/// at a separate runtime directory.
///
/// # Errors
///
/// Returns an error if the platform cannot determine a home directory.
pub fn get_os_home_dir() -> Result<PathBuf> {
    if let Ok(home) = std::env::var("ATM_CONFIG_HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    #[cfg(not(windows))]
    if let Ok(home) = std::env::var("HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    dirs::home_dir().context("Could not determine OS home directory")
}

/// Return the canonical `~/.claude` config root, always bypassing `ATM_HOME`.
pub fn claude_root_dir() -> Result<PathBuf> {
    Ok(config_claude_root_dir_for(&get_os_home_dir()?))
}

/// Return the canonical `{runtime_root}/.claude` path for the provided runtime home.
///
/// This helper is intentionally runtime-root relative and is kept for legacy
/// callers that still need an explicit runtime-scoped `.claude` path.
pub fn claude_root_dir_for(home: &Path) -> PathBuf {
    home.join(".claude")
}

/// Return the canonical `~/.claude/teams` team-state root, always bypassing `ATM_HOME`.
pub fn teams_root_dir() -> Result<PathBuf> {
    Ok(config_teams_root_dir_for(&get_os_home_dir()?))
}

/// Return the canonical `{runtime_root}/.claude/teams` path for the provided runtime home.
///
/// This helper is intentionally runtime-root relative and is kept for legacy
/// callers that still need an explicit runtime-scoped teams path.
pub fn teams_root_dir_for(home: &Path) -> PathBuf {
    claude_root_dir_for(home).join("teams")
}

/// Return the canonical `~/.claude` config root for the current OS home.
pub fn config_claude_root_dir() -> Result<PathBuf> {
    Ok(config_claude_root_dir_for(&get_os_home_dir()?))
}

/// Return the canonical `~/.claude` config root for the provided OS home.
pub fn config_claude_root_dir_for(os_home: &Path) -> PathBuf {
    os_home.join(".claude")
}

/// Return the canonical `~/.claude/teams` team-state root for the current OS home.
pub fn config_teams_root_dir() -> Result<PathBuf> {
    Ok(config_teams_root_dir_for(&get_os_home_dir()?))
}

/// Return the canonical `~/.claude/teams` team-state root for the provided OS home.
pub fn config_teams_root_dir_for(os_home: &Path) -> PathBuf {
    config_claude_root_dir_for(os_home).join("teams")
}

/// Return the canonical team directory under `~/.claude/teams`.
pub fn config_team_dir(team: &str) -> Result<PathBuf> {
    Ok(config_team_dir_for(&get_os_home_dir()?, team))
}

/// Return the canonical team directory under `~/.claude/teams` for the provided OS home.
pub fn config_team_dir_for(os_home: &Path, team: &str) -> PathBuf {
    config_teams_root_dir_for(os_home).join(team)
}

/// Return the canonical team config path under `~/.claude/teams`.
pub fn config_team_config_path(team: &str) -> Result<PathBuf> {
    Ok(config_team_config_path_for(&get_os_home_dir()?, team))
}

/// Return the canonical team config path under `~/.claude/teams` for the provided OS home.
pub fn config_team_config_path_for(os_home: &Path, team: &str) -> PathBuf {
    config_team_dir_for(os_home, team).join("config.json")
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

/// Return the canonical `{runtime_root}/.config/atm` directory for the provided runtime home.
pub fn atm_config_dir_for(home: &Path) -> PathBuf {
    home.join(".config").join("atm")
}

/// Return the canonical agent session directory for the provided runtime home.
pub fn sessions_dir_for(home: &Path) -> PathBuf {
    atm_config_dir_for(home).join("agent-sessions")
}
#[cfg(test)]
mod tests {
    use super::*;
    // get_home_dir resolves ATM_HOME from process-wide env, so these tests must
    // remain serialized until the API stops reading global state directly.
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
        let home = PathBuf::from("test-home");
        let os_home = PathBuf::from("os-home");

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
        assert_eq!(
            config_claude_root_dir_for(&os_home),
            os_home.join(".claude")
        );
        assert_eq!(
            config_teams_root_dir_for(&os_home),
            os_home.join(".claude").join("teams")
        );
        assert_eq!(
            config_team_dir_for(&os_home, "atm-dev"),
            os_home.join(".claude").join("teams").join("atm-dev")
        );
        assert_eq!(
            config_team_config_path_for(&os_home, "atm-dev"),
            os_home
                .join(".claude")
                .join("teams")
                .join("atm-dev")
                .join("config.json")
        );
    }

    #[test]
    #[serial]
    fn test_runtime_root_helpers_respect_atm_home() {
        let original = env::var("ATM_HOME").ok();
        unsafe { env::set_var("ATM_HOME", "test-home") };

        assert_eq!(get_home_dir().unwrap(), PathBuf::from("test-home"));

        unsafe {
            match original {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_config_root_helpers_ignore_atm_home() {
        let original = env::var("ATM_HOME").ok();
        let runtime_home = env::temp_dir().join("runtime-home");
        unsafe { env::set_var("ATM_HOME", &runtime_home) };

        let os_home = dirs::home_dir().unwrap();
        assert_eq!(claude_root_dir().unwrap(), os_home.join(".claude"));
        assert_eq!(teams_root_dir().unwrap(), os_home.join(".claude/teams"));
        assert_eq!(config_claude_root_dir().unwrap(), os_home.join(".claude"));
        assert_eq!(
            config_teams_root_dir().unwrap(),
            os_home.join(".claude/teams")
        );

        unsafe {
            match original {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }

    #[cfg(not(windows))]
    #[test]
    #[serial]
    fn test_get_os_home_dir_honors_home_env() {
        let original_config_home = env::var("ATM_CONFIG_HOME").ok();
        let original_home = env::var("HOME").ok();
        let config_home = env::temp_dir().join("config-home");
        unsafe { env::remove_var("ATM_CONFIG_HOME") };
        unsafe { env::set_var("HOME", &config_home) };

        assert_eq!(get_os_home_dir().unwrap(), config_home);

        unsafe {
            match original_config_home {
                Some(v) => env::set_var("ATM_CONFIG_HOME", v),
                None => env::remove_var("ATM_CONFIG_HOME"),
            }
            match original_home {
                Some(v) => env::set_var("HOME", v),
                None => env::remove_var("HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_get_os_home_dir_honors_atm_config_home() {
        let original_config_home = env::var("ATM_CONFIG_HOME").ok();
        let original_home = env::var("HOME").ok();
        let atm_config_home = env::temp_dir().join("atm-config-home");
        unsafe { env::set_var("ATM_CONFIG_HOME", &atm_config_home) };
        #[cfg(not(windows))]
        let ignored_home = env::temp_dir().join("ignored-home");
        #[cfg(not(windows))]
        unsafe {
            env::set_var("HOME", &ignored_home);
        }

        assert_eq!(get_os_home_dir().unwrap(), atm_config_home);

        unsafe {
            match original_config_home {
                Some(v) => env::set_var("ATM_CONFIG_HOME", v),
                None => env::remove_var("ATM_CONFIG_HOME"),
            }
            #[cfg(not(windows))]
            match original_home {
                Some(v) => env::set_var("HOME", v),
                None => env::remove_var("HOME"),
            }
        }
    }
}
