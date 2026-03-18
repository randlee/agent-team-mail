use agent_team_mail_core::logging_event::LogEventV1;
pub use agent_team_mail_core::observability::{OtelHealthSnapshot, OtelLastError};
use sc_observability_types::{MetricRecord, OtelConfig, TraceRecord};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

pub const LOG_EVENT_QUEUE_CAPACITY: usize = 4096;
pub const SOCKET_ERROR_VERSION_MISMATCH: &str = "VERSION_MISMATCH";
pub const SOCKET_ERROR_INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
pub const SOCKET_ERROR_INTERNAL_ERROR: &str = "INTERNAL_ERROR";

pub type OtelExportHook = Arc<dyn Fn(&Path, &LogEventV1) + Send + Sync>;
pub type OtelHealthHook = Arc<dyn Fn(&Path) -> OtelHealthSnapshot + Send + Sync>;
pub type TraceExportHook = Arc<dyn Fn(&[TraceRecord], &OtelConfig) + Send + Sync>;
pub type MetricExportHook = Arc<dyn Fn(&[MetricRecord], &OtelConfig) + Send + Sync>;

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

pub fn install_trace_export_hook(hook: TraceExportHook) {
    *trace_export_hook_slot()
        .lock()
        .expect("atm-daemon trace export hook lock poisoned") = Some(hook);
}

pub fn clear_trace_export_hook() {
    *trace_export_hook_slot()
        .lock()
        .expect("atm-daemon trace export hook lock poisoned") = None;
}

pub fn install_metric_export_hook(hook: MetricExportHook) {
    *metric_export_hook_slot()
        .lock()
        .expect("atm-daemon metric export hook lock poisoned") = Some(hook);
}

pub fn clear_metric_export_hook() {
    *metric_export_hook_slot()
        .lock()
        .expect("atm-daemon metric export hook lock poisoned") = None;
}

fn otel_export_hook_slot() -> &'static Mutex<Option<OtelExportHook>> {
    static OTEL_EXPORT_HOOK: OnceLock<Mutex<Option<OtelExportHook>>> = OnceLock::new();
    OTEL_EXPORT_HOOK.get_or_init(|| Mutex::new(None))
}

fn otel_health_hook_slot() -> &'static Mutex<Option<OtelHealthHook>> {
    static OTEL_HEALTH_HOOK: OnceLock<Mutex<Option<OtelHealthHook>>> = OnceLock::new();
    OTEL_HEALTH_HOOK.get_or_init(|| Mutex::new(None))
}

fn trace_export_hook_slot() -> &'static Mutex<Option<TraceExportHook>> {
    static TRACE_EXPORT_HOOK: OnceLock<Mutex<Option<TraceExportHook>>> = OnceLock::new();
    TRACE_EXPORT_HOOK.get_or_init(|| Mutex::new(None))
}

fn metric_export_hook_slot() -> &'static Mutex<Option<MetricExportHook>> {
    static METRIC_EXPORT_HOOK: OnceLock<Mutex<Option<MetricExportHook>>> = OnceLock::new();
    METRIC_EXPORT_HOOK.get_or_init(|| Mutex::new(None))
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

pub fn export_trace_records_best_effort(records: &[TraceRecord], config: &OtelConfig) {
    let hook = trace_export_hook_slot()
        .lock()
        .expect("atm-daemon trace export hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        hook(records, config);
    }
}

pub fn export_metric_records_best_effort(records: &[MetricRecord], config: &OtelConfig) {
    let hook = metric_export_hook_slot()
        .lock()
        .expect("atm-daemon metric export hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        hook(records, config);
    }
}

pub fn otel_config_from_env() -> OtelConfig {
    OtelConfig::from_env()
}

pub fn current_session_id() -> Option<String> {
    for key in ["CLAUDE_SESSION_ID", "ATM_SESSION_ID", "CODEX_THREAD_ID"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}
