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

#[cfg(test)]
mod tests {
    use super::{OtelHealthSnapshot, OtelLastError};

    #[test]
    fn otel_health_snapshot_round_trips_when_fully_populated() {
        let snapshot = OtelHealthSnapshot {
            schema_version: "v1".to_string(),
            enabled: true,
            collector_endpoint: Some("https://collector.example/v1".to_string()),
            protocol: "otlp_http".to_string(),
            collector_state: "healthy".to_string(),
            local_mirror_state: "healthy".to_string(),
            local_mirror_path: std::env::temp_dir()
                .join("atm.log.otel.jsonl")
                .to_string_lossy()
                .into_owned(),
            debug_local_export: true,
            debug_local_state: "healthy".to_string(),
            last_error: OtelLastError {
                code: Some("collector_timeout".to_string()),
                message: Some("collector timed out".to_string()),
                at: Some("2026-03-18T12:34:56Z".to_string()),
            },
        };

        let encoded = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let decoded: OtelHealthSnapshot =
            serde_json::from_str(&encoded).expect("deserialize snapshot");
        assert_eq!(decoded, snapshot);
        assert_eq!(
            decoded.last_error,
            OtelLastError {
                code: Some("collector_timeout".to_string()),
                message: Some("collector timed out".to_string()),
                at: Some("2026-03-18T12:34:56Z".to_string()),
            }
        );
    }

    #[test]
    fn otel_health_snapshot_round_trips_when_default() {
        let snapshot = OtelHealthSnapshot::default();

        let encoded = serde_json::to_string(&snapshot).expect("serialize default snapshot");
        let decoded: OtelHealthSnapshot =
            serde_json::from_str(&encoded).expect("deserialize default snapshot");
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn otel_last_error_nested_fields_round_trip() {
        let snapshot = OtelHealthSnapshot {
            last_error: OtelLastError {
                code: Some("auth_failed".to_string()),
                message: Some("invalid auth header".to_string()),
                at: Some("2026-03-18T18:22:01Z".to_string()),
            },
            ..OtelHealthSnapshot::default()
        };

        let value = serde_json::to_value(&snapshot).expect("serialize nested error");
        let decoded: OtelHealthSnapshot =
            serde_json::from_value(value).expect("deserialize nested error");
        assert_eq!(decoded.last_error.code.as_deref(), Some("auth_failed"));
        assert_eq!(
            decoded.last_error.message.as_deref(),
            Some("invalid auth header")
        );
        assert_eq!(
            decoded.last_error.at.as_deref(),
            Some("2026-03-18T18:22:01Z")
        );
    }
}
