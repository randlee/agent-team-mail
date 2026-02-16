//! Bridge plugin — cross-computer queue synchronization

mod config;
mod plugin;
mod transport;
mod mock_transport;

#[cfg(feature = "ssh")]
mod ssh;

pub use config::BridgePluginConfig;
pub use plugin::BridgePlugin;
pub use transport::{Transport, TransportError};
pub use mock_transport::MockTransport;

#[cfg(feature = "ssh")]
pub use ssh::{SshTransport, SshConfig};
