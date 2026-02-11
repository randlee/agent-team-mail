//! Configuration resolution
//!
//! Resolves configuration from multiple sources with priority:
//! 1. Command-line flags (passed as parameters)
//! 2. Environment variables
//! 3. Repo-local config (.atm.toml)
//! 4. Global config (~/.config/atm/config.toml)
//! 5. Defaults

mod discovery;
mod types;

pub use discovery::{resolve_config, resolve_settings, ConfigError, ConfigOverrides};
pub use types::{Config, CoreConfig, DisplayConfig, MessagingConfig, OutputFormat, TimestampFormat};
