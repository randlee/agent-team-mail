use serde::{Deserialize, Serialize};

/// Plugin metadata — identity and capabilities
#[derive(Debug, Clone)]
pub struct PluginMetadata {
    pub name: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub capabilities: Vec<Capability>,
}

/// What a plugin can do
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    // --- Behavioral capabilities (routing/dispatch) ---
    /// Plugin can add synthetic members to teams via RosterService
    AdvertiseMembers,
    /// Plugin can inject inbound messages into agent inboxes
    InjectMessages,
    /// Plugin reacts to events (new message, team change, file watch)
    EventListener,

    // --- Domain capabilities (metadata/categorization) ---
    /// Plugin monitors CI pipelines
    CiMonitor,
    /// Custom capability
    Custom(String),
}

/// Plugin lifecycle state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginState {
    Created,
    Initialized,
    Running,
    Stopped,
    Failed,
}

/// Plugin errors with structured variants
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin init failed: {message}")]
    Init {
        message: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("plugin runtime error: {message}")]
    Runtime {
        message: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("plugin shutdown failed: {message}")]
    Shutdown {
        message: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("plugin config error: {message}")]
    Config { message: String },

    #[error("provider error: {message}")]
    Provider {
        message: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
}
