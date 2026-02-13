pub mod context;
pub mod mail_service;
pub mod registry;
pub mod traits;
pub mod types;

pub use context::PluginContext;
pub use mail_service::MailService;
pub use registry::{PluginRegistry, SharedPlugin};
pub use traits::{ErasedPlugin, Plugin};
pub use types::{Capability, PluginError, PluginMetadata, PluginState};
