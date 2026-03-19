use crate::{OTEL_PROTOCOL_HTTP, OtelConfig, OtelError, OtelExporterKind, default_otel_path};
use agent_team_mail_core::observability::{OtelHealthSnapshot, OtelLastError};
use chrono::{SecondsFormat, Utc};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Default)]
struct OtelRuntimeHealth {
    collector_error: Option<String>,
    collector_error_at: Option<String>,
    collector_success_at: Option<String>,
    local_mirror_error: Option<String>,
    local_mirror_error_at: Option<String>,
    local_mirror_success_at: Option<String>,
    debug_local_error: Option<String>,
    debug_local_error_at: Option<String>,
    debug_local_success_at: Option<String>,
}

fn otel_runtime_health_slot() -> &'static Mutex<OtelRuntimeHealth> {
    static OTEL_RUNTIME_HEALTH: OnceLock<Mutex<OtelRuntimeHealth>> = OnceLock::new();
    OTEL_RUNTIME_HEALTH.get_or_init(|| Mutex::new(OtelRuntimeHealth::default()))
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn note_export_success(kind: OtelExporterKind) {
    let mut health = otel_runtime_health_slot()
        .lock()
        .expect("sc-observability otel health lock poisoned");
    let now = now_rfc3339();
    match kind {
        OtelExporterKind::Collector => {
            health.collector_error = None;
            health.collector_error_at = None;
            health.collector_success_at = Some(now);
        }
        OtelExporterKind::LocalMirror => {
            health.local_mirror_error = None;
            health.local_mirror_error_at = None;
            health.local_mirror_success_at = Some(now);
        }
        OtelExporterKind::DebugLocal => {
            health.debug_local_error = None;
            health.debug_local_error_at = None;
            health.debug_local_success_at = Some(now);
        }
    }
}

pub(crate) fn note_export_failure(kind: OtelExporterKind, err: &OtelError) {
    let mut health = otel_runtime_health_slot()
        .lock()
        .expect("sc-observability otel health lock poisoned");
    let now = now_rfc3339();
    match kind {
        OtelExporterKind::Collector => {
            health.collector_error = Some(err.to_string());
            health.collector_error_at = Some(now);
        }
        OtelExporterKind::LocalMirror => {
            health.local_mirror_error = Some(err.to_string());
            health.local_mirror_error_at = Some(now);
        }
        OtelExporterKind::DebugLocal => {
            health.debug_local_error = Some(err.to_string());
            health.debug_local_error_at = Some(now);
        }
    }
}

fn health_state(
    configured: bool,
    success_at: &Option<String>,
    error_at: &Option<String>,
) -> String {
    if !configured {
        return "disabled".to_string();
    }
    match (success_at, error_at) {
        (Some(success), Some(error)) => {
            if error > success {
                "degraded".to_string()
            } else {
                "healthy".to_string()
            }
        }
        (Some(_), None) => "healthy".to_string(),
        (None, Some(_)) => "degraded".to_string(),
        (None, None) => "configured".to_string(),
    }
}

pub fn current_otel_health(log_path: &Path) -> OtelHealthSnapshot {
    let config = OtelConfig::from_env();
    let local_mirror_path = default_otel_path(log_path);
    let health = otel_runtime_health_slot()
        .lock()
        .expect("sc-observability otel health lock poisoned");

    let collector_configured = config.enabled
        && config
            .endpoint
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
    let debug_local_configured = config.enabled && config.debug_local_export;
    let local_mirror_configured = config.enabled;

    let last_error = if health.collector_error.is_some() {
        OtelLastError {
            code: Some("COLLECTOR_EXPORT_FAILED".to_string()),
            message: health.collector_error.clone(),
            at: health.collector_error_at.clone(),
        }
    } else if health.local_mirror_error.is_some() {
        OtelLastError {
            code: Some("LOCAL_MIRROR_EXPORT_FAILED".to_string()),
            message: health.local_mirror_error.clone(),
            at: health.local_mirror_error_at.clone(),
        }
    } else if health.debug_local_error.is_some() {
        OtelLastError {
            code: Some("DEBUG_LOCAL_EXPORT_FAILED".to_string()),
            message: health.debug_local_error.clone(),
            at: health.debug_local_error_at.clone(),
        }
    } else {
        OtelLastError::default()
    };

    let collector_state = if !config.enabled {
        "disabled".to_string()
    } else if !collector_configured {
        "not_configured".to_string()
    } else {
        health_state(
            true,
            &health.collector_success_at,
            &health.collector_error_at,
        )
    };

    OtelHealthSnapshot {
        schema_version: "v1".to_string(),
        enabled: config.enabled,
        collector_endpoint: config.endpoint.clone(),
        protocol: if config.protocol.trim().is_empty() {
            OTEL_PROTOCOL_HTTP.to_string()
        } else {
            config.protocol.clone()
        },
        collector_state,
        local_mirror_state: health_state(
            local_mirror_configured,
            &health.local_mirror_success_at,
            &health.local_mirror_error_at,
        ),
        local_mirror_path: local_mirror_path.to_string_lossy().into_owned(),
        debug_local_export: config.debug_local_export,
        debug_local_state: health_state(
            debug_local_configured,
            &health.debug_local_success_at,
            &health.debug_local_error_at,
        ),
        last_error,
    }
}
