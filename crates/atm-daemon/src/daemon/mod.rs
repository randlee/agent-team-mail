pub mod dedup;
pub mod event_loop;
pub mod session_registry;
pub mod shutdown;
pub mod socket;
pub mod spool_task;
pub mod status;
pub mod watcher;

pub use event_loop::run;
pub use session_registry::{
    SessionRecord, SessionRegistry, SessionState, SharedSessionRegistry, is_pid_alive,
    new_session_registry,
};
pub use shutdown::graceful_shutdown;
pub use socket::{
    LaunchRequest, LaunchSender, SharedDedupeStore, SharedPubSubStore, SharedStateStore,
    SocketServerHandle, new_dedup_store, new_launch_sender, new_pubsub_store, new_state_store,
    start_socket_server,
};
pub use spool_task::spool_drain_loop;
pub use status::{DaemonStatus, PluginStatus, PluginStatusKind, StatusWriter};
pub use watcher::{InboxEvent, InboxEventKind, watch_inboxes};
