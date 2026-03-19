use agent_team_mail_core::logging_event::new_log_event;
use sc_observability::export_otel_best_effort_from_path;
use serial_test::serial;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct LogCollector {
    endpoint: String,
    requests: Arc<Mutex<Vec<String>>>,
    shutdown: Arc<AtomicBool>,
    wake_addr: String,
    join: Option<thread::JoinHandle<()>>,
}

impl LogCollector {
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
                        stream
                            .set_nonblocking(false)
                            .expect("accepted stream should block for request body");
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

impl Drop for LogCollector {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.wake_addr);
        if let Some(join) = self.join.take() {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !join.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            assert!(
                join.is_finished(),
                "collector thread should finish within 30s after shutdown"
            );
            join.join().expect("collector thread should join");
        }
    }
}

#[test]
#[serial]
fn log_event_exports_to_otlp_http_collector_with_service_name() {
    let collector = LogCollector::start();
    let temp = TempDir::new().expect("temp dir");
    let log_path = temp.path().join("atm.log.jsonl");
    let _enabled = unsafe_env::set("ATM_OTEL_ENABLED", "true");
    let _endpoint = unsafe_env::set("ATM_OTEL_ENDPOINT", &collector.endpoint);

    let mut event = new_log_event("atm", "command_success", "atm::config", "info");
    event.team = Some("atm-dev".to_string());
    event.agent = Some("arch-ctm".to_string());
    event.runtime = Some("codex".to_string());
    event.session_id = Some("session-123".to_string());
    event.fields.insert(
        "command".to_string(),
        serde_json::Value::String("config".to_string()),
    );

    export_otel_best_effort_from_path(&log_path, &event);

    let requests = collector.wait_for_request();
    assert_eq!(
        requests.len(),
        1,
        "collector should receive one log request"
    );
    assert!(
        requests[0].starts_with("POST /v1/logs HTTP/1.1"),
        "collector request should target OTLP logs endpoint: {requests:?}"
    );
    assert!(requests[0].contains("\"service.name\""));
    assert!(requests[0].contains("\"service_name\""));
    assert!(requests[0].contains("\"atm\""));
    assert!(requests[0].contains("\"session-123\""));
}

mod unsafe_env {
    pub struct Guard {
        key: &'static str,
        old: Option<String>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            match &self.old {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    pub fn set(key: &'static str, value: &str) -> Guard {
        let old = std::env::var(key).ok();
        unsafe { std::env::set_var(key, value) };
        Guard { key, old }
    }
}
