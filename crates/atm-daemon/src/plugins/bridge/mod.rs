//! Bridge plugin — cross-computer queue synchronization

mod config;
mod dedup;
mod plugin;
mod self_write_filter;
mod sync;
mod transport;
mod mock_transport;

#[cfg(feature = "ssh")]
mod ssh;

pub use config::BridgePluginConfig;
pub use dedup::{assign_message_ids, SyncState};
pub use plugin::BridgePlugin;
pub use self_write_filter::SelfWriteFilter;
pub use sync::{SyncEngine, SyncStats};
pub use transport::{Transport, TransportError};
pub use mock_transport::MockTransport;

#[cfg(feature = "ssh")]
pub use ssh::{SshTransport, SshConfig};
