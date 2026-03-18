use agent_team_mail_core::logging_event::LogEventV1;
use chrono::Utc;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

pub const LOG_EVENT_QUEUE_CAPACITY: usize = 4096;
pub const SOCKET_ERROR_VERSION_MISMATCH: &str = "VERSION_MISMATCH";
pub const SOCKET_ERROR_INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
pub const SOCKET_ERROR_INTERNAL_ERROR: &str = "INTERNAL_ERROR";

pub type OtelExportHook = Arc<dyn Fn(&Path, &LogEventV1) + Send + Sync>;
pub type OtelHealthHook = Arc<dyn Fn(&Path) -> OtelHealthSnapshot + Send + Sync>;
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, Default)]
pub struct OtelLastError {
    pub code: Option<String>,
    pub message: Option<String>,
    pub at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct OtelHealthSnapshot {
    pub schema_version: String,
    pub enabled: bool,
    pub collector_endpoint: Option<String>,
    pub protocol: String,
    pub collector_state: String,
    pub local_mirror_state: String,
    pub local_mirror_path: String,
    pub debug_local_export: bool,
    pub debug_local_state: String,
    pub last_error: OtelLastError,
}

impl Default for OtelHealthSnapshot {
    fn default() -> Self {
        Self {
            schema_version: "v1".to_string(),
            enabled: true,
            collector_endpoint: None,
            protocol: "otlp_http".to_string(),
            collector_state: "not_configured".to_string(),
            local_mirror_state: "healthy".to_string(),
            local_mirror_path: String::new(),
            debug_local_export: false,
            debug_local_state: "disabled".to_string(),
            last_error: OtelLastError::default(),
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

fn lifecycle_trace_hook_slot() -> &'static Mutex<Option<LifecycleTraceHook>> {
    static LIFECYCLE_TRACE_HOOK: OnceLock<Mutex<Option<LifecycleTraceHook>>> = OnceLock::new();
    LIFECYCLE_TRACE_HOOK.get_or_init(|| Mutex::new(None))
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
