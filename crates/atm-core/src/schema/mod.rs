//! Schema types for Claude agent team API
//!
//! This module contains all data structures that map to the Claude agent team
//! file-based API. All types preserve unknown fields for forward compatibility.

pub mod agent_member;
mod inbox_message;
mod permissions;
mod settings;
mod task;
mod team_config;
mod version;

pub use agent_member::{AgentMember, BackendType};
pub use inbox_message::InboxMessage;
pub use permissions::Permissions;
pub use settings::SettingsJson;
pub use task::{TaskItem, TaskStatus};
pub use team_config::TeamConfig;
pub use version::SchemaVersion;
