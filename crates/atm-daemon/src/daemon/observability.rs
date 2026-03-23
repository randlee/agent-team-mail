use agent_team_mail_core::logging_event::LogEventV1;
pub use agent_team_mail_core::observability::{OtelHealthSnapshot, OtelLastError};
use chrono::Utc;
use sc_observability_types::{MetricRecord, OtelConfig, TraceRecord};
use std::collections::BTreeMap;
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
pub type LifecycleTraceHook = Arc<dyn Fn(LifecycleTraceRecord) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleTraceStatus {
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleTraceRecord {
    pub timestamp: String,
    pub session_id: Option<String>,
    pub trace_id: String,
    pub span_id: String,
    pub name: String,
    pub status: LifecycleTraceStatus,
    pub source_binary: String,
    pub attributes: BTreeMap<String, String>,
}

impl LifecycleTraceRecord {
    pub fn new(
        name: impl Into<String>,
        status: LifecycleTraceStatus,
        trace_id: String,
        span_id: String,
    ) -> Self {
        Self {
            timestamp: Utc::now().to_rfc3339(),
            session_id: current_session_id(),
            trace_id,
            span_id,
            name: name.into(),
            status,
            source_binary: "atm-daemon".to_string(),
            attributes: BTreeMap::new(),
        }
    }
}

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

pub fn install_lifecycle_trace_hook(hook: LifecycleTraceHook) {
    *lifecycle_trace_hook_slot()
        .lock()
        .expect("atm-daemon lifecycle trace hook lock poisoned") = Some(hook);
}

pub fn clear_lifecycle_trace_hook() {
    *lifecycle_trace_hook_slot()
        .lock()
        .expect("atm-daemon lifecycle trace hook lock poisoned") = None;
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

fn lifecycle_trace_hook_slot() -> &'static Mutex<Option<LifecycleTraceHook>> {
    static LIFECYCLE_TRACE_HOOK: OnceLock<Mutex<Option<LifecycleTraceHook>>> = OnceLock::new();
    LIFECYCLE_TRACE_HOOK.get_or_init(|| Mutex::new(None))
}

fn otel_export_serial_lock() -> &'static Mutex<()> {
    static OTEL_EXPORT_SERIAL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    OTEL_EXPORT_SERIAL_LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(test)]
fn pending_test_otel_export_threads() -> &'static Mutex<Vec<std::thread::JoinHandle<()>>> {
    static PENDING_TEST_OTEL_EXPORT_THREADS: OnceLock<Mutex<Vec<std::thread::JoinHandle<()>>>> =
        OnceLock::new();
    PENDING_TEST_OTEL_EXPORT_THREADS.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(test)]
pub fn wait_for_test_otel_exports() {
    let handles = {
        let mut pending = pending_test_otel_export_threads()
            .lock()
            .expect("atm-daemon test otel export thread lock poisoned");
        std::mem::take(&mut *pending)
    };
    for handle in handles {
        handle
            .join()
            .expect("atm-daemon test otel export thread should complete");
    }
}
fn trace_export_serial_lock() -> &'static Mutex<()> {
    static TRACE_EXPORT_SERIAL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    TRACE_EXPORT_SERIAL_LOCK.get_or_init(|| Mutex::new(()))
}

fn metric_export_serial_lock() -> &'static Mutex<()> {
    static METRIC_EXPORT_SERIAL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    METRIC_EXPORT_SERIAL_LOCK.get_or_init(|| Mutex::new(()))
}

pub fn export_otel_best_effort(log_path: &Path, event: &LogEventV1) {
    let hook = otel_export_hook_slot()
        .lock()
        .expect("atm-daemon otel export hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        let log_path = log_path.to_path_buf();
        let event = event.clone();
        if let Ok(export_thread) = std::thread::Builder::new()
            .name("atm-daemon-otel-export".to_string())
            .spawn(move || {
                let _guard = otel_export_serial_lock()
                    .lock()
                    .expect("atm-daemon otel export serial lock poisoned");
                hook(&log_path, &event);
            })
        {
            #[cfg(test)]
            {
                pending_test_otel_export_threads()
                    .lock()
                    .expect("atm-daemon test otel export thread lock poisoned")
                    .push(export_thread);
            }
            #[cfg(not(test))]
            {
                let _ = export_thread;
            }
        }
    }
}

pub fn export_lifecycle_trace(record: LifecycleTraceRecord) {
    let hook = lifecycle_trace_hook_slot()
        .lock()
        .expect("atm-daemon lifecycle trace hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        hook(record);
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
        let records = records.to_vec();
        let config = config.clone();
        let _ = std::thread::Builder::new()
            .name("atm-daemon-trace-export".to_string())
            .spawn(move || {
                let _guard = trace_export_serial_lock()
                    .lock()
                    .expect("atm-daemon trace export serial lock poisoned");
                hook(&records, &config);
            });
    }
}

pub fn export_metric_records_best_effort(records: &[MetricRecord], config: &OtelConfig) {
    let hook = metric_export_hook_slot()
        .lock()
        .expect("atm-daemon metric export hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        let records = records.to_vec();
        let config = config.clone();
        let _ = std::thread::Builder::new()
            .name("atm-daemon-metric-export".to_string())
            .spawn(move || {
                let _guard = metric_export_serial_lock()
                    .lock()
                    .expect("atm-daemon metric export serial lock poisoned");
                hook(&records, &config);
            });
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
