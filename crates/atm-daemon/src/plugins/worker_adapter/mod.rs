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
//! - `mock_backend.rs` — Mock backend for testing without tmux/Codex
//! - `agent_state.rs` — Turn-level agent state machine (Launching/Busy/Idle/Killed)
//! - `hook_watcher.rs` — Incremental events.jsonl watcher for Codex hook events
//! - `nudge.rs` — NudgeEngine: auto-nudge idle agents with unread messages
//! - `pubsub.rs` — Ephemeral in-memory pub/sub for agent state change notifications

pub mod activity;
pub mod agent_state;
pub mod capture;
pub mod codex_tmux;
pub mod config;
pub mod hook_watcher;
pub mod lifecycle;
pub mod mock_backend;
pub mod nudge;
pub mod plugin;
pub mod pubsub;
pub mod router;
pub mod trait_def;

pub use activity::ActivityTracker;
pub use agent_state::{AgentPaneInfo, AgentState, AgentStateTracker};
pub use capture::{CaptureConfig, CapturedResponse, LogTailer};
pub use codex_tmux::CodexTmuxBackend;
pub use config::{AgentConfig, NudgeConfig, WorkersConfig, DEFAULT_COMMAND, DEFAULT_NUDGE_TEXT};
pub use hook_watcher::HookWatcher;
pub use lifecycle::{LifecycleManager, WorkerState};
pub use mock_backend::{MockCall, MockTmuxBackend};
pub use nudge::{InboxEntry, NudgeDecision, NudgeEngine};
pub use plugin::WorkerAdapterPlugin;
pub use pubsub::{PubSub, PubSubError, Subscription};
pub use router::{ConcurrencyPolicy, MessageRouter};
pub use trait_def::{WorkerAdapter, WorkerHandle};
