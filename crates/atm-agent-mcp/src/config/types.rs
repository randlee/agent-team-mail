//! Configuration types for atm-agent-mcp.
//!
//! [`AgentMcpConfig`] is deserialized from the `[plugins.atm-agent-mcp]` section
//! of `.atm.toml`. [`RolePreset`] holds per-role model/sandbox/approval overrides.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Default value functions for new mail-injection config fields (FR-8)
// ---------------------------------------------------------------------------

fn default_max_mail_messages() -> usize {
    10
}

fn default_max_mail_message_length() -> usize {
    4096
}

/// Per-role model/sandbox/approval_policy overrides.
///
/// Role presets are defined under `[plugins.atm-agent-mcp.roles.<name>]` in `.atm.toml`
/// and selected at runtime via `--role <name>`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RolePreset {
    /// Model to use for this role
    pub model: Option<String>,
    /// Sandbox mode for this role
    pub sandbox: Option<String>,
    /// Approval policy for this role
    pub approval_policy: Option<String>,
    /// Reasoning effort level (e.g., "low", "medium", "high")
    pub reasoning_effort: Option<String>,
}

/// Resolved atm-agent-mcp plugin configuration.
///
/// Deserialized from `[plugins.atm-agent-mcp]` section of `.atm.toml`.
/// All fields have sensible defaults so a minimal or absent config section
/// produces a fully functional configuration.
///
/// # Example `.atm.toml` section
///
/// ```toml
/// [plugins.atm-agent-mcp]
/// codex_bin = "/usr/local/bin/codex"
/// identity = "arch-ctm"
/// sandbox = "workspace-write"
/// approval_policy = "on-failure"
///
/// [plugins.atm-agent-mcp.roles.architect]
/// model = "o3"
/// reasoning_effort = "high"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMcpConfig {
    /// Path to codex binary (default: `"codex"` from `PATH`)
    #[serde(default = "default_codex_bin")]
    pub codex_bin: String,

    /// Default agent identity
    #[serde(default)]
    pub identity: Option<String>,

    /// Model override (None = let Codex use its default)
    #[serde(default)]
    pub model: Option<String>,

    /// Fast model for `--fast` flag
    #[serde(default)]
    pub fast_model: Option<String>,

    /// Reasoning effort level
    #[serde(default)]
    pub reasoning_effort: Option<String>,

    /// Sandbox mode (default: `"workspace-write"`)
    #[serde(default = "default_sandbox")]
    pub sandbox: String,

    /// Approval policy (default: `"on-failure"`)
    #[serde(default = "default_approval_policy")]
    pub approval_policy: String,

    /// Mail poll interval in milliseconds (default: `5000`)
    #[serde(default = "default_mail_poll_interval_ms")]
    pub mail_poll_interval_ms: u64,

    /// Request timeout in seconds (default: `300`)
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,

    /// Maximum concurrent agent threads (default: `10`)
    #[serde(default = "default_max_concurrent_threads")]
    pub max_concurrent_threads: usize,

    /// Persist thread IDs to disk across restarts (default: `true`)
    #[serde(default = "default_persist_threads")]
    pub persist_threads: bool,

    /// Enable automatic mail injection into Codex context (default: `true`)
    #[serde(default = "default_auto_mail")]
    pub auto_mail: bool,

    /// Maximum number of messages to inject per auto-mail turn (FR-8.5, default: `10`).
    #[serde(default = "default_max_mail_messages")]
    pub max_mail_messages: usize,

    /// Maximum message body length in characters before truncation (FR-8.5, default: `4096`).
    #[serde(default = "default_max_mail_message_length")]
    pub max_mail_message_length: usize,

    /// Per-thread auto-mail overrides.
    ///
    /// Map of `agent_id` → `bool` enabling or disabling auto-mail injection for
    /// a specific thread (FR-8.8).  When absent, the global [`Self::auto_mail`]
    /// setting applies.
    #[serde(default)]
    pub per_thread_auto_mail: HashMap<String, bool>,

    /// Optional base prompt file path
    #[serde(default)]
    pub base_prompt_file: Option<String>,

    /// Optional extra instructions file path
    #[serde(default)]
    pub extra_instructions_file: Option<String>,

    /// Named role presets indexed by role name
    #[serde(default)]
    pub roles: HashMap<String, RolePreset>,
}

fn default_codex_bin() -> String {
    "codex".to_string()
}

fn default_sandbox() -> String {
    "workspace-write".to_string()
}

fn default_approval_policy() -> String {
    "on-failure".to_string()
}

fn default_mail_poll_interval_ms() -> u64 {
    5000
}

fn default_request_timeout_secs() -> u64 {
    300
}

fn default_max_concurrent_threads() -> usize {
    10
}

fn default_persist_threads() -> bool {
    true
}

fn default_auto_mail() -> bool {
    true
}

impl Default for AgentMcpConfig {
    fn default() -> Self {
        Self {
            codex_bin: default_codex_bin(),
            identity: None,
            model: None,
            fast_model: None,
            reasoning_effort: None,
            sandbox: default_sandbox(),
            approval_policy: default_approval_policy(),
            mail_poll_interval_ms: default_mail_poll_interval_ms(),
            request_timeout_secs: default_request_timeout_secs(),
            max_concurrent_threads: default_max_concurrent_threads(),
            persist_threads: default_persist_threads(),
            auto_mail: default_auto_mail(),
            max_mail_messages: default_max_mail_messages(),
            max_mail_message_length: default_max_mail_message_length(),
            per_thread_auto_mail: HashMap::new(),
            base_prompt_file: None,
            extra_instructions_file: None,
            roles: HashMap::new(),
        }
    }
}
