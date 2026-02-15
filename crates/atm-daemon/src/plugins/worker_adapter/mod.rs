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
//! - `router.rs` — Message routing with concurrency control
//! - `capture.rs` — Response capture via log file tailing
//! - `activity.rs` — Agent activity tracking
//! - `lifecycle.rs` — Worker lifecycle management (startup, health, restart, shutdown)

pub mod activity;
pub mod capture;
pub mod codex_tmux;
pub mod config;
pub mod lifecycle;
pub mod plugin;
pub mod router;
pub mod trait_def;

pub use activity::ActivityTracker;
pub use capture::{CaptureConfig, CapturedResponse, LogTailer};
pub use codex_tmux::CodexTmuxBackend;
pub use config::{AgentConfig, WorkersConfig};
pub use lifecycle::{LifecycleManager, WorkerState};
pub use plugin::WorkerAdapterPlugin;
pub use router::{ConcurrencyPolicy, MessageRouter};
pub use trait_def::{WorkerAdapter, WorkerHandle};
