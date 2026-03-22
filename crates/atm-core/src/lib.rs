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
pub mod consts;
pub mod context;
pub mod control;
pub mod daemon_client;
pub mod daemon_stream;
pub mod event_log;
pub mod gh_command;
pub mod home;
pub mod io;
pub mod log_reader;
pub mod logging;
pub mod logging_event;
pub mod model_registry;
pub mod observability;
pub mod pid;
pub mod retention;
pub mod schema;
pub mod spawn;
pub mod team_config_store;
pub mod text;
pub mod util;

pub use schema::{
    AgentMember, InboxMessage, Permissions, SettingsJson, TaskItem, TaskStatus, TeamConfig,
};

// Re-export toml for plugin config access
pub use toml;
