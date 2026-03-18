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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportConfig {
    pub endpoint: Option<String>,
    pub protocol: String,
    pub auth_header: Option<String>,
    pub ca_file: Option<PathBuf>,
    pub insecure_skip_verify: bool,
    pub timeout_ms: u64,
    pub debug_local_export: bool,
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

#[derive(Debug)]
pub struct OtlpHttpExporter {
    client: Client,
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
            endpoint: normalize_logs_endpoint(endpoint),
        })
    }
}

impl TransportExporter for OtlpHttpExporter {
    fn export(&self, record: &TransportRecord) -> Result<(), TransportError> {
        let body = build_logs_payload(record);
        let response = self
            .client
            .post(&self.endpoint)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .map_err(|err| TransportError::ExportFailed(err.to_string()))?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(TransportError::ExportFailed(format!(
                "collector returned {}",
                response.status()
            )))
        }
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
    let endpoint = endpoint.trim_end_matches('/');
    if endpoint.ends_with("/v1/logs") {
        endpoint.to_string()
    } else {
        format!("{endpoint}/v1/logs")
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

    #[test]
    fn build_exporters_returns_empty_when_transport_disabled() {
        let exporters = build_exporters(&TransportConfig::default()).expect("build exporters");
        assert!(exporters.is_empty());
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
