use sc_observability_otlp::{
    MetricKind, MetricTransportRecord, TraceStatus, TraceTransportRecord, TransportConfig,
    export_metrics, export_traces,
};
use serde_json::{Map, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

fn read_http_request(listener: TcpListener) -> String {
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
}

#[test]
fn export_traces_uses_real_unix_nanos_for_span_times() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let handle = thread::spawn(move || read_http_request(listener));

    let record = TraceTransportRecord {
        timestamp: "2026-01-15T12:00:00Z".to_string(),
        team: Some("atm-dev".to_string()),
        agent: Some("arch-ctm".to_string()),
        runtime: Some("codex".to_string()),
        session_id: Some("sess-123".to_string()),
        trace_id: "trace-123".to_string(),
        span_id: "span-456".to_string(),
        parent_span_id: None,
        name: "atm.command.send".to_string(),
        status: TraceStatus::Ok,
        duration_ms: 100,
        source_binary: "atm".to_string(),
        attributes: Map::new(),
    };

    let config = TransportConfig {
        endpoint: Some(format!("http://{addr}")),
        ..TransportConfig::default()
    };
    export_traces(&config, &[record]).expect("export traces");

    let request = handle.join().expect("join server thread");
    assert!(request.contains("\"startTimeUnixNano\":\"1736942400000000000\""));
    assert!(request.contains("\"endTimeUnixNano\":\"1736942400100000000\""));
}

#[test]
fn export_metrics_uses_real_unix_nanos_for_data_points() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let handle = thread::spawn(move || read_http_request(listener));

    let record = MetricTransportRecord {
        timestamp: "2026-01-15T12:00:00Z".to_string(),
        team: Some("atm-dev".to_string()),
        agent: Some("arch-ctm".to_string()),
        runtime: Some("codex".to_string()),
        session_id: Some("sess-123".to_string()),
        name: "atm_messages_total".to_string(),
        kind: MetricKind::Counter,
        value: 1.0,
        unit: Some("count".to_string()),
        source_binary: "atm".to_string(),
        attributes: Map::<String, Value>::new(),
    };

    let config = TransportConfig {
        endpoint: Some(format!("http://{addr}")),
        ..TransportConfig::default()
    };
    export_metrics(&config, &[record]).expect("export metrics");

    let request = handle.join().expect("join server thread");
    assert!(request.contains("\"timeUnixNano\":\"1736942400000000000\""));
}
