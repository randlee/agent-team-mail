//! Worker Adapter Plugin — async agent teammates in tmux panes
//!
//! This plugin enables daemon-managed agent workers that can:
//! - Receive messages via inbox events
//! - Run in isolated tmux panes
//! - Respond asynchronously without blocking the user's terminal
//!
//! ## Components
//!
//! - `trait_def.rs` — WorkerAdapter trait and WorkerHandle
//! - `codex_tmux.rs` — Codex backend implementation
//! - `config.rs` — Configuration parsing from [workers] section
//! - `plugin.rs` — Plugin implementation

pub mod codex_tmux;
pub mod config;
pub mod plugin;
pub mod trait_def;

pub use codex_tmux::CodexTmuxBackend;
pub use config::WorkersConfig;
pub use plugin::WorkerAdapterPlugin;
pub use trait_def::{WorkerAdapter, WorkerHandle};
