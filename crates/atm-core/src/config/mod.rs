//! Configuration resolution
//!
//! Resolves configuration from multiple sources with priority:
//! 1. Command-line flags (passed as parameters)
//! 2. Environment variables
//! 3. Repo-local config (.atm.toml)
//! 4. Global config (~/.config/atm/config.toml)
//! 5. Defaults

mod bridge;
mod discovery;
mod types;

pub use bridge::{BridgeConfig, BridgeRole, HostnameRegistry, RemoteConfig};
pub use discovery::{resolve_config, resolve_settings, ConfigError, ConfigOverrides};
pub use types::{CleanupStrategy, Config, CoreConfig, DisplayConfig, MessagingConfig, OutputFormat, RetentionConfig, TimestampFormat};
