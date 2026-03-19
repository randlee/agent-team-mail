use sc_observability::{MetricKind, MetricRecord, OtelConfig, export_metric_records_best_effort};
use serial_test::serial;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

struct MetricCollector {
    endpoint: String,
    requests: Arc<Mutex<Vec<String>>>,
    shutdown: Arc<AtomicBool>,
    wake_addr: String,
    join: Option<thread::JoinHandle<()>>,
}

impl MetricCollector {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind collector");
        listener
            .set_nonblocking(true)
            .expect("collector nonblocking");
        let addr = listener.local_addr().expect("collector addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&requests);
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_flag = Arc::clone(&shutdown);
        let join = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !shutdown_flag.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if shutdown_flag.load(Ordering::SeqCst) {
                            break;
                        }
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
                        stream
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
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
            shutdown,
            wake_addr: addr.to_string(),
            join: Some(join),
        }
    }

    fn wait_for_request(&self) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut requests = self.requests.lock().expect("collector lock").clone();
        while requests.is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
            requests = self.requests.lock().expect("collector lock").clone();
        }
        requests
    }
}

impl Drop for MetricCollector {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.wake_addr);
        if let Some(join) = self.join.take() {
            join.join().expect("collector thread should join");
        }
    }
}

#[test]
#[serial]
fn metric_record_exports_to_otlp_http_collector() {
    let collector = MetricCollector::start();
    let record = MetricRecord {
        timestamp: "2026-03-18T08:00:00Z".to_string(),
        team: Some("atm-dev".to_string()),
        agent: Some("arch-ctm".to_string()),
        runtime: Some("codex".to_string()),
        session_id: Some("session-123".to_string()),
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

    let config = OtelConfig {
        enabled: true,
        endpoint: Some(collector.endpoint.clone()),
        ..OtelConfig::default()
    };

    export_metric_records_best_effort(&[record], &config);

    let requests = collector.wait_for_request();
    assert_eq!(
        requests.len(),
        1,
        "collector should receive one metric request"
    );
    assert!(
        requests[0].starts_with("POST /v1/metrics HTTP/1.1"),
        "collector request should target OTLP metrics endpoint: {requests:?}"
    );
    assert!(requests[0].contains("\"atm_messages_total\""));
    assert!(requests[0].contains("\"service.name\""));
    assert!(requests[0].contains("\"atm-dev\""));
    assert!(requests[0].contains("\"arch-ctm\""));
}
