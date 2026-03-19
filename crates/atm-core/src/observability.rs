//! Neutral observability contracts shared across ATM crates.
//!
//! These types intentionally live in `atm-core` so entry-point crates can pass
//! OTel health state around without taking a direct dependency on
//! `sc-observability`.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, Default)]
pub struct OtelLastError {
    pub code: Option<String>,
    pub message: Option<String>,
    pub at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(default)]
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
