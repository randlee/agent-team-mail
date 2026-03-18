use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use agent_team_mail_core::daemon_client::daemon_status_path_for;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct LoggingHealthSnapshot {
    pub(crate) state: String,
    pub(crate) dropped_counter: u64,
    pub(crate) spool_path: String,
    pub(crate) last_error: Option<String>,
    pub(crate) canonical_log_path: String,
    pub(crate) spool_count: u64,
    pub(crate) oldest_spool_age: Option<u64>,
}

impl Default for LoggingHealthSnapshot {
    fn default() -> Self {
        Self {
            state: "unavailable".to_string(),
            dropped_counter: 0,
            spool_path: String::new(),
            last_error: None,
            canonical_log_path: String::new(),
            spool_count: 0,
            oldest_spool_age: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct OtelHealthSnapshot {
    pub(crate) schema_version: String,
    pub(crate) enabled: bool,
    pub(crate) collector_endpoint: Option<String>,
    pub(crate) protocol: String,
    pub(crate) collector_state: String,
    pub(crate) local_mirror_state: String,
    pub(crate) local_mirror_path: String,
    pub(crate) debug_local_export: bool,
    pub(crate) debug_local_state: String,
    pub(crate) last_error: OtelLastError,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct LastError {
    pub(crate) code: Option<String>,
    pub(crate) message: Option<String>,
    pub(crate) at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct OtelLastError {
    pub(crate) code: Option<String>,
    pub(crate) message: Option<String>,
    pub(crate) at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LoggingHealthContract {
    pub(crate) schema_version: String,
    pub(crate) state: String,
    pub(crate) log_root: String,
    pub(crate) canonical_log_path: String,
    pub(crate) spool_path: String,
    pub(crate) dropped_events_total: u64,
    pub(crate) spool_file_count: u64,
    pub(crate) oldest_spool_age_seconds: Option<u64>,
    pub(crate) last_error: LastError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OtelHealthContract {
    pub(crate) schema_version: String,
    pub(crate) enabled: bool,
    pub(crate) collector_endpoint: Option<String>,
    pub(crate) protocol: String,
    pub(crate) collector_state: String,
    pub(crate) local_mirror_state: String,
    pub(crate) local_mirror_path: String,
    pub(crate) debug_local_export: bool,
    pub(crate) debug_local_state: String,
    pub(crate) last_error: OtelLastError,
}

impl Default for OtelHealthContract {
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

impl Default for LoggingHealthContract {
    fn default() -> Self {
        Self {
            schema_version: "v1".to_string(),
            state: "unavailable".to_string(),
            log_root: String::new(),
            canonical_log_path: String::new(),
            spool_path: String::new(),
            dropped_events_total: 0,
            spool_file_count: 0,
            oldest_spool_age_seconds: None,
            last_error: LastError::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DaemonStatusSnapshot {
    #[serde(default)]
    logging: LoggingHealthSnapshot,
    #[serde(default)]
    otel: OtelHealthSnapshot,
}

pub(crate) fn read_daemon_logging_health(home_dir: &Path) -> LoggingHealthSnapshot {
    let status_path = daemon_status_path_for(home_dir);
    let Ok(content) = fs::read_to_string(status_path) else {
        return LoggingHealthSnapshot::default();
    };
    serde_json::from_str::<DaemonStatusSnapshot>(&content)
        .map(|status| status.logging)
        .unwrap_or_default()
}

pub(crate) fn read_daemon_otel_health(home_dir: &Path) -> OtelHealthSnapshot {
    let status_path = daemon_status_path_for(home_dir);
    let Ok(content) = fs::read_to_string(status_path) else {
        return OtelHealthSnapshot::default();
    };
    serde_json::from_str::<DaemonStatusSnapshot>(&content)
        .map(|status| status.otel)
        .unwrap_or_default()
}

pub(crate) fn build_logging_health_contract(
    logging: &LoggingHealthSnapshot,
    home_dir: &Path,
) -> LoggingHealthContract {
    let log_root = resolve_log_root(&logging.canonical_log_path, home_dir);
    let last_error = build_last_error(logging);
    LoggingHealthContract {
        schema_version: "v1".to_string(),
        state: logging.state.clone(),
        log_root,
        canonical_log_path: logging.canonical_log_path.clone(),
        spool_path: logging.spool_path.clone(),
        dropped_events_total: logging.dropped_counter,
        spool_file_count: logging.spool_count,
        oldest_spool_age_seconds: logging.oldest_spool_age,
        last_error,
    }
}

pub(crate) fn build_otel_health_contract(otel: &OtelHealthSnapshot) -> OtelHealthContract {
    OtelHealthContract {
        schema_version: otel.schema_version.clone(),
        enabled: otel.enabled,
        collector_endpoint: otel.collector_endpoint.clone(),
        protocol: otel.protocol.clone(),
        collector_state: otel.collector_state.clone(),
        local_mirror_state: otel.local_mirror_state.clone(),
        local_mirror_path: otel.local_mirror_path.clone(),
        debug_local_export: otel.debug_local_export,
        debug_local_state: otel.debug_local_state.clone(),
        last_error: otel.last_error.clone(),
    }
}

pub(crate) fn logging_remediation(state: &str) -> Option<&'static str> {
    match state {
        "degraded_dropping" => {
            Some("queue is dropping events; verify daemon health and reduce log burst load")
        }
        "degraded_spooling" => Some(
            "events are spooling locally; verify daemon socket/path and allow merge to catch up",
        ),
        "unavailable" => Some(
            "logging unavailable; check ATM_LOG value, daemon status, and log path permissions",
        ),
        _ => None,
    }
}

fn resolve_log_root(canonical_log_path: &str, home_dir: &Path) -> String {
    if !canonical_log_path.trim().is_empty() {
        let candidate = PathBuf::from(canonical_log_path);
        if let Some(parent) = candidate.parent().and_then(|p| p.parent()) {
            return parent.to_string_lossy().into_owned();
        }
        if let Some(parent) = candidate.parent() {
            return parent.to_string_lossy().into_owned();
        }
    }
    home_dir
        .join(".config/atm/logs")
        .to_string_lossy()
        .into_owned()
}

fn build_last_error(logging: &LoggingHealthSnapshot) -> LastError {
    let Some(message) = logging
        .last_error
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    else {
        return LastError::default();
    };

    let code = match logging.state.as_str() {
        "degraded_spooling" => "DEGRADED_SPOOLING",
        "degraded_dropping" => "DEGRADED_DROPPING",
        "unavailable" => "UNAVAILABLE",
        _ => "LOGGING_ERROR",
    };

    LastError {
        code: Some(code.to_string()),
        message: Some(message.to_string()),
        at: Some(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)),
    }
}
