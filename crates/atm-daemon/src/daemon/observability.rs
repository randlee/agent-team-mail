use agent_team_mail_core::logging_event::LogEventV1;
use std::path::Path;

pub const LOG_EVENT_QUEUE_CAPACITY: usize = 4096;
pub const SOCKET_ERROR_VERSION_MISMATCH: &str = "VERSION_MISMATCH";
pub const SOCKET_ERROR_INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
pub const SOCKET_ERROR_INTERNAL_ERROR: &str = "INTERNAL_ERROR";

pub fn export_otel_best_effort(log_path: &Path, event: &LogEventV1) {
    sc_observability::export_otel_best_effort_from_path(log_path, event);
}
