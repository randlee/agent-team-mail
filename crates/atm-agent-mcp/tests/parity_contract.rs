use std::fs;
use std::path::PathBuf;

fn contract_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parity/contract")
}

fn read_json(path: &std::path::Path) -> serde_json::Value {
    let raw = fs::read_to_string(path).expect("fixture file must be readable");
    serde_json::from_str(&raw).expect("fixture must be valid JSON")
}

#[test]
fn parity_contract_all_transport_fixtures_have_event_type() {
    let base = contract_fixture_dir();
    for transport in ["mcp", "cli-json", "app-server"] {
        let path = base.join(transport).join("event.sample.json");
        let payload = read_json(&path);
        let event_type = payload.pointer("/params/type").and_then(|v| v.as_str());
        assert!(
            event_type.is_some(),
            "contract fixture {} must include /params/type",
            path.display()
        );
    }
}

#[test]
fn parity_contract_stream_error_fixture_has_message_and_thread_id() {
    let path = contract_fixture_dir()
        .join("app-server")
        .join("event.sample.json");
    let payload = read_json(&path);
    assert_eq!(
        payload.pointer("/params/type").and_then(|v| v.as_str()),
        Some("stream_error")
    );
    assert!(
        payload
            .pointer("/params/message")
            .and_then(|v| v.as_str())
            .is_some(),
        "stream_error fixture must include message"
    );
    assert!(
        payload
            .pointer("/params/threadId")
            .and_then(|v| v.as_str())
            .is_some(),
        "stream_error fixture must include threadId"
    );
}
