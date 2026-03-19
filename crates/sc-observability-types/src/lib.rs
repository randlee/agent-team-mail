use std::path::PathBuf;

pub const DEFAULT_OTEL_MAX_RETRIES: u32 = 2;
pub const DEFAULT_OTEL_INITIAL_BACKOFF_MS: u64 = 25;
pub const DEFAULT_OTEL_MAX_BACKOFF_MS: u64 = 250;
pub const DEFAULT_OTEL_TIMEOUT_MS: u64 = 1_500;
pub const OTEL_PROTOCOL_HTTP: &str = "otlp_http";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtelConfig {
    pub enabled: bool,
    pub endpoint: Option<String>,
    pub protocol: String,
    pub auth_header: Option<String>,
    pub ca_file: Option<PathBuf>,
    pub insecure_skip_verify: bool,
    pub timeout_ms: u64,
    pub debug_local_export: bool,
    pub max_retries: u32,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            endpoint: None,
            protocol: OTEL_PROTOCOL_HTTP.to_string(),
            auth_header: None,
            ca_file: None,
            insecure_skip_verify: false,
            timeout_ms: DEFAULT_OTEL_TIMEOUT_MS,
            debug_local_export: false,
            max_retries: DEFAULT_OTEL_MAX_RETRIES,
            initial_backoff_ms: DEFAULT_OTEL_INITIAL_BACKOFF_MS,
            max_backoff_ms: DEFAULT_OTEL_MAX_BACKOFF_MS,
        }
    }
}

impl OtelConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Ok(raw) = std::env::var("ATM_OTEL_ENABLED") {
            let norm = raw.trim().to_ascii_lowercase();
            cfg.enabled = !matches!(norm.as_str(), "0" | "false" | "off" | "no" | "disabled");
        }
        if let Ok(raw) = std::env::var("ATM_OTEL_ENDPOINT") {
            let raw = raw.trim();
            cfg.endpoint = (!raw.is_empty()).then(|| raw.to_string());
        }
        if let Ok(raw) = std::env::var("ATM_OTEL_PROTOCOL") {
            let raw = raw.trim();
            if !raw.is_empty() {
                cfg.protocol = raw.to_string();
            }
        }
        if let Ok(raw) = std::env::var("ATM_OTEL_AUTH_HEADER") {
            let raw = raw.trim();
            cfg.auth_header = (!raw.is_empty()).then(|| raw.to_string());
        }
        if let Ok(raw) = std::env::var("ATM_OTEL_CA_FILE") {
            let raw = raw.trim();
            cfg.ca_file = (!raw.is_empty()).then(|| PathBuf::from(raw));
        }
        if let Ok(raw) = std::env::var("ATM_OTEL_INSECURE_SKIP_VERIFY") {
            let norm = raw.trim().to_ascii_lowercase();
            cfg.insecure_skip_verify = matches!(norm.as_str(), "1" | "true" | "on" | "yes");
        }
        if let Ok(raw) = std::env::var("ATM_OTEL_TIMEOUT_MS")
            && let Ok(parsed) = raw.parse::<u64>()
        {
            cfg.timeout_ms = parsed.max(1);
        }
        if let Ok(raw) = std::env::var("ATM_OTEL_DEBUG_LOCAL_EXPORT") {
            let norm = raw.trim().to_ascii_lowercase();
            cfg.debug_local_export = matches!(norm.as_str(), "1" | "true" | "on" | "yes");
        }
        if let Ok(raw) = std::env::var("ATM_OTEL_RETRY_MAX_ATTEMPTS")
            && let Ok(parsed) = raw.parse::<u32>()
        {
            cfg.max_retries = parsed;
        }
        if let Ok(raw) = std::env::var("ATM_OTEL_RETRY_BACKOFF_MS")
            && let Ok(parsed) = raw.parse::<u64>()
        {
            cfg.initial_backoff_ms = parsed;
        }
        if let Ok(raw) = std::env::var("ATM_OTEL_RETRY_MAX_BACKOFF_MS")
            && let Ok(parsed) = raw.parse::<u64>()
        {
            cfg.max_backoff_ms = parsed;
        }
        if cfg.max_backoff_ms < cfg.initial_backoff_ms {
            cfg.max_backoff_ms = cfg.initial_backoff_ms;
        }
        cfg
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OtelError {
    #[error("missing required correlation field '{field}'")]
    MissingRequiredField { field: &'static str },
    #[error(
        "invalid span context: trace_id and span_id must either both be present or both be absent"
    )]
    InvalidSpanContext,
    #[error("export failed: {0}")]
    ExportFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtelExporterKind {
    LocalMirror,
    Collector,
    DebugLocal,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct OtelRecord {
    pub name: String,
    pub source_binary: String,
    pub level: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct TraceRecord {
    pub timestamp: String,
    pub team: Option<String>,
    pub agent: Option<String>,
    pub runtime: Option<String>,
    pub session_id: Option<String>,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub status: TraceStatus,
    pub duration_ms: u64,
    pub source_binary: String,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceStatus {
    Ok,
    Error,
    Unset,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct MetricRecord {
    pub timestamp: String,
    pub team: Option<String>,
    pub agent: Option<String>,
    pub runtime: Option<String>,
    pub session_id: Option<String>,
    pub name: String,
    pub kind: MetricKind,
    pub value: f64,
    pub unit: Option<String>,
    pub source_binary: String,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    Counter,
    Gauge,
    Histogram,
}
