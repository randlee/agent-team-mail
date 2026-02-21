//! Per-user TUI preferences loaded from `{home}/.config/atm/tui.toml`.
//!
//! The config file is optional. If it is absent or cannot be parsed, all
//! fields fall back to [`TuiConfig::default()`] silently. Parse errors are
//! reported to stderr so the user can diagnose formatting mistakes without
//! crashing the TUI.
//!
//! # File location
//!
//! ```text
//! ~/.config/atm/tui.toml
//! ```
//!
//! Override the base directory with `ATM_HOME`:
//!
//! ```text
//! ATM_HOME=/custom/home atm-tui --team atm-dev
//! # loads: /custom/home/.config/atm/tui.toml
//! ```
//!
//! # Example configuration
//!
//! ```toml
//! # ~/.config/atm/tui.toml
//! interrupt_policy = "confirm"   # "always" | "never" | "confirm"
//! follow_mode_default = true     # auto-scroll stream pane on startup
//! stdin_timeout_secs = 10        # total wait budget for stdin control actions
//! interrupt_timeout_secs = 5     # total wait budget for interrupt control actions
//! ```

use serde::Deserialize;

use agent_team_mail_core::home::get_home_dir;

/// TUI runtime preferences.
///
/// Loaded once at startup from `{home}/.config/atm/tui.toml`. All fields
/// have documented defaults that are used when the file is absent or when
/// individual fields are omitted.
#[derive(Debug, Clone, Deserialize)]
pub struct TuiConfig {
    /// Controls how the TUI reacts to a `Ctrl-I` interrupt keystroke.
    ///
    /// - [`InterruptPolicy::Always`] — sends the interrupt immediately.
    /// - [`InterruptPolicy::Never`] — silently discards the keystroke.
    /// - [`InterruptPolicy::Confirm`] — shows a `[y/N]` prompt in the status
    ///   bar and waits for confirmation before dispatching.
    ///
    /// Defaults to [`InterruptPolicy::Confirm`].
    #[serde(default)]
    pub interrupt_policy: InterruptPolicy,

    /// Whether the stream pane should auto-scroll to the latest line on
    /// startup.
    ///
    /// When `true` (default), the stream pane follows new log output as it
    /// arrives. Press `F` to toggle follow mode at runtime.
    #[serde(default = "default_follow_mode")]
    pub follow_mode_default: bool,

    /// Total wait budget in seconds for a stdin control action.
    ///
    /// On a first-attempt timeout the request is retried once after
    /// `stdin_timeout_secs / 2` seconds with the same idempotency key.
    /// Defaults to `10`.
    #[serde(default = "default_stdin_timeout")]
    pub stdin_timeout_secs: u64,

    /// Total wait budget in seconds for an interrupt control action.
    ///
    /// On a first-attempt timeout the request is retried once after
    /// `interrupt_timeout_secs / 2` seconds with the same idempotency key.
    /// Defaults to `5`.
    #[serde(default = "default_interrupt_timeout")]
    pub interrupt_timeout_secs: u64,
}

// ── Serde field defaults ──────────────────────────────────────────────────────

fn default_follow_mode() -> bool {
    true
}

fn default_stdin_timeout() -> u64 {
    10
}

fn default_interrupt_timeout() -> u64 {
    5
}

// ── Default impl ──────────────────────────────────────────────────────────────

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            interrupt_policy: InterruptPolicy::Confirm,
            follow_mode_default: true,
            stdin_timeout_secs: 10,
            interrupt_timeout_secs: 5,
        }
    }
}

// ── InterruptPolicy ───────────────────────────────────────────────────────────

/// Determines how `Ctrl-I` interrupt keystrokes are handled.
///
/// Configured via the `interrupt_policy` field in `tui.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum InterruptPolicy {
    /// Send the interrupt immediately without any confirmation dialog.
    Always,
    /// Silently discard the interrupt keystroke — interrupt is disabled.
    Never,
    /// Show a `[y/N]` confirmation prompt in the status bar before sending.
    #[default]
    Confirm,
}

// ── Config loader ─────────────────────────────────────────────────────────────

/// Load TUI configuration from `{home}/.config/atm/tui.toml`.
///
/// Returns [`TuiConfig::default()`] if:
/// - The file does not exist (expected for first-time users).
/// - The home directory cannot be determined.
/// - The file cannot be read (permissions, I/O error).
/// - The TOML is malformed or contains unrecognised fields.
///
/// Parse errors are printed to stderr so users can diagnose formatting
/// mistakes without crashing the TUI.
pub fn load_tui_config() -> TuiConfig {
    let home = match get_home_dir() {
        Ok(h) => h,
        Err(_) => return TuiConfig::default(),
    };

    let path = home.join(".config/atm/tui.toml");

    if !path.exists() {
        return TuiConfig::default();
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("atm-tui: warning: could not read {}: {e}", path.display());
            return TuiConfig::default();
        }
    };

    match toml::from_str::<TuiConfig>(&content) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!(
                "atm-tui: warning: could not parse {}: {e}. Using defaults.",
                path.display()
            );
            TuiConfig::default()
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // ── Default values ────────────────────────────────────────────────────────

    #[test]
    fn test_default_interrupt_policy_is_confirm() {
        let cfg = TuiConfig::default();
        assert_eq!(cfg.interrupt_policy, InterruptPolicy::Confirm);
    }

    #[test]
    fn test_default_follow_mode_is_true() {
        assert!(TuiConfig::default().follow_mode_default);
    }

    #[test]
    fn test_default_stdin_timeout_is_10() {
        assert_eq!(TuiConfig::default().stdin_timeout_secs, 10);
    }

    #[test]
    fn test_default_interrupt_timeout_is_5() {
        assert_eq!(TuiConfig::default().interrupt_timeout_secs, 5);
    }

    // ── TOML parsing ──────────────────────────────────────────────────────────

    #[test]
    fn test_parse_always_policy() {
        let cfg: TuiConfig = toml::from_str("interrupt_policy = \"always\"").unwrap();
        assert_eq!(cfg.interrupt_policy, InterruptPolicy::Always);
    }

    #[test]
    fn test_parse_never_policy() {
        let cfg: TuiConfig = toml::from_str("interrupt_policy = \"never\"").unwrap();
        assert_eq!(cfg.interrupt_policy, InterruptPolicy::Never);
    }

    #[test]
    fn test_parse_confirm_policy() {
        let cfg: TuiConfig = toml::from_str("interrupt_policy = \"confirm\"").unwrap();
        assert_eq!(cfg.interrupt_policy, InterruptPolicy::Confirm);
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
            interrupt_policy = "always"
            follow_mode_default = false
            stdin_timeout_secs = 20
            interrupt_timeout_secs = 8
        "#;
        let cfg: TuiConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.interrupt_policy, InterruptPolicy::Always);
        assert!(!cfg.follow_mode_default);
        assert_eq!(cfg.stdin_timeout_secs, 20);
        assert_eq!(cfg.interrupt_timeout_secs, 8);
    }

    #[test]
    fn test_parse_empty_toml_uses_defaults() {
        let cfg: TuiConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.interrupt_policy, InterruptPolicy::Confirm);
        assert!(cfg.follow_mode_default);
        assert_eq!(cfg.stdin_timeout_secs, 10);
        assert_eq!(cfg.interrupt_timeout_secs, 5);
    }

    // ── load_tui_config file behaviour ────────────────────────────────────────

    #[test]
    #[serial]
    fn test_load_tui_config_missing_file_returns_defaults() {
        let dir = tempfile::TempDir::new().unwrap();
        unsafe { std::env::set_var("ATM_HOME", dir.path()) };

        let cfg = load_tui_config();
        assert_eq!(cfg.interrupt_policy, InterruptPolicy::Confirm);
        assert!(cfg.follow_mode_default);

        unsafe { std::env::remove_var("ATM_HOME") };
    }

    #[test]
    #[serial]
    fn test_load_tui_config_valid_file_parsed() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path().join(".config/atm");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("tui.toml"),
            "interrupt_policy = \"never\"\nfollow_mode_default = false\n",
        )
        .unwrap();

        unsafe { std::env::set_var("ATM_HOME", dir.path()) };

        let cfg = load_tui_config();
        assert_eq!(cfg.interrupt_policy, InterruptPolicy::Never);
        assert!(!cfg.follow_mode_default);

        unsafe { std::env::remove_var("ATM_HOME") };
    }

    #[test]
    #[serial]
    fn test_load_tui_config_malformed_file_returns_defaults() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path().join(".config/atm");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("tui.toml"), "this is not valid toml!!!").unwrap();

        unsafe { std::env::set_var("ATM_HOME", dir.path()) };

        let cfg = load_tui_config();
        // Malformed file must silently fall back to defaults.
        assert_eq!(cfg.interrupt_policy, InterruptPolicy::Confirm);

        unsafe { std::env::remove_var("ATM_HOME") };
    }
}
