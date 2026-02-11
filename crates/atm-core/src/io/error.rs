//! Error types for atomic I/O operations

use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur during inbox operations
#[derive(Error, Debug)]
pub enum InboxError {
    /// Failed to acquire file lock after multiple retries
    #[error("Failed to acquire lock on {path} after {retries} retries")]
    LockTimeout { path: PathBuf, retries: u32 },

    /// File I/O error
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    /// Failed to parse JSON
    #[error("JSON parse error in {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },

    /// Atomic swap operation not supported on this platform
    #[error("Atomic swap not supported on this platform")]
    AtomicSwapUnsupported,

    /// Invalid inbox path (e.g., missing team or agent name)
    #[error("Invalid inbox path: {path}")]
    InvalidPath { path: PathBuf },

    /// Hash mismatch detected but merge failed
    #[error("Conflict detected but merge failed: {message}")]
    MergeFailed { message: String },

    /// Spool directory error
    #[error("Spool directory error: {message}")]
    SpoolError { message: String },
}
