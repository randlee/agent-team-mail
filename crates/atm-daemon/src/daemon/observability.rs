use agent_team_mail_core::logging_event::LogEventV1;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

pub const LOG_EVENT_QUEUE_CAPACITY: usize = 4096;
pub const SOCKET_ERROR_VERSION_MISMATCH: &str = "VERSION_MISMATCH";
pub const SOCKET_ERROR_INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
pub const SOCKET_ERROR_INTERNAL_ERROR: &str = "INTERNAL_ERROR";

pub type OtelHealthSnapshot = sc_observability::OtelHealthSnapshot;
pub type OtelLastError = sc_observability::OtelLastError;

pub type OtelExportHook = Arc<dyn Fn(&Path, &LogEventV1) + Send + Sync>;
pub type OtelHealthHook = Arc<dyn Fn(&Path) -> OtelHealthSnapshot + Send + Sync>;

pub fn install_otel_export_hook(hook: OtelExportHook) {
    *otel_export_hook_slot()
        .lock()
        .expect("atm-daemon otel export hook lock poisoned") = Some(hook);
}

pub fn clear_otel_export_hook() {
    *otel_export_hook_slot()
        .lock()
        .expect("atm-daemon otel export hook lock poisoned") = None;
}

pub fn install_otel_health_hook(hook: OtelHealthHook) {
    *otel_health_hook_slot()
        .lock()
        .expect("atm-daemon otel health hook lock poisoned") = Some(hook);
}

pub fn clear_otel_health_hook() {
    *otel_health_hook_slot()
        .lock()
        .expect("atm-daemon otel health hook lock poisoned") = None;
}

fn otel_export_hook_slot() -> &'static Mutex<Option<OtelExportHook>> {
    static OTEL_EXPORT_HOOK: OnceLock<Mutex<Option<OtelExportHook>>> = OnceLock::new();
    OTEL_EXPORT_HOOK.get_or_init(|| Mutex::new(None))
}

fn otel_health_hook_slot() -> &'static Mutex<Option<OtelHealthHook>> {
    static OTEL_HEALTH_HOOK: OnceLock<Mutex<Option<OtelHealthHook>>> = OnceLock::new();
    OTEL_HEALTH_HOOK.get_or_init(|| Mutex::new(None))
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

pub fn current_otel_health(log_path: &Path) -> OtelHealthSnapshot {
    let hook = otel_health_hook_slot()
        .lock()
        .expect("atm-daemon otel health hook lock poisoned")
        .clone();
    hook.map(|hook| hook(log_path)).unwrap_or_default()
}
