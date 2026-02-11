//! Core types and schemas for agent-team-mail (atm)
//!
//! This crate provides the fundamental data structures for interacting with
//! Claude agent teams via the file-based API at `~/.claude/teams/`.
//!
//! All schema types are designed to:
//! - Preserve unknown fields for forward compatibility
//! - Use proper serde configuration for camelCase ↔ snake_case
//! - Support round-trip serialization without data loss

pub mod config;
pub mod context;
pub mod io;
pub mod schema;

pub use schema::{
    AgentMember, InboxMessage, Permissions, SettingsJson, TaskItem, TaskStatus, TeamConfig,
};
