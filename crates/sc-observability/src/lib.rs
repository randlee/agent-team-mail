//! Shared observability primitives for ATM ecosystem tools.
//!
//! AH.1 scope:
//! - `Logger` + `emit()` with JSONL rotation
//! - `LogConfig` with environment-driven defaults
//! - spool write/merge semantics
//! - socket error-code constants for the `log-event` contract

use agent_team_mail_core::logging_event::{LogEventV1, ValidationError};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use thiserror::Error;

mod health;
mod otlp_adapter;

pub use health::{OtelHealthSnapshot, OtelLastError, current_otel_health};

pub const DEFAULT_QUEUE_CAPACITY: usize = 4096;
pub const DEFAULT_MAX_EVENT_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_BYTES: u64 = 50 * 1024 * 1024;
pub const DEFAULT_MAX_FILES: u32 = 5;
pub const DEFAULT_RETENTION_DAYS: u32 = 7;
pub const DEFAULT_OTEL_MAX_RETRIES: u32 = 2;
pub const DEFAULT_OTEL_INITIAL_BACKOFF_MS: u64 = 25;
pub const DEFAULT_OTEL_MAX_BACKOFF_MS: u64 = 250;
pub const DEFAULT_OTEL_TIMEOUT_MS: u64 = 1_500;
pub const OTEL_PROTOCOL_HTTP: &str = "otlp_http";

pub const SOCKET_ERROR_VERSION_MISMATCH: &str = "VERSION_MISMATCH";
pub const SOCKET_ERROR_INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
pub const SOCKET_ERROR_INTERNAL_ERROR: &str = "INTERNAL_ERROR";

#[derive(Debug, Clone)]
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

#[derive(Debug, Error, PartialEq, Eq)]
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

pub trait OtelExporter: Send + Sync {
    fn kind(&self) -> OtelExporterKind;
    fn export(&self, record: &OtelRecord) -> Result<(), OtelError>;
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
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// Neutral trace signal contract for producer-side observability code.
///
/// Correlation fields are intentionally optional and fail-open in AW.1 so
/// producers can adopt trace emission incrementally without blocking callers.
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

/// Neutral metric signal contract for producer-side observability code.
///
/// Correlation fields are intentionally optional and fail-open in AW.1 so
/// metric rollout can happen before every producer is fully correlated.
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

#[derive(Debug, Clone)]
pub struct FileOtelExporter {
    path: PathBuf,
}

impl FileOtelExporter {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl OtelExporter for FileOtelExporter {
    fn kind(&self) -> OtelExporterKind {
        OtelExporterKind::LocalMirror
    }

    fn export(&self, record: &OtelRecord) -> Result<(), OtelError> {
        if let Some(parent) = self.path.parent()
            && let Err(err) = fs::create_dir_all(parent)
        {
            return Err(OtelError::ExportFailed(err.to_string()));
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|err| OtelError::ExportFailed(err.to_string()))?;
        let line = serde_json::to_string(record)
            .map_err(|err| OtelError::ExportFailed(err.to_string()))?;
        writeln!(file, "{line}").map_err(|err| OtelError::ExportFailed(err.to_string()))
    }
}

#[derive(Clone)]
struct OtelPipeline {
    config: OtelConfig,
    exporters: Vec<Arc<dyn OtelExporter>>,
    log_path: PathBuf,
    sleeper: fn(Duration),
}

impl std::fmt::Debug for OtelPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtelPipeline")
            .field("config", &self.config)
            .finish()
    }
}

impl OtelPipeline {
    fn new_default(log_path: &Path) -> Self {
        let config = OtelConfig::from_env();
        let mut exporters: Vec<Arc<dyn OtelExporter>> =
            vec![Arc::new(FileOtelExporter::new(default_otel_path(log_path)))];
        if let Ok(mut transport_exporters) = otlp_adapter::build_transport_exporters(&config) {
            exporters.append(&mut transport_exporters);
        }
        Self {
            config,
            exporters,
            log_path: log_path.to_path_buf(),
            sleeper: std::thread::sleep,
        }
    }

    fn export_event(&self, event: &LogEventV1) -> Result<(), OtelError> {
        export_otel_with_retry(
            event,
            &self.config,
            &self.exporters,
            &self.log_path,
            self.sleeper,
        )
    }
}

fn export_otel_with_retry(
    event: &LogEventV1,
    config: &OtelConfig,
    exporters: &[Arc<dyn OtelExporter>],
    _log_path: &Path,
    sleeper: fn(Duration),
) -> Result<(), OtelError> {
    if !config.enabled {
        return Ok(());
    }
    let record = build_otel_record(event)?;
    if exporters.is_empty() {
        return Ok(());
    }

    let mut attempt: u32 = 0;
    let mut backoff = config.initial_backoff_ms;
    loop {
        let mut last_err = None;
        let mut any_failed = false;
        for exporter in exporters {
            if let Err(err) = exporter.export(&record) {
                health::note_export_failure(exporter.kind(), &err);
                any_failed = true;
                last_err = Some(err);
            } else {
                health::note_export_success(exporter.kind());
            }
        }
        if !any_failed {
            return Ok(());
        }
        if attempt >= config.max_retries {
            return Err(last_err
                .unwrap_or_else(|| OtelError::ExportFailed("unknown export failure".to_string())));
        }
        sleeper(Duration::from_millis(backoff));
        backoff = backoff.saturating_mul(2).min(config.max_backoff_ms);
        attempt = attempt.saturating_add(1);
    }
}

/// Export to OTel without allowing exporter failures to fail the caller.
///
/// This is the public fail-open helper for producer-only code paths that do
/// not own a full [`Logger`] instance.
pub fn export_otel_best_effort(
    event: &LogEventV1,
    config: &OtelConfig,
    exporter: &dyn OtelExporter,
) {
    if !config.enabled {
        return;
    }
    let Ok(record) = build_otel_record(event) else {
        return;
    };

    let mut attempt: u32 = 0;
    let mut backoff = config.initial_backoff_ms;
    loop {
        if exporter.export(&record).is_ok() {
            health::note_export_success(exporter.kind());
            return;
        }
        health::note_export_failure(
            exporter.kind(),
            &OtelError::ExportFailed("best-effort exporter failed".to_string()),
        );
        if attempt >= config.max_retries {
            return;
        }
        std::thread::sleep(Duration::from_millis(backoff));
        backoff = backoff.saturating_mul(2).min(config.max_backoff_ms);
        attempt = attempt.saturating_add(1);
    }
}

/// Export trace records without allowing exporter failures to affect callers.
///
/// AW.3 uses this to emit native trace spans from CLI and daemon code while
/// keeping all failures fail-open.
pub fn export_trace_records_best_effort(records: &[TraceRecord], config: &OtelConfig) {
    if !config.enabled || records.is_empty() {
        return;
    }
    if config
        .endpoint
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        return;
    }
    if let Err(err) = otlp_adapter::export_traces(config, records) {
        health::note_export_failure(OtelExporterKind::Collector, &err);
    } else {
        health::note_export_success(OtelExporterKind::Collector);
    }
}

/// Export metric records without allowing exporter failures to affect callers.
pub fn export_metric_records_best_effort(records: &[MetricRecord], config: &OtelConfig) {
    if !config.enabled || records.is_empty() {
        return;
    }
    if config
        .endpoint
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        return;
    }
    if let Err(err) = otlp_adapter::export_metrics(config, records) {
        health::note_export_failure(OtelExporterKind::Collector, &err);
    } else {
        health::note_export_success(OtelExporterKind::Collector);
    }
}

fn build_otel_record(event: &LogEventV1) -> Result<OtelRecord, OtelError> {
    let runtime_scoped = event.team.is_some()
        || event.agent.is_some()
        || event.runtime.is_some()
        || event.session_id.is_some();
    if runtime_scoped {
        for (field, value) in [
            ("team", event.team.as_deref()),
            ("agent", event.agent.as_deref()),
            ("runtime", event.runtime.as_deref()),
            ("session_id", event.session_id.as_deref()),
        ] {
            if value.is_none_or(|v| v.trim().is_empty()) {
                return Err(OtelError::MissingRequiredField { field });
            }
        }
    }

    let has_trace = event
        .trace_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let has_span = event
        .span_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    if has_trace != has_span {
        return Err(OtelError::InvalidSpanContext);
    }

    let subagent_scoped = event.subagent_id.is_some() || event.action.starts_with("subagent.");
    if subagent_scoped {
        if event
            .subagent_id
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(OtelError::MissingRequiredField {
                field: "subagent_id",
            });
        }
        for (field, value) in [
            ("team", event.team.as_deref()),
            ("agent", event.agent.as_deref()),
            ("runtime", event.runtime.as_deref()),
            ("session_id", event.session_id.as_deref()),
            ("trace_id", event.trace_id.as_deref()),
            ("span_id", event.span_id.as_deref()),
        ] {
            if value.is_none_or(|v| v.trim().is_empty()) {
                return Err(OtelError::MissingRequiredField { field });
            }
        }
    }

    let mut attributes = serde_json::Map::new();
    if let Some(team) = event.team.as_ref() {
        attributes.insert("team".to_string(), serde_json::Value::String(team.clone()));
    }
    if let Some(agent) = event.agent.as_ref() {
        attributes.insert(
            "agent".to_string(),
            serde_json::Value::String(agent.clone()),
        );
    }
    if let Some(runtime) = event.runtime.as_ref() {
        attributes.insert(
            "runtime".to_string(),
            serde_json::Value::String(runtime.clone()),
        );
    }
    if let Some(session_id) = event.session_id.as_ref() {
        attributes.insert(
            "session_id".to_string(),
            serde_json::Value::String(session_id.clone()),
        );
    }
    if let Some(subagent_id) = event.subagent_id.as_ref() {
        attributes.insert(
            "subagent_id".to_string(),
            serde_json::Value::String(subagent_id.clone()),
        );
    }
    attributes.insert(
        "source_binary".to_string(),
        serde_json::Value::String(event.source_binary.clone()),
    );
    attributes.insert(
        "target".to_string(),
        serde_json::Value::String(event.target.clone()),
    );
    attributes.insert(
        "action".to_string(),
        serde_json::Value::String(event.action.clone()),
    );
    for (key, value) in &event.fields {
        attributes
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }

    Ok(OtelRecord {
        name: event.action.clone(),
        trace_id: event.trace_id.clone(),
        span_id: event.span_id.clone(),
        attributes,
    })
}

pub use agent_team_mail_core::logging_event::SpanRefV1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl FromStr for LogLevel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogConfig {
    pub log_path: PathBuf,
    pub spool_dir: PathBuf,
    pub level: LogLevel,
    pub message_preview_enabled: bool,
    pub max_bytes: u64,
    pub max_files: u32,
    pub retention_days: u32,
    pub queue_capacity: usize,
    pub max_event_bytes: usize,
}

impl LogConfig {
    fn normalize_tool_name(tool: &str) -> String {
        let trimmed = tool.trim();
        if trimmed.is_empty() {
            return "atm".to_string();
        }
        trimmed
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                    ch
                } else {
                    '_'
                }
            })
            .collect()
    }

    fn canonical_log_path(home_dir: &Path, tool: &str) -> PathBuf {
        let tool = Self::normalize_tool_name(tool);
        home_dir
            .join(".config")
            .join("atm")
            .join("logs")
            .join(&tool)
            .join(format!("{tool}.log.jsonl"))
    }

    fn canonical_spool_dir(home_dir: &Path, tool: &str) -> PathBuf {
        let tool = Self::normalize_tool_name(tool);
        home_dir
            .join(".config")
            .join("atm")
            .join("logs")
            .join(tool)
            .join("spool")
    }

    fn spool_dir_from_log_path(log_path: &Path) -> PathBuf {
        log_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("spool")
    }

    pub fn from_home(home_dir: &Path) -> Self {
        Self::from_home_for_tool(home_dir, "atm")
    }

    pub fn from_home_for_tool(home_dir: &Path, tool: &str) -> Self {
        let log_path = std::env::var("ATM_LOG_FILE")
            .or_else(|_| std::env::var("ATM_LOG_PATH"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| Self::canonical_log_path(home_dir, tool));

        let spool_dir =
            if std::env::var("ATM_LOG_FILE").is_ok() || std::env::var("ATM_LOG_PATH").is_ok() {
                Self::spool_dir_from_log_path(&log_path)
            } else {
                Self::canonical_spool_dir(home_dir, tool)
            };
        let level = std::env::var("ATM_LOG")
            .ok()
            .and_then(|v| LogLevel::from_str(&v).ok())
            .unwrap_or(LogLevel::Info);
        let message_preview_enabled = std::env::var("ATM_LOG_MSG")
            .ok()
            .map(|v| v.trim() == "1")
            .unwrap_or(false);
        let max_bytes = std::env::var("ATM_LOG_MAX_BYTES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MAX_BYTES);
        let max_files = std::env::var("ATM_LOG_MAX_FILES")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(DEFAULT_MAX_FILES);
        let retention_days = std::env::var("ATM_LOG_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|days| *days > 0)
            .unwrap_or(DEFAULT_RETENTION_DAYS);

        Self {
            log_path,
            spool_dir,
            level,
            message_preview_enabled,
            max_bytes,
            max_files,
            retention_days,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        }
    }
}

#[derive(Debug, Error)]
pub enum LoggerError {
    #[error("event validation failed: {0}")]
    Validation(#[from] ValidationError),
    #[error("failed to serialize log event: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("event exceeds configured size guard: {size} > {max}")]
    EventTooLarge { size: usize, max: usize },
}

#[derive(Debug, Clone)]
pub struct Logger {
    config: LogConfig,
    otel: OtelPipeline,
}

/// Apply canonical redaction rules to a logging event.
pub fn redact_event(event: &mut LogEventV1) {
    event.redact();
}

impl Logger {
    pub fn new(config: LogConfig) -> Self {
        let otel = OtelPipeline::new_default(&config.log_path);
        Self { config, otel }
    }

    pub fn with_otel_exporter(
        config: LogConfig,
        otel_config: OtelConfig,
        exporter: Arc<dyn OtelExporter>,
    ) -> Self {
        Self {
            config,
            otel: OtelPipeline {
                config: otel_config,
                exporters: vec![exporter],
                log_path: PathBuf::new(),
                sleeper: std::thread::sleep,
            },
        }
    }

    pub fn config(&self) -> &LogConfig {
        &self.config
    }

    /// Validate, redact, and append an event to the canonical JSONL log.
    ///
    /// # Errors
    ///
    /// Returns an error when validation fails, serialization fails, the event
    /// exceeds `max_event_bytes`, or filesystem writes fail.
    pub fn emit(&self, event: &LogEventV1) -> Result<(), LoggerError> {
        let line = self.prepare_line(event)?;
        self.append_line_to_canonical(&line)?;
        let _ = self.otel.export_event(event);
        Ok(())
    }

    /// Convenience helper for tools that only need action/outcome + fields.
    ///
    /// This builds a [`LogEventV1`] with the configured log level and emits it
    /// through the same validation/redaction/path pipeline as [`Self::emit`].
    pub fn emit_action(
        &self,
        source_binary: &str,
        target: &str,
        action: &str,
        outcome: Option<&str>,
        fields: serde_json::Value,
    ) -> Result<(), LoggerError> {
        let mut event = LogEventV1::builder(source_binary, action, target)
            .level(self.config.level.as_str())
            .build();
        event.outcome = outcome.map(ToOwned::to_owned);
        event.fields = value_to_map(fields);
        self.emit(&event)
    }
    /// Write a human-readable log line to the canonical log path.
    ///
    /// Produces `<timestamp> level=<level> action=<action> outcome=<outcome> fields=<json>`
    /// format, sharing the same file-path and directory-creation logic as
    /// [`Self::emit`]. This routes Human-mode output through SharedLogger rather
    /// than a parallel per-tool implementation.
    ///
    /// # Errors
    ///
    /// Returns an error when directory creation or file appending fails.
    pub fn emit_human(
        &self,
        level: &str,
        action: &str,
        outcome: &str,
        fields: &serde_json::Value,
    ) -> Result<(), LoggerError> {
        use chrono::{SecondsFormat, Utc};
        let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let fields_json = serde_json::to_string(fields).unwrap_or_else(|_| "{}".to_string());
        let line =
            format!("{ts} level={level} action={action} outcome={outcome} fields={fields_json}");
        self.append_line_to_canonical(&line)?;
        Ok(())
    }

    /// Write one event to a per-source spool file for deferred fan-in merge.
    ///
    /// # Errors
    ///
    /// Returns an error when validation/serialization fails, the event exceeds
    /// `max_event_bytes`, or spool file creation/appending fails.
    pub fn write_to_spool(
        &self,
        event: &LogEventV1,
        unix_millis: u128,
    ) -> Result<PathBuf, LoggerError> {
        let line = self.prepare_line(event)?;
        fs::create_dir_all(&self.config.spool_dir)?;

        let name = spool_file_name(&event.source_binary, event.pid, unix_millis);
        let path = self.config.spool_dir.join(name);
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(file, "{line}")?;
        Ok(path)
    }

    /// Merge spool fragments into the canonical log in deterministic order.
    ///
    /// Supports crash-recovery of stale `.claiming` files from interrupted
    /// prior merges.
    ///
    /// # Errors
    ///
    /// Returns an error when reading the spool directory or writing to the
    /// canonical log fails.
    pub fn merge_spool(&self) -> Result<u64, LoggerError> {
        if !self.config.spool_dir.exists() {
            return Ok(0);
        }

        let mut spool_files: Vec<PathBuf> = fs::read_dir(&self.config.spool_dir)?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| {
                path.is_file()
                    && path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext == "jsonl" || ext == "claiming")
                        .unwrap_or(false)
            })
            .collect();
        spool_files.sort();

        let mut claimed_files: Vec<PathBuf> = Vec::new();
        let mut events: Vec<(LogEventV1, String)> = Vec::new();

        for path in spool_files {
            let claiming = if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "claiming")
            {
                path.clone()
            } else {
                let claiming = path.with_extension("claiming");
                if fs::rename(&path, &claiming).is_err() {
                    continue;
                }
                claiming
            };
            let ordering_key = claiming
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();

            let content = match fs::read_to_string(&claiming) {
                Ok(content) => content,
                Err(_) => {
                    let _ = fs::remove_file(&claiming);
                    continue;
                }
            };
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(event) = serde_json::from_str::<LogEventV1>(trimmed) {
                    events.push((event, ordering_key.clone()));
                }
            }
            claimed_files.push(claiming);
        }

        events.sort_by(|(a, file_a), (b, file_b)| a.ts.cmp(&b.ts).then(file_a.cmp(file_b)));

        let mut merged = 0_u64;
        for (event, _) in events {
            let line = serde_json::to_string(&event)?;
            if line.len() > self.config.max_event_bytes {
                continue;
            }
            self.append_line_to_canonical(&line)?;
            merged += 1;
        }

        for claimed in claimed_files {
            let _ = fs::remove_file(claimed);
        }

        Ok(merged)
    }

    fn prepare_line(&self, event: &LogEventV1) -> Result<String, LoggerError> {
        let mut event = event.clone();
        event.validate()?;
        redact_event(&mut event);
        let line = serde_json::to_string(&event)?;
        let size = line.len();
        if size > self.config.max_event_bytes {
            return Err(LoggerError::EventTooLarge {
                size,
                max: self.config.max_event_bytes,
            });
        }
        Ok(line)
    }

    fn append_line_to_canonical(&self, line: &str) -> Result<(), LoggerError> {
        static CANONICAL_APPEND_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let lock = CANONICAL_APPEND_LOCK.get_or_init(|| Mutex::new(()));
        let _guard = lock.lock().expect("canonical append lock poisoned");

        if let Some(parent) = self.config.log_path.parent() {
            fs::create_dir_all(parent)?;
        }

        self.rotate_if_needed()?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.log_path)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    fn rotate_if_needed(&self) -> Result<(), LoggerError> {
        let current_size = fs::metadata(&self.config.log_path)
            .map(|m| m.len())
            .unwrap_or(0);
        if current_size < self.config.max_bytes {
            return Ok(());
        }
        rotate_log_files(&self.config.log_path, self.config.max_files)?;
        Ok(())
    }
}

/// Export a single event to OTel using default pipeline settings (fail-open).
///
/// This helper is intended for producers that already own canonical JSONL
/// writing and only need shared OTel export semantics. It creates an
/// [`OtelPipeline`] from the given log path using default configuration.
///
/// For callers that already have an exporter, use [`export_otel_best_effort`]
/// instead.
pub fn export_otel_best_effort_from_path(log_path: &Path, event: &LogEventV1) {
    let pipeline = OtelPipeline::new_default(log_path);
    export_otel_best_effort_with_pipeline(&pipeline, event);
}

fn export_otel_best_effort_with_pipeline(pipeline: &OtelPipeline, event: &LogEventV1) {
    let _ = pipeline.export_event(event);
}

pub fn spool_file_name(source_binary: &str, pid: u32, unix_millis: u128) -> String {
    let sanitized = sanitize_source_binary(source_binary);
    format!("{}-{}-{}.jsonl", sanitized, pid, unix_millis)
}

fn sanitize_source_binary(source_binary: &str) -> String {
    let mut out = String::with_capacity(source_binary.len());
    for ch in source_binary.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

fn rotate_log_files(base: &Path, max_files: u32) -> Result<(), LoggerError> {
    if max_files == 0 {
        let _ = fs::remove_file(base);
        return Ok(());
    }

    let oldest = rotation_path(base, max_files);
    let _ = fs::remove_file(&oldest);

    for idx in (1..max_files).rev() {
        let from = rotation_path(base, idx);
        let to = rotation_path(base, idx + 1);
        if from.exists() {
            let _ = fs::rename(&from, &to);
        }
    }

    if base.exists() {
        let first = rotation_path(base, 1);
        fs::rename(base, first)?;
    }
    Ok(())
}

pub(crate) fn default_otel_path(log_path: &Path) -> PathBuf {
    let mut otel_path = log_path.to_path_buf();
    let stem = log_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("telemetry");
    otel_path.set_file_name(format!("{stem}.otel.jsonl"));
    otel_path
}

fn rotation_path(base: &Path, n: u32) -> PathBuf {
    let mut os = base.as_os_str().to_os_string();
    os.push(format!(".{n}"));
    PathBuf::from(os)
}

fn value_to_map(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("value".to_string(), other);
            map
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_core::logging_event::new_log_event;
    use serial_test::serial;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Instant;
    use tempfile::TempDir;

    #[derive(Default)]
    struct CountingExporter {
        attempts: AtomicUsize,
        fail_for: AtomicUsize,
        records: Mutex<Vec<OtelRecord>>,
    }

    impl CountingExporter {
        fn with_failures(failures: usize) -> Self {
            Self {
                attempts: AtomicUsize::new(0),
                fail_for: AtomicUsize::new(failures),
                records: Mutex::new(Vec::new()),
            }
        }
    }

    struct TestCollector {
        endpoint: String,
        requests: Arc<Mutex<Vec<String>>>,
        join: Option<thread::JoinHandle<()>>,
    }

    impl TestCollector {
        fn start(status_line: &'static str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind collector");
            listener
                .set_nonblocking(true)
                .expect("collector nonblocking");
            let addr = listener.local_addr().expect("collector addr");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let shared = Arc::clone(&requests);
            let join = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let mut request = Vec::new();
                            let mut header_buf = [0_u8; 4096];
                            let header_len = stream.read(&mut header_buf).expect("read request");
                            request.extend_from_slice(&header_buf[..header_len]);
                            let header_text = String::from_utf8_lossy(&request);
                            let content_length = header_text
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    (name.eq_ignore_ascii_case("content-length"))
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            let header_end = header_text
                                .find("\r\n\r\n")
                                .map(|idx| idx + 4)
                                .unwrap_or(request.len());
                            let body_read = request.len().saturating_sub(header_end);
                            if body_read < content_length {
                                let mut body = vec![0_u8; content_length - body_read];
                                stream.read_exact(&mut body).expect("read request body");
                                request.extend_from_slice(&body);
                            }
                            shared
                                .lock()
                                .expect("collector lock")
                                .push(String::from_utf8_lossy(&request).to_string());
                            let response = format!(
                                "HTTP/1.1 {status_line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            );
                            stream
                                .write_all(response.as_bytes())
                                .expect("write response");
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(err) => panic!("collector accept failed: {err}"),
                    }
                }
            });
            Self {
                endpoint: format!("http://{addr}"),
                requests,
                join: Some(join),
            }
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().expect("collector lock").clone()
        }
    }

    impl Drop for TestCollector {
        fn drop(&mut self) {
            if let Some(join) = self.join.take() {
                join.join().expect("collector thread should join");
            }
        }
    }

    impl OtelExporter for CountingExporter {
        fn kind(&self) -> OtelExporterKind {
            OtelExporterKind::Collector
        }

        fn export(&self, record: &OtelRecord) -> Result<(), OtelError> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            let fail_for = self.fail_for.load(Ordering::SeqCst);
            if attempt <= fail_for {
                return Err(OtelError::ExportFailed(
                    "simulated transport outage".to_string(),
                ));
            }
            self.records
                .lock()
                .expect("records lock")
                .push(record.clone());
            Ok(())
        }
    }

    fn make_event(ts: &str) -> LogEventV1 {
        let mut event = new_log_event("atm", "test_action", "atm::test", "info");
        event.ts = ts.to_string();
        event
    }

    #[test]
    fn trace_record_round_trip_allows_missing_correlation_fields() {
        let record = TraceRecord {
            timestamp: "2026-03-18T06:00:00Z".to_string(),
            team: None,
            agent: None,
            runtime: None,
            session_id: None,
            trace_id: "trace-123".to_string(),
            span_id: "span-456".to_string(),
            parent_span_id: Some("span-000".to_string()),
            name: "atm.send".to_string(),
            status: TraceStatus::Ok,
            duration_ms: 42,
            source_binary: "atm".to_string(),
            attributes: serde_json::Map::from_iter([(
                "target".to_string(),
                serde_json::Value::String("team-lead@atm-dev".to_string()),
            )]),
        };

        let json = serde_json::to_value(&record).expect("serialize trace record");
        let round_trip: TraceRecord =
            serde_json::from_value(json).expect("deserialize trace record");
        assert_eq!(round_trip, record);
    }

    #[test]
    fn metric_record_round_trip_with_partial_correlation() {
        let record = MetricRecord {
            timestamp: "2026-03-18T06:00:00Z".to_string(),
            team: Some("atm-dev".to_string()),
            agent: None,
            runtime: Some("codex".to_string()),
            session_id: None,
            name: "atm_messages_total".to_string(),
            kind: MetricKind::Counter,
            value: 7.0,
            unit: Some("count".to_string()),
            source_binary: "atm".to_string(),
            attributes: serde_json::Map::from_iter([(
                "scope".to_string(),
                serde_json::Value::String("mail".to_string()),
            )]),
        };

        let json = serde_json::to_value(&record).expect("serialize metric record");
        let round_trip: MetricRecord =
            serde_json::from_value(json).expect("deserialize metric record");
        assert_eq!(round_trip, record);
    }

    static BACKOFF_SLEEPS_MS: OnceLock<Mutex<Vec<u64>>> = OnceLock::new();

    fn record_sleep(duration: Duration) {
        BACKOFF_SLEEPS_MS
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .expect("backoff sleeps lock")
            .push(duration.as_millis() as u64);
    }

    #[test]
    #[serial]
    fn config_defaults_and_env_overrides() {
        let tmp = TempDir::new().expect("temp dir");
        let custom_log = tmp.path().join("custom-atm.log");
        let home_root = tmp.path().join("home-root");
        // SAFETY: test-scoped env mutation.
        unsafe {
            std::env::set_var("ATM_LOG", "debug");
            std::env::set_var("ATM_LOG_MSG", "1");
            std::env::set_var("ATM_LOG_FILE", &custom_log);
            std::env::set_var("ATM_LOG_MAX_BYTES", "1024");
            std::env::set_var("ATM_LOG_MAX_FILES", "7");
            std::env::set_var("ATM_LOG_RETENTION_DAYS", "9");
        }
        let cfg = LogConfig::from_home(&home_root);
        assert_eq!(cfg.level, LogLevel::Debug);
        assert!(cfg.message_preview_enabled);
        assert_eq!(cfg.log_path, custom_log);
        assert_eq!(cfg.spool_dir, tmp.path().join("spool"));
        assert_eq!(cfg.max_bytes, 1024);
        assert_eq!(cfg.max_files, 7);
        assert_eq!(cfg.retention_days, 9);
        assert_eq!(cfg.queue_capacity, DEFAULT_QUEUE_CAPACITY);
        assert_eq!(cfg.max_event_bytes, DEFAULT_MAX_EVENT_BYTES);
        // SAFETY: cleanup after test.
        unsafe {
            std::env::remove_var("ATM_LOG");
            std::env::remove_var("ATM_LOG_MSG");
            std::env::remove_var("ATM_LOG_FILE");
            std::env::remove_var("ATM_LOG_MAX_BYTES");
            std::env::remove_var("ATM_LOG_MAX_FILES");
            std::env::remove_var("ATM_LOG_RETENTION_DAYS");
        }
    }

    #[test]
    #[serial]
    fn config_default_paths_follow_tool_scoped_contract() {
        let tmp = TempDir::new().expect("temp dir");
        // SAFETY: test-scoped env cleanup to force default path resolution.
        unsafe {
            std::env::remove_var("ATM_LOG_FILE");
            std::env::remove_var("ATM_LOG_PATH");
        }

        let cfg = LogConfig::from_home_for_tool(tmp.path(), "atm-daemon");
        assert_eq!(
            cfg.log_path,
            tmp.path()
                .join(".config/atm/logs/atm-daemon/atm-daemon.log.jsonl")
        );
        assert_eq!(
            cfg.spool_dir,
            tmp.path().join(".config/atm/logs/atm-daemon/spool")
        );
    }

    #[test]
    fn span_ref_v1_round_trip_serialization() {
        let span = SpanRefV1 {
            name: "compose".to_string(),
            trace_id: "trace-123".to_string(),
            span_id: "span-456".to_string(),
            parent_span_id: None,
            fields: serde_json::Map::new(),
        };
        let json = serde_json::to_string(&span).expect("serialize span");
        let decoded: SpanRefV1 = serde_json::from_str(&json).expect("deserialize span");
        assert_eq!(decoded, span);
    }

    #[test]
    fn span_ref_v1_fields_are_non_empty_after_construction() {
        let span = SpanRefV1 {
            name: "compose".to_string(),
            trace_id: "trace-abc".to_string(),
            span_id: "span-def".to_string(),
            parent_span_id: None,
            fields: serde_json::Map::new(),
        };
        assert!(!span.trace_id.is_empty());
        assert!(!span.span_id.is_empty());
    }

    #[test]
    fn spool_filename_format_matches_contract() {
        let name = spool_file_name("atm-daemon", 44201, 123456789);
        assert_eq!(name, "atm-daemon-44201-123456789.jsonl");
    }

    #[test]
    fn spool_filename_sanitizes_windows_unsafe_chars() {
        let name = spool_file_name(r"atm\daemon:core?*", 44201, 123456789);
        assert_eq!(name, "atm_daemon_core__-44201-123456789.jsonl");
    }

    #[test]
    fn emit_rotates_file() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: 1,
            max_files: 2,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };
        let logger = Logger::new(cfg);

        let ev1 = make_event("2026-03-09T00:00:01Z");
        logger.emit(&ev1).expect("first emit");
        let ev2 = make_event("2026-03-09T00:00:02Z");
        logger.emit(&ev2).expect("second emit");

        assert!(logger.config.log_path.exists());
        assert!(rotation_path(&logger.config.log_path, 1).exists());
    }

    #[test]
    fn emit_rejects_event_larger_than_configured_guard() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: 256,
        };
        let logger = Logger::new(cfg);

        let mut event = make_event("2026-03-09T00:00:01Z");
        event.fields.insert(
            "blob".to_string(),
            serde_json::Value::String("x".repeat(2048)),
        );
        let err = logger.emit(&event).expect_err("expected size guard error");
        assert!(matches!(err, LoggerError::EventTooLarge { .. }));
    }

    #[test]
    fn merge_spool_sorts_by_timestamp_and_deletes_claimed_files() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };
        let logger = Logger::new(cfg.clone());

        let ev_late = make_event("2026-03-09T00:00:05Z");
        let ev_early = make_event("2026-03-09T00:00:01Z");
        logger
            .write_to_spool(&ev_late, 2000)
            .expect("write late spool");
        logger
            .write_to_spool(&ev_early, 1000)
            .expect("write early spool");

        let merged = logger.merge_spool().expect("merge spool");
        assert_eq!(merged, 2);

        let lines: Vec<String> = fs::read_to_string(&cfg.log_path)
            .expect("read canonical log")
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(lines.len(), 2);
        let parsed0: LogEventV1 = serde_json::from_str(&lines[0]).expect("line 0 parse");
        let parsed1: LogEventV1 = serde_json::from_str(&lines[1]).expect("line 1 parse");
        assert_eq!(parsed0.ts, "2026-03-09T00:00:01Z");
        assert_eq!(parsed1.ts, "2026-03-09T00:00:05Z");

        let leftover: Vec<_> = fs::read_dir(&cfg.spool_dir)
            .expect("spool dir")
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            leftover.is_empty(),
            "spool files should be deleted after merge"
        );
    }

    #[test]
    fn merge_spool_recovers_stale_claiming_files() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };
        fs::create_dir_all(&cfg.spool_dir).expect("create spool dir");
        let stale_claiming = cfg.spool_dir.join("atm-44201-1000.claiming");
        let ev = make_event("2026-03-09T00:00:01Z");
        fs::write(
            &stale_claiming,
            format!("{}\n", serde_json::to_string(&ev).expect("serialize")),
        )
        .expect("write stale claiming");

        let logger = Logger::new(cfg.clone());
        let merged = logger.merge_spool().expect("merge spool");
        assert_eq!(merged, 1);
        assert!(!stale_claiming.exists());

        let log_content = fs::read_to_string(&cfg.log_path).expect("read log");
        let lines: Vec<_> = log_content.lines().collect();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn write_to_spool_creates_dir_and_appends() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };
        let logger = Logger::new(cfg);
        let ev = make_event("2026-03-09T00:00:01Z");
        let path1 = logger.write_to_spool(&ev, 1000).expect("spool write 1");
        let path2 = logger.write_to_spool(&ev, 1000).expect("spool write 2");
        assert_eq!(path1, path2);
        let spool_content = fs::read_to_string(path1).expect("read spool");
        let lines: Vec<_> = spool_content.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn rotate_log_files_max_files_zero_removes_base() {
        let tmp = TempDir::new().expect("temp dir");
        let base = tmp.path().join("atm.log.jsonl");
        fs::write(&base, "line\n").expect("write base");
        rotate_log_files(&base, 0).expect("rotate");
        assert!(!base.exists());
    }

    #[test]
    fn rotate_log_files_evicts_oldest_when_limit_reached() {
        let tmp = TempDir::new().expect("temp dir");
        let base = tmp.path().join("atm.log.jsonl");
        fs::write(&base, "base\n").expect("write base");
        fs::write(rotation_path(&base, 1), "one\n").expect("write .1");
        fs::write(rotation_path(&base, 2), "two\n").expect("write .2");

        rotate_log_files(&base, 2).expect("rotate");

        assert_eq!(
            fs::read_to_string(rotation_path(&base, 1)).expect("read .1"),
            "base\n"
        );
        assert_eq!(
            fs::read_to_string(rotation_path(&base, 2)).expect("read .2"),
            "one\n"
        );
        assert!(!rotation_path(&base, 3).exists());
    }

    #[test]
    fn socket_error_codes_match_contract() {
        assert_eq!(SOCKET_ERROR_VERSION_MISMATCH, "VERSION_MISMATCH");
        assert_eq!(SOCKET_ERROR_INVALID_PAYLOAD, "INVALID_PAYLOAD");
        assert_eq!(SOCKET_ERROR_INTERNAL_ERROR, "INTERNAL_ERROR");
    }

    #[test]
    fn emit_action_writes_schema_compatible_event() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
            retention_days: DEFAULT_RETENTION_DAYS,
        };
        let logger = Logger::new(cfg.clone());

        logger
            .emit_action(
                "sc-compose",
                "sc_compose::cli",
                "command_end",
                Some("success"),
                serde_json::json!({"code": 0}),
            )
            .expect("emit action");

        let lines: Vec<_> = fs::read_to_string(&cfg.log_path)
            .expect("read log")
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(lines.len(), 1);
        let parsed: LogEventV1 = serde_json::from_str(&lines[0]).expect("parse event");
        assert_eq!(parsed.source_binary, "sc-compose");
        assert_eq!(parsed.action, "command_end");
        assert_eq!(parsed.outcome.as_deref(), Some("success"));
        assert_eq!(parsed.fields.get("code").and_then(|v| v.as_u64()), Some(0));
    }

    #[test]
    #[serial]
    fn logger_emit_exports_to_http_collector_and_local_mirror() {
        let collector = TestCollector::start("200 OK");
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };

        unsafe {
            std::env::set_var("ATM_OTEL_ENABLED", "true");
            std::env::set_var("ATM_OTEL_ENDPOINT", &collector.endpoint);
        }

        let logger = Logger::new(cfg.clone());
        let mut event = new_log_event("atm", "command_success", "atm::config", "info");
        event.fields.insert(
            "command".to_string(),
            serde_json::Value::String("config".to_string()),
        );
        logger.emit(&event).expect("emit should succeed");

        let requests = collector.requests();
        assert_eq!(requests.len(), 1, "collector should receive one request");
        assert!(
            requests[0].starts_with("POST /v1/logs HTTP/1.1"),
            "collector request should target OTLP logs endpoint: {requests:?}"
        );
        assert!(
            requests[0].contains("\"command_success\""),
            "collector payload should include the emitted action: {requests:?}"
        );

        let canonical = fs::read_to_string(&cfg.log_path).expect("canonical log should exist");
        assert!(canonical.contains("\"command_success\""));
        let sidecar_path = default_otel_path(&cfg.log_path);
        let sidecar = fs::read_to_string(sidecar_path).expect("otel sidecar should exist");
        assert!(sidecar.contains("\"command_success\""));

        unsafe {
            std::env::remove_var("ATM_OTEL_ENABLED");
            std::env::remove_var("ATM_OTEL_ENDPOINT");
        }
    }

    #[test]
    #[serial]
    fn logger_emit_remains_fail_open_when_collector_returns_error() {
        let collector = TestCollector::start("503 Service Unavailable");
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };

        unsafe {
            std::env::set_var("ATM_OTEL_ENABLED", "true");
            std::env::set_var("ATM_OTEL_ENDPOINT", &collector.endpoint);
            std::env::set_var("ATM_OTEL_RETRY_MAX_ATTEMPTS", "0");
        }

        let logger = Logger::new(cfg.clone());
        let start = Instant::now();
        logger
            .emit(&new_log_event(
                "atm",
                "command_error",
                "atm::config",
                "error",
            ))
            .expect("emit should remain fail-open");

        assert!(
            start.elapsed() < Duration::from_secs(1),
            "collector outage should not block logging"
        );
        let requests = collector.requests();
        assert_eq!(
            requests.len(),
            1,
            "collector outage should still attempt one POST"
        );
        assert!(
            requests[0].contains("\"command_error\""),
            "collector outage payload should preserve the emitted action: {requests:?}"
        );

        let canonical = fs::read_to_string(&cfg.log_path).expect("canonical log should exist");
        assert!(canonical.contains("\"command_error\""));
        let sidecar_path = default_otel_path(&cfg.log_path);
        let sidecar = fs::read_to_string(sidecar_path).expect("otel sidecar should exist");
        assert!(sidecar.contains("\"command_error\""));

        unsafe {
            std::env::remove_var("ATM_OTEL_ENABLED");
            std::env::remove_var("ATM_OTEL_ENDPOINT");
            std::env::remove_var("ATM_OTEL_RETRY_MAX_ATTEMPTS");
        }
    }

    #[test]
    #[serial]
    fn otel_default_on_env_override_supported() {
        // SAFETY: test-scoped environment mutation.
        unsafe {
            std::env::remove_var("ATM_OTEL_ENABLED");
        }
        let default_cfg = OtelConfig::from_env();
        assert!(default_cfg.enabled, "OTel should be enabled by default");

        // SAFETY: test-scoped environment mutation.
        unsafe {
            std::env::set_var("ATM_OTEL_ENABLED", "false");
        }
        let disabled_cfg = OtelConfig::from_env();
        assert!(
            !disabled_cfg.enabled,
            "ATM_OTEL_ENABLED=false should disable exporter"
        );
        // SAFETY: cleanup after test.
        unsafe {
            std::env::remove_var("ATM_OTEL_ENABLED");
        }
    }

    #[test]
    fn emit_is_fail_open_when_otel_exporter_fails() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };
        let exporter = Arc::new(CountingExporter::with_failures(10));
        let logger = Logger::with_otel_exporter(
            cfg.clone(),
            OtelConfig {
                enabled: true,
                max_retries: 2,
                initial_backoff_ms: 0,
                max_backoff_ms: 0,
                ..OtelConfig::default()
            },
            exporter.clone(),
        );

        let event = new_log_event("atm", "send_message", "atm::send", "info");
        logger.emit(&event).expect("emit should not fail");

        let log_lines = fs::read_to_string(&cfg.log_path).expect("canonical log should exist");
        assert!(
            !log_lines.trim().is_empty(),
            "canonical log should be written"
        );
        assert_eq!(
            exporter.attempts.load(Ordering::SeqCst),
            3,
            "initial attempt + 2 retries"
        );
        assert!(
            exporter.records.lock().expect("records lock").is_empty(),
            "all export attempts should fail in this test"
        );
    }

    #[test]
    fn export_otel_best_effort_from_path_is_fail_open_when_export_fails() {
        let tmp = TempDir::new().expect("temp dir");
        let parent_file = tmp.path().join("not-a-directory");
        std::fs::write(&parent_file, "occupied").expect("create parent file");
        let log_path = parent_file.join("atm.log.jsonl");
        let event = new_log_event("atm-daemon", "register_hint", "atm_daemon::socket", "info");

        // Must not panic or propagate errors when exporter cannot create its output path.
        export_otel_best_effort_from_path(&log_path, &event);
    }

    #[test]
    fn otel_exporter_retries_then_succeeds() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };
        let exporter = Arc::new(CountingExporter::with_failures(2));
        let logger = Logger::with_otel_exporter(
            cfg,
            OtelConfig {
                enabled: true,
                max_retries: 4,
                initial_backoff_ms: 0,
                max_backoff_ms: 0,
                ..OtelConfig::default()
            },
            exporter.clone(),
        );

        let mut event = new_log_event("atm", "subagent.run", "atm::runtime", "info");
        event.team = Some("atm-dev".to_string());
        event.agent = Some("arch-ctm".to_string());
        event.runtime = Some("codex".to_string());
        event.session_id = Some("local:arch-ctm:123".to_string());
        event.trace_id = Some("trace-123".to_string());
        event.span_id = Some("span-456".to_string());
        event.subagent_id = Some("subagent-7".to_string());

        logger.emit(&event).expect("emit should succeed");
        assert_eq!(
            exporter.attempts.load(Ordering::SeqCst),
            3,
            "2 failures + 1 success"
        );
        assert_eq!(
            exporter.records.lock().expect("records lock").len(),
            1,
            "record should be exported after retries"
        );
    }

    #[test]
    fn producer_events_export_through_pipeline_with_counting_exporter() {
        let tmp = TempDir::new().expect("temp dir");
        let cfg = LogConfig {
            log_path: tmp.path().join("atm.log.jsonl"),
            spool_dir: tmp.path().join("log-spool"),
            level: LogLevel::Info,
            message_preview_enabled: false,
            max_bytes: DEFAULT_MAX_BYTES,
            max_files: DEFAULT_MAX_FILES,
            retention_days: DEFAULT_RETENTION_DAYS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        };
        let exporter = Arc::new(CountingExporter::with_failures(0));
        let logger = Logger::with_otel_exporter(
            cfg,
            OtelConfig {
                enabled: true,
                max_retries: 0,
                initial_backoff_ms: 0,
                max_backoff_ms: 0,
                ..OtelConfig::default()
            },
            exporter.clone(),
        );

        for (idx, source, action) in [
            (1u8, "atm", "send"),
            (2u8, "atm-daemon", "register_hint"),
            (3u8, "sc-composer", "compose"),
        ] {
            let mut event = new_log_event(source, action, "atm::test", "info");
            event.team = Some("atm-dev".to_string());
            event.agent = Some("arch-ctm".to_string());
            event.runtime = Some("codex".to_string());
            event.session_id = Some("sess-123".to_string());
            event.trace_id = Some("trace-123".to_string());
            event.span_id = Some(format!("span-{idx}"));
            logger.emit(&event).expect("emit should succeed");
        }

        let records = exporter.records.lock().expect("records lock");
        assert_eq!(records.len(), 3, "all producer events should export");
        assert_eq!(
            records.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
            vec![
                "send".to_string(),
                "register_hint".to_string(),
                "compose".to_string()
            ]
        );
    }

    #[test]
    fn export_otel_best_effort_is_public_and_fail_open() {
        let exporter = CountingExporter::with_failures(10);
        let event = new_log_event("atm", "send_message", "atm::send", "info");
        export_otel_best_effort(
            &event,
            &OtelConfig {
                enabled: true,
                max_retries: 2,
                initial_backoff_ms: 0,
                max_backoff_ms: 0,
                ..OtelConfig::default()
            },
            &exporter,
        );

        assert_eq!(
            exporter.attempts.load(Ordering::SeqCst),
            3,
            "initial attempt + 2 retries"
        );
    }

    #[test]
    fn otel_retry_backoff_is_bounded_by_max_backoff() {
        let sleeps = BACKOFF_SLEEPS_MS.get_or_init(|| Mutex::new(Vec::new()));
        sleeps.lock().expect("backoff sleeps lock").clear();

        let exporter = Arc::new(CountingExporter::with_failures(10));
        let event = new_log_event("atm", "send_message", "atm::send", "info");
        let exporters: Vec<Arc<dyn OtelExporter>> = vec![exporter.clone()];
        let err = export_otel_with_retry(
            &event,
            &OtelConfig {
                enabled: true,
                max_retries: 4,
                initial_backoff_ms: 5,
                max_backoff_ms: 12,
                ..OtelConfig::default()
            },
            &exporters,
            Path::new("/tmp/atm.log.jsonl"),
            record_sleep,
        )
        .expect_err("should return final export error");
        assert!(matches!(err, OtelError::ExportFailed(_)));

        let sleeps = sleeps.lock().expect("backoff sleeps lock").clone();
        assert_eq!(sleeps, vec![5, 10, 12, 12]);
        assert!(
            sleeps.iter().all(|v| *v <= 12),
            "sleep exceeded max_backoff"
        );
    }

    #[test]
    fn build_otel_record_requires_runtime_for_runtime_scoped_events() {
        let mut event = new_log_event("atm", "send_message", "atm::send", "info");
        event.team = Some("atm-dev".to_string());
        event.agent = Some("arch-ctm".to_string());
        event.session_id = Some("local:arch-ctm".to_string());
        // runtime intentionally missing

        let err = build_otel_record(&event).expect_err("runtime should be required");
        assert_eq!(err, OtelError::MissingRequiredField { field: "runtime" });
    }

    #[test]
    fn build_otel_record_requires_subagent_id_for_subagent_actions() {
        let mut event = new_log_event("atm", "subagent.run", "atm::runtime", "info");
        event.team = Some("atm-dev".to_string());
        event.agent = Some("arch-ctm".to_string());
        event.runtime = Some("codex".to_string());
        event.session_id = Some("local:arch-ctm".to_string());
        event.trace_id = Some("trace-123".to_string());
        event.span_id = Some("span-456".to_string());
        // subagent_id intentionally missing

        let err = build_otel_record(&event).expect_err("subagent_id should be required");
        assert_eq!(
            err,
            OtelError::MissingRequiredField {
                field: "subagent_id"
            }
        );
    }

    #[test]
    fn build_otel_record_requires_full_span_context_when_partial() {
        let mut event = new_log_event("atm", "send_message", "atm::send", "info");
        event.trace_id = Some("trace-123".to_string());
        // span_id intentionally missing
        let err = build_otel_record(&event).expect_err("partial span context should fail");
        assert_eq!(err, OtelError::InvalidSpanContext);
    }

    #[test]
    fn build_otel_record_includes_required_correlation_attributes() {
        let mut event = new_log_event("atm", "subagent.run", "atm::runtime", "info");
        event.team = Some("atm-dev".to_string());
        event.agent = Some("arch-ctm".to_string());
        event.runtime = Some("codex".to_string());
        event.session_id = Some("local:arch-ctm".to_string());
        event.trace_id = Some("trace-123".to_string());
        event.span_id = Some("span-456".to_string());
        event.subagent_id = Some("subagent-7".to_string());

        let record = build_otel_record(&event).expect("record should build");
        assert_eq!(record.trace_id.as_deref(), Some("trace-123"));
        assert_eq!(record.span_id.as_deref(), Some("span-456"));
        assert_eq!(
            record.attributes.get("team").and_then(|v| v.as_str()),
            Some("atm-dev")
        );
        assert_eq!(
            record.attributes.get("agent").and_then(|v| v.as_str()),
            Some("arch-ctm")
        );
        assert_eq!(
            record.attributes.get("runtime").and_then(|v| v.as_str()),
            Some("codex")
        );
        assert_eq!(
            record.attributes.get("session_id").and_then(|v| v.as_str()),
            Some("local:arch-ctm")
        );
        assert_eq!(
            record
                .attributes
                .get("subagent_id")
                .and_then(|v| v.as_str()),
            Some("subagent-7")
        );
    }

    #[test]
    fn build_otel_record_projects_event_fields_without_overwriting_core_attributes() {
        let mut event = new_log_event("sc-compose", "compose", "sc_compose::cli", "info");
        event.fields.insert(
            "runtime".to_string(),
            serde_json::Value::String("claude".to_string()),
        );
        event.fields.insert(
            "resolved_files".to_string(),
            serde_json::Value::Number(serde_json::Number::from(2)),
        );
        event.fields.insert(
            "action".to_string(),
            serde_json::Value::String("wrong".to_string()),
        );

        let record = build_otel_record(&event).expect("record should build");
        assert_eq!(
            record.attributes.get("runtime").and_then(|v| v.as_str()),
            Some("claude")
        );
        assert_eq!(
            record
                .attributes
                .get("resolved_files")
                .and_then(|v| v.as_u64()),
            Some(2)
        );
        assert_eq!(
            record.attributes.get("action").and_then(|v| v.as_str()),
            Some("compose")
        );
    }
}
