//! Transport trait for bridge plugin
//!
//! Defines an abstraction for transferring files between machines.
//! Implementations include SSH/SFTP (production) and mock (testing).

use async_trait::async_trait;
use std::path::Path;

/// Result type for transport operations
pub type Result<T> = std::result::Result<T, TransportError>;

/// Transport errors
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// Connection failed
    #[error("Connection failed: {message}")]
    ConnectionFailed { message: String },

    /// IO error during file transfer
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Remote operation failed
    #[error("Remote operation failed: {message}")]
    RemoteError { message: String },

    /// Authentication failed
    #[error("Authentication failed: {message}")]
    AuthenticationFailed { message: String },

    /// Path error (invalid path format)
    #[error("Invalid path: {message}")]
    InvalidPath { message: String },
}

/// Transport abstraction for file transfer operations
///
/// Implementations must be thread-safe (Send + Sync) to allow
/// concurrent operations from the bridge sync engine.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Establish connection to the remote host
    ///
    /// # Errors
    ///
    /// Returns error if connection fails or authentication fails
    async fn connect(&mut self) -> Result<()>;

    /// Check if the connection is alive
    ///
    /// Returns `true` if connected, `false` otherwise.
    /// Does not attempt to reconnect.
    async fn is_connected(&self) -> bool;

    /// Upload a file to the remote path
    ///
    /// The implementation should ensure atomicity (e.g., write to temp file,
    /// then rename to final path).
    ///
    /// # Errors
    ///
    /// Returns error if upload fails or remote path is invalid
    async fn upload(&self, local_path: &Path, remote_path: &Path) -> Result<()>;

    /// Download a file from the remote path
    ///
    /// # Errors
    ///
    /// Returns error if download fails or file does not exist
    async fn download(&self, remote_path: &Path, local_path: &Path) -> Result<()>;

    /// List files in a remote directory matching a pattern
    ///
    /// Returns a list of filenames (not full paths) matching the pattern.
    /// Pattern matching is implementation-specific (glob-style recommended).
    ///
    /// # Errors
    ///
    /// Returns error if remote directory does not exist or is inaccessible
    async fn list(&self, remote_dir: &Path, pattern: &str) -> Result<Vec<String>>;

    /// Rename/move a file on the remote
    ///
    /// Used for atomic writes: upload to temp file, then rename to final path.
    ///
    /// # Errors
    ///
    /// Returns error if rename fails or source file does not exist
    async fn rename(&self, from: &Path, to: &Path) -> Result<()>;

    /// Disconnect from the remote host
    ///
    /// Releases any held resources (connections, file handles).
    /// After disconnect, `is_connected()` must return `false`.
    ///
    /// # Errors
    ///
    /// Returns error if disconnect operation fails (non-fatal)
    async fn disconnect(&mut self) -> Result<()>;
}
