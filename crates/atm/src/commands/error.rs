//! Error types for command execution

use thiserror::Error;

/// Command execution errors
#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum CommandError {
    /// Agent not found in team configuration
    #[error("Agent '{agent}' not found in team '{team}'")]
    AgentNotFound { agent: String, team: String },

    /// Team not found (~/.claude/teams/{team}/ doesn't exist)
    #[error("Team '{team}' not found (directory ~/.claude/teams/{team}/ doesn't exist)")]
    TeamNotFound { team: String },

    /// Invalid addressing format
    #[error("Invalid address format: {0}")]
    InvalidAddress(String),

    /// File not found (for --file)
    #[error("File not found: {0}")]
    FileNotFound(String),

    /// Inbox write failure
    #[error("Failed to write to inbox: {0}")]
    InboxWriteFailed(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
