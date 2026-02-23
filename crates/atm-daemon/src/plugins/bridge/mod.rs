//! Bridge plugin — cross-computer queue synchronization

mod config;
mod dedup;
mod metrics;
mod mock_transport;
mod plugin;
mod self_write_filter;
mod sync;
mod team_config_sync;
mod transport;

#[cfg(feature = "ssh")]
mod ssh;

pub use config::BridgePluginConfig;
pub use dedup::{SyncState, assign_message_ids};
pub use metrics::BridgeMetrics;
pub use mock_transport::{MockTransport, SharedFilesystem, SharedMockTransport};
pub use plugin::BridgePlugin;
pub use self_write_filter::SelfWriteFilter;
pub use sync::{SyncEngine, SyncStats};
pub use team_config_sync::{cleanup_stale_tmp_files, sync_team_config};
pub use transport::{Transport, TransportError};

#[cfg(feature = "ssh")]
pub use ssh::{SshConfig, SshTransport};
