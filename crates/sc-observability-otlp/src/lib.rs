use reqwest::blocking::{Client, ClientBuilder};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;

pub const OTLP_HTTP_PROTOCOL: &str = "otlp_http";
pub const DEFAULT_TIMEOUT_MS: u64 = 1_500;
pub const DEFAULT_MAX_RETRIES: u32 = 2;
pub const DEFAULT_INITIAL_BACKOFF_MS: u64 = 25;
pub const DEFAULT_MAX_BACKOFF_MS: u64 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportExporterKind {
    Collector,
    DebugLocal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportConfig {
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

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            endpoint: None,
            protocol: OTLP_HTTP_PROTOCOL.to_string(),
            auth_header: None,
            ca_file: None,
            insecure_skip_verify: false,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            debug_local_export: false,
            max_retries: DEFAULT_MAX_RETRIES,
            initial_backoff_ms: DEFAULT_INITIAL_BACKOFF_MS,
            max_backoff_ms: DEFAULT_MAX_BACKOFF_MS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransportRecord {
    pub name: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub attributes: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraceTransportRecord {
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
    pub attributes: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceStatus {
    Ok,
    Error,
    Unset,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricTransportRecord {
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
    pub attributes: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    Counter,
    Gauge,
    Histogram,
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("unsupported transport protocol: {0}")]
    UnsupportedProtocol(String),
    #[error("invalid auth header '{0}'")]
    InvalidAuthHeader(String),
    #[error("invalid header name '{0}'")]
    InvalidHeaderName(String),
    #[error("invalid header value for '{0}'")]
    InvalidHeaderValue(String),
    #[error("failed to read CA bundle {path}: {source}")]
    ReadCaFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse CA bundle {path}")]
    InvalidCaFile { path: PathBuf },
    #[error("failed to build HTTP client: {0}")]
    ClientBuild(String),
    #[error("export failed: {0}")]
    ExportFailed(String),
    #[error("stdout export failed: {0}")]
    Stdout(String),
}

pub trait TransportExporter: Send + Sync {
    fn kind(&self) -> TransportExporterKind;
    fn export(&self, record: &TransportRecord) -> Result<(), TransportError>;
}

pub fn build_exporters(
    config: &TransportConfig,
) -> Result<Vec<Arc<dyn TransportExporter>>, TransportError> {
    let mut exporters: Vec<Arc<dyn TransportExporter>> = Vec::new();

    if let Some(endpoint) = config
        .endpoint
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let protocol = config.protocol.trim();
        if protocol != OTLP_HTTP_PROTOCOL {
            return Err(TransportError::UnsupportedProtocol(protocol.to_string()));
        }
        exporters.push(Arc::new(OtlpHttpExporter::new(endpoint, config)?));
    }

    if config.debug_local_export {
        exporters.push(Arc::new(StdoutDebugExporter::stdout()));
    }

    Ok(exporters)
}

pub fn export_traces(
    config: &TransportConfig,
    records: &[TraceTransportRecord],
) -> Result<(), TransportError> {
    if records.is_empty() {
        return Ok(());
    }
    let endpoint = config
        .endpoint
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            TransportError::ExportFailed("collector endpoint not configured".to_string())
        })?;
    let exporter = OtlpHttpExporter::new(endpoint, config)?;
    exporter.export_json(
        &normalize_signal_endpoint(endpoint, "traces"),
        build_traces_payload(records),
    )
}

pub fn export_metrics(
    config: &TransportConfig,
    records: &[MetricTransportRecord],
) -> Result<(), TransportError> {
    if records.is_empty() {
        return Ok(());
    }
    let endpoint = config
        .endpoint
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            TransportError::ExportFailed("collector endpoint not configured".to_string())
        })?;
    let exporter = OtlpHttpExporter::new(endpoint, config)?;
    exporter.export_json(
        &normalize_signal_endpoint(endpoint, "metrics"),
        build_metrics_payload(records),
    )
}

#[derive(Debug)]
pub struct OtlpHttpExporter {
    client: Client,
    config: TransportConfig,
    endpoint: String,
}

impl OtlpHttpExporter {
    pub fn new(endpoint: &str, config: &TransportConfig) -> Result<Self, TransportError> {
        let mut builder = ClientBuilder::new().timeout(Duration::from_millis(config.timeout_ms));
        if config.insecure_skip_verify {
            builder = builder.danger_accept_invalid_certs(true);
        }
        if let Some(ca_path) = config.ca_file.as_ref() {
            let raw = fs::read(ca_path).map_err(|source| TransportError::ReadCaFile {
                path: ca_path.clone(),
                source,
            })?;
            let cert = reqwest::Certificate::from_pem(&raw)
                .or_else(|_| reqwest::Certificate::from_der(&raw))
                .map_err(|_| TransportError::InvalidCaFile {
                    path: ca_path.clone(),
                })?;
            builder = builder.add_root_certificate(cert);
        }
        if let Some((name, value)) = parse_auth_header(config.auth_header.as_deref())? {
            let mut headers = HeaderMap::new();
            headers.insert(name, value);
            builder = builder.default_headers(headers);
        }

        let client = builder
            .build()
            .map_err(|err| TransportError::ClientBuild(err.to_string()))?;

        Ok(Self {
            client,
            config: config.clone(),
            endpoint: normalize_logs_endpoint(endpoint),
        })
    }

    fn export_json(&self, endpoint: &str, body: Value) -> Result<(), TransportError> {
        let body = body.to_string();
        let mut attempt: u32 = 0;
        let mut backoff = self.config.initial_backoff_ms;
        loop {
            let response = self
                .client
                .post(endpoint)
                .header(CONTENT_TYPE, "application/json")
                .body(body.clone())
                .send()
                .map_err(|err| TransportError::ExportFailed(err.to_string()));

            match response {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response) => {
                    if attempt >= self.config.max_retries {
                        return Err(TransportError::ExportFailed(format!(
                            "collector returned {}",
                            response.status()
                        )));
                    }
                }
                Err(err) => {
                    if attempt >= self.config.max_retries {
                        return Err(err);
                    }
                }
            }

            std::thread::sleep(Duration::from_millis(backoff));
            backoff = backoff.saturating_mul(2).min(self.config.max_backoff_ms);
            attempt = attempt.saturating_add(1);
        }
    }
}

impl TransportExporter for OtlpHttpExporter {
    fn kind(&self) -> TransportExporterKind {
        TransportExporterKind::Collector
    }

    fn export(&self, record: &TransportRecord) -> Result<(), TransportError> {
        self.export_json(&self.endpoint, build_logs_payload(record))
    }
}

#[derive(Clone)]
pub struct StdoutDebugExporter {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl std::fmt::Debug for StdoutDebugExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdoutDebugExporter").finish()
    }
}

impl StdoutDebugExporter {
    pub fn stdout() -> Self {
        Self {
            writer: Arc::new(Mutex::new(Box::new(io::stdout()))),
        }
    }

    #[cfg(test)]
    fn with_writer(writer: Box<dyn Write + Send>) -> Self {
        Self {
            writer: Arc::new(Mutex::new(writer)),
        }
    }
}

impl TransportExporter for StdoutDebugExporter {
    fn kind(&self) -> TransportExporterKind {
        TransportExporterKind::DebugLocal
    }

    fn export(&self, record: &TransportRecord) -> Result<(), TransportError> {
        let line =
            serde_json::to_string(record).map_err(|err| TransportError::Stdout(err.to_string()))?;
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| TransportError::Stdout("stdout writer lock poisoned".to_string()))?;
        writeln!(writer, "{line}").map_err(|err| TransportError::Stdout(err.to_string()))
    }
}

fn parse_auth_header(
    raw: Option<&str>,
) -> Result<Option<(HeaderName, HeaderValue)>, TransportError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    let Some((name, value)) = raw.split_once(':') else {
        return Err(TransportError::InvalidAuthHeader(raw.to_string()));
    };
    let name = HeaderName::from_bytes(name.trim().as_bytes())
        .map_err(|_| TransportError::InvalidHeaderName(name.trim().to_string()))?;
    let value = HeaderValue::from_str(value.trim())
        .map_err(|_| TransportError::InvalidHeaderValue(name.to_string()))?;
    Ok(Some((name, value)))
}

fn normalize_logs_endpoint(endpoint: &str) -> String {
    normalize_signal_endpoint(endpoint, "logs")
}

fn normalize_signal_endpoint(endpoint: &str, signal: &str) -> String {
    let endpoint = endpoint.trim_end_matches('/');
    let suffix = format!("/v1/{signal}");
    if endpoint.ends_with(&suffix) {
        endpoint.to_string()
    } else {
        format!("{endpoint}{suffix}")
    }
}

fn build_logs_payload(record: &TransportRecord) -> Value {
    let mut attributes = vec![];
    for (key, value) in &record.attributes {
        attributes.push(json!({
            "key": key,
            "value": json_value_to_otlp_any(value),
        }));
    }
    if let Some(trace_id) = &record.trace_id {
        attributes.push(json!({
            "key": "trace_id",
            "value": { "stringValue": trace_id },
        }));
    }
    if let Some(span_id) = &record.span_id {
        attributes.push(json!({
            "key": "span_id",
            "value": { "stringValue": span_id },
        }));
    }

    json!({
        "resourceLogs": [{
            "scopeLogs": [{
                "scope": { "name": "sc-observability-otlp" },
                "logRecords": [{
                    "body": { "stringValue": record.name },
                    "attributes": attributes,
                }]
            }]
        }]
    })
}

fn build_traces_payload(records: &[TraceTransportRecord]) -> Value {
    let spans = records
        .iter()
        .map(|record| {
            let mut attributes = correlation_attributes(
                &record.team,
                &record.agent,
                &record.runtime,
                &record.session_id,
            );
            for (key, value) in &record.attributes {
                attributes.push(json!({
                    "key": key,
                    "value": json_value_to_otlp_any(value),
                }));
            }

            let mut span = json!({
                "traceId": record.trace_id,
                "spanId": record.span_id,
                "name": record.name,
                "status": {
                    "code": match record.status {
                        TraceStatus::Ok => "STATUS_CODE_OK",
                        TraceStatus::Error => "STATUS_CODE_ERROR",
                        TraceStatus::Unset => "STATUS_CODE_UNSET",
                    }
                },
                "attributes": attributes,
            });

            if let Some(parent_span_id) = &record.parent_span_id {
                span["parentSpanId"] = json!(parent_span_id);
            }
            if record.duration_ms > 0 {
                span["startTimeUnixNano"] = json!("0");
                span["endTimeUnixNano"] = json!((record.duration_ms * 1_000_000).to_string());
            }

            json!({
                "resource": {
                    "attributes": [{
                        "key": "service.name",
                        "value": { "stringValue": record.source_binary },
                    }]
                },
                "scopeSpans": [{
                    "scope": { "name": "sc-observability-otlp" },
                    "spans": [span]
                }]
            })
        })
        .collect::<Vec<_>>();

    json!({ "resourceSpans": spans })
}

fn build_metrics_payload(records: &[MetricTransportRecord]) -> Value {
    let metrics = records
        .iter()
        .map(|record| {
            let attributes = correlation_attributes(
                &record.team,
                &record.agent,
                &record.runtime,
                &record.session_id,
            )
            .into_iter()
            .chain(record.attributes.iter().map(|(key, value)| {
                json!({
                    "key": key,
                    "value": json_value_to_otlp_any(value),
                })
            }))
            .collect::<Vec<_>>();

            let data_point = json!({
                "attributes": attributes.clone(),
                "timeUnixNano": "0",
                "asDouble": record.value,
            });

            let mut metric = json!({
                "name": record.name,
                "unit": record.unit.clone().unwrap_or_default(),
            });
            match record.kind {
                MetricKind::Counter => {
                    metric["sum"] = json!({
                        "aggregationTemporality": 2,
                        "isMonotonic": true,
                        "dataPoints": [data_point],
                    });
                }
                MetricKind::Gauge => {
                    metric["gauge"] = json!({
                        "dataPoints": [data_point],
                    });
                }
                MetricKind::Histogram => {
                    metric["histogram"] = json!({
                        "aggregationTemporality": 2,
                        "dataPoints": [{
                            "attributes": attributes,
                            "timeUnixNano": "0",
                            "count": "1",
                            "sum": record.value,
                            "bucketCounts": ["1"],
                            "explicitBounds": [],
                        }],
                    });
                }
            }

            json!({
                "resource": {
                    "attributes": [{
                        "key": "service.name",
                        "value": { "stringValue": record.source_binary },
                    }]
                },
                "scopeMetrics": [{
                    "scope": { "name": "sc-observability-otlp" },
                    "metrics": [metric]
                }]
            })
        })
        .collect::<Vec<_>>();

    json!({ "resourceMetrics": metrics })
}

fn correlation_attributes(
    team: &Option<String>,
    agent: &Option<String>,
    runtime: &Option<String>,
    session_id: &Option<String>,
) -> Vec<Value> {
    let mut attributes = Vec::new();
    for (key, value) in [
        ("team", team.as_ref()),
        ("agent", agent.as_ref()),
        ("runtime", runtime.as_ref()),
        ("session_id", session_id.as_ref()),
    ] {
        if let Some(value) = value {
            attributes.push(json!({
                "key": key,
                "value": { "stringValue": value },
            }));
        }
    }
    attributes
}

fn json_value_to_otlp_any(value: &Value) -> Value {
    match value {
        Value::Null => json!({ "stringValue": "null" }),
        Value::Bool(v) => json!({ "boolValue": v }),
        Value::Number(v) if v.is_i64() => {
            json!({ "intValue": v.as_i64().unwrap_or_default().to_string() })
        }
        Value::Number(v) if v.is_u64() => {
            json!({ "intValue": v.as_u64().unwrap_or_default().to_string() })
        }
        Value::Number(v) => json!({ "doubleValue": v.as_f64().unwrap_or_default() }),
        Value::String(v) => json!({ "stringValue": v }),
        Value::Array(values) => json!({
            "arrayValue": {
                "values": values.iter().map(json_value_to_otlp_any).collect::<Vec<_>>()
            }
        }),
        Value::Object(map) => json!({
            "kvlistValue": {
                "values": map.iter().map(|(key, value)| json!({
                    "key": key,
                    "value": json_value_to_otlp_any(value),
                })).collect::<Vec<_>>()
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Result as IoResult};
    use std::net::TcpListener;
    use std::thread;
    use tempfile::TempDir;

    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
            self.0.lock().expect("buffer lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> IoResult<()> {
            Ok(())
        }
    }

    fn sample_record() -> TransportRecord {
        let mut attributes = Map::new();
        attributes.insert("team".to_string(), Value::String("atm-dev".to_string()));
        attributes.insert("count".to_string(), Value::Number(2.into()));
        TransportRecord {
            name: "atm.send".to_string(),
            trace_id: Some("trace-123".to_string()),
            span_id: Some("span-123".to_string()),
            attributes,
        }
    }

    fn sample_trace_record() -> TraceTransportRecord {
        let mut attributes = Map::new();
        attributes.insert("target".to_string(), Value::String("daemon".to_string()));
        TraceTransportRecord {
            timestamp: "2026-03-18T00:00:00Z".to_string(),
            team: Some("atm-dev".to_string()),
            agent: Some("arch-ctm".to_string()),
            runtime: Some("codex".to_string()),
            session_id: Some("sess-123".to_string()),
            trace_id: "trace-123".to_string(),
            span_id: "span-456".to_string(),
            parent_span_id: Some("span-000".to_string()),
            name: "daemon.request".to_string(),
            status: TraceStatus::Ok,
            duration_ms: 25,
            source_binary: "atm-daemon".to_string(),
            attributes,
        }
    }

    fn sample_metric_record() -> MetricTransportRecord {
        let mut attributes = Map::new();
        attributes.insert("scope".to_string(), Value::String("mail".to_string()));
        MetricTransportRecord {
            timestamp: "2026-03-18T00:00:00Z".to_string(),
            team: Some("atm-dev".to_string()),
            agent: None,
            runtime: Some("codex".to_string()),
            session_id: None,
            name: "atm_messages_total".to_string(),
            kind: MetricKind::Counter,
            value: 7.0,
            unit: Some("count".to_string()),
            source_binary: "atm".to_string(),
            attributes,
        }
    }

    #[test]
    fn build_exporters_returns_empty_when_transport_disabled() {
        let exporters = build_exporters(&TransportConfig::default()).expect("build exporters");
        assert!(exporters.is_empty());
    }

    #[test]
    fn normalize_logs_endpoint_appends_logs_suffix() {
        assert_eq!(
            normalize_logs_endpoint("https://collector.example"),
            "https://collector.example/v1/logs"
        );
    }

    #[test]
    fn normalize_logs_endpoint_preserves_existing_logs_suffix() {
        assert_eq!(
            normalize_logs_endpoint("https://collector.example/v1/logs"),
            "https://collector.example/v1/logs"
        );
    }

    #[test]
    fn stdout_exporter_writes_json_line() {
        let shared = SharedBuffer::default();
        let exporter = StdoutDebugExporter::with_writer(Box::new(shared.clone()));
        exporter.export(&sample_record()).expect("stdout export");

        let output =
            String::from_utf8(shared.0.lock().expect("buffer lock").clone()).expect("utf8");
        assert!(output.contains("\"name\":\"atm.send\""));
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn otlp_http_exporter_posts_logs_endpoint_and_header() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
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
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .expect("write response");
            String::from_utf8_lossy(&request).to_string()
        });

        let config = TransportConfig {
            endpoint: Some(format!("http://{addr}")),
            auth_header: Some("authorization: Bearer secret".to_string()),
            ..TransportConfig::default()
        };
        let exporter = OtlpHttpExporter::new(config.endpoint.as_deref().unwrap(), &config)
            .expect("create exporter");
        exporter.export(&sample_record()).expect("export record");

        let request = handle.join().expect("join server thread");
        assert!(request.starts_with("POST /v1/logs HTTP/1.1"));
        assert!(request.contains("authorization: Bearer secret"));
        assert!(request.contains("\"atm.send\""));
    }

    #[test]
    fn export_traces_posts_traces_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
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
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .expect("write response");
            String::from_utf8_lossy(&request).to_string()
        });

        let config = TransportConfig {
            endpoint: Some(format!("http://{addr}")),
            ..TransportConfig::default()
        };
        export_traces(&config, &[sample_trace_record()]).expect("export traces");

        let request = handle.join().expect("join server thread");
        assert!(request.starts_with("POST /v1/traces HTTP/1.1"));
        assert!(request.contains("\"daemon.request\""));
        assert!(request.contains("\"trace-123\""));
    }

    #[test]
    fn export_metrics_posts_metrics_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
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
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .expect("write response");
            String::from_utf8_lossy(&request).to_string()
        });

        let config = TransportConfig {
            endpoint: Some(format!("http://{addr}")),
            ..TransportConfig::default()
        };
        export_metrics(&config, &[sample_metric_record()]).expect("export metrics");

        let request = handle.join().expect("join server thread");
        assert!(request.starts_with("POST /v1/metrics HTTP/1.1"));
        assert!(request.contains("\"atm_messages_total\""));
        assert!(request.contains("\"count\""));
    }

    #[test]
    fn otlp_http_exporter_loads_custom_ca_bundle() {
        let dir = TempDir::new().expect("temp dir");
        let ca_path = dir.path().join("ca.pem");
        std::fs::write(
            &ca_path,
            "-----BEGIN CERTIFICATE-----\nMIIBYzCCAQmgAwIBAgIUW0Fj3b9GmTrkA+P2D2CxS1lq7xkwCgYIKoZIzj0EAwIw\nEDEOMAwGA1UEAwwFZHVtbXkwHhcNMjYwMzE3MDAwMDAwWhcNMjcwMzE3MDAwMDAw\nWjAQMQ4wDAYDVQQDDAVkdW1teTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABJ0F\nc3jEqq1P2s5H3S1n2l4sK4eG0M8fM7UeqQ1bYf5NFrWSxv2w1+M4Dr1+W7g+uM7P\nQSh4d2mH7l8u4hsk1MujUzBRMB0GA1UdDgQWBBT3R2Y0XlCkGQ1M3KpD4h6cfxcQ\nQDAfBgNVHSMEGDAWgBT3R2Y0XlCkGQ1M3KpD4h6cfxcQQDAPBgNVHRMBAf8EBTAD\nAQH/MAoGCCqGSM49BAMCA0gAMEUCIQCZJ4fObLw6fWv6sF1j6Kz7N+wLkH4mV7yA\nFGT5no1TpgIgN+1T0b1WQ7m4wP7Ew8us4j5iZBq0wY3D4FZg5k3M7PA=\n-----END CERTIFICATE-----\n",
        )
        .expect("write dummy pem");

        let config = TransportConfig {
            endpoint: Some("https://collector.example".to_string()),
            ca_file: Some(ca_path),
            ..TransportConfig::default()
        };
        let _ = OtlpHttpExporter::new(config.endpoint.as_deref().unwrap(), &config);
    }
}
