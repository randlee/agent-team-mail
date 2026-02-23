pub mod dedup;
pub mod event_loop;
pub mod log_writer;
pub mod session_registry;
pub mod shutdown;
pub mod socket;
pub mod spool_merge;
pub mod spool_task;
pub mod status;
pub mod watcher;

pub use event_loop::run;
pub use log_writer::{
    BoundedQueue, LogEventQueue, LogWriterConfig, new_log_event_queue, run_log_writer_task,
};
pub use session_registry::{
    SessionRecord, SessionRegistry, SessionState, SharedSessionRegistry, is_pid_alive,
    new_session_registry,
};
pub use shutdown::graceful_shutdown;
pub use socket::{
    LaunchRequest, LaunchSender, SharedDedupeStore, SharedPubSubStore, SharedStateStore,
    SharedStreamEventSender, SharedStreamStateStore, SocketServerHandle, new_dedup_store,
    new_launch_sender, new_pubsub_store, new_state_store, new_stream_event_sender,
    new_stream_state_store, start_socket_server,
};
pub use spool_task::spool_drain_loop;
pub use status::{DaemonStatus, PluginStatus, PluginStatusKind, StatusWriter};
pub use watcher::{InboxEvent, InboxEventKind, watch_inboxes};
