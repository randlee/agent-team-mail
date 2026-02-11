//! Atomic file I/O operations for agent team inboxes
//!
//! This module provides safe, conflict-aware file operations for the `~/.claude/teams/`
//! file structure. Key features:
//!
//! - **Atomic swap**: Platform-specific atomic file exchange (macOS/Linux)
//! - **File locking**: Advisory locks with exponential backoff retry
//! - **Conflict detection**: BLAKE3 hashing to detect concurrent writes
//! - **Guaranteed delivery**: Spooling for messages that can't be delivered immediately
//! - **Round-trip preservation**: Unknown JSON fields preserved on read-modify-write
//!
//! # Example
//!
//! ```rust,no_run
//! use atm_core::io::{inbox_append, WriteOutcome};
//! use atm_core::InboxMessage;
//! use std::path::Path;
//! use std::collections::HashMap;
//!
//! let inbox_path = Path::new("/home/user/.claude/teams/my-team/inboxes/agent.json");
//! let message = InboxMessage {
//!     from: "team-lead".to_string(),
//!     text: "CI failure detected".to_string(),
//!     timestamp: "2026-02-11T14:30:00Z".to_string(),
//!     read: false,
//!     summary: Some("CI failure detected".to_string()),
//!     message_id: Some("msg-12345".to_string()),
//!     unknown_fields: HashMap::new(),
//! };
//!
//! match inbox_append(inbox_path, &message).unwrap() {
//!     WriteOutcome::Success => println!("Message delivered"),
//!     WriteOutcome::ConflictResolved { merged_messages } => {
//!         println!("Conflict resolved, merged {} messages", merged_messages)
//!     }
//!     WriteOutcome::Queued { spool_path } => {
//!         println!("Message queued at {:?}", spool_path)
//!     }
//! }
//! ```

pub mod atomic;
pub mod error;
pub mod hash;
pub mod inbox;
pub mod lock;

// Re-export primary API
pub use error::InboxError;
pub use inbox::{inbox_append, WriteOutcome};
