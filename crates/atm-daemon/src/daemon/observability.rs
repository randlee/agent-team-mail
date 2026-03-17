use agent_team_mail_core::logging_event::LogEventV1;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

pub const LOG_EVENT_QUEUE_CAPACITY: usize = 4096;
pub const SOCKET_ERROR_VERSION_MISMATCH: &str = "VERSION_MISMATCH";
pub const SOCKET_ERROR_INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
pub const SOCKET_ERROR_INTERNAL_ERROR: &str = "INTERNAL_ERROR";

pub type OtelExportHook = Arc<dyn Fn(&Path, &LogEventV1) + Send + Sync>;

pub fn install_otel_export_hook(hook: OtelExportHook) {
    *otel_export_hook_slot()
        .lock()
        .expect("atm-daemon otel export hook lock poisoned") = Some(hook);
}

#[cfg(test)]
pub fn clear_otel_export_hook() {
    *otel_export_hook_slot()
        .lock()
        .expect("atm-daemon otel export hook lock poisoned") = None;
}

fn otel_export_hook_slot() -> &'static Mutex<Option<OtelExportHook>> {
    static OTEL_EXPORT_HOOK: OnceLock<Mutex<Option<OtelExportHook>>> = OnceLock::new();
    OTEL_EXPORT_HOOK.get_or_init(|| Mutex::new(None))
}

pub fn export_otel_best_effort(log_path: &Path, event: &LogEventV1) {
    let hook = otel_export_hook_slot()
        .lock()
        .expect("atm-daemon otel export hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        hook(log_path, event);
    }
}
