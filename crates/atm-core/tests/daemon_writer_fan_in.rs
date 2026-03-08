use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_core::logging::{
    RotationConfig, UnifiedLogMode, init_stderr_only, init_unified, producer_sender,
};
use agent_team_mail_core::logging_event::{LogEventV1, spool_dir};
use std::fs;
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct EnvGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        // SAFETY: test-scoped env mutation guarded by RAII restore in Drop.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: test-scoped env restore.
        unsafe {
            if let Some(old) = &self.old {
                std::env::set_var(self.key, old);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

fn read_spool_events(spool: &std::path::Path) -> Vec<LogEventV1> {
    if !spool.exists() {
        return Vec::new();
    }
    let mut events = Vec::new();
    let entries = match fs::read_dir(spool) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| x == "jsonl")
            != Some(true)
        {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines().filter(|line| !line.trim().is_empty()) {
            if let Ok(event) = serde_json::from_str::<LogEventV1>(line) {
                events.push(event);
            }
        }
    }
    events
}

#[test]
fn daemon_writer_mode_wires_producer_sender_and_spools_emitted_event() {
    let tmp = TempDir::new().expect("tempdir");
    let _home_guard = EnvGuard::set("ATM_HOME", &tmp.path().to_string_lossy());

    let log_path = tmp.path().join("logs/atm-daemon.jsonl");
    let _guards = init_unified(
        "atm-daemon",
        UnifiedLogMode::DaemonWriter {
            file_path: log_path,
            rotation: RotationConfig::default(),
        },
    )
    .unwrap_or_else(|_| init_stderr_only());

    assert!(
        producer_sender().is_some(),
        "DaemonWriter must register a producer sender for daemon-side emit_event calls"
    );

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm-daemon",
        action: "daemon_writer_probe",
        team: Some("atm-dev".to_string()),
        ..Default::default()
    });

    let spool = spool_dir(tmp.path());
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let events = read_spool_events(&spool);
        if events
            .iter()
            .any(|event| event.action == "daemon_writer_probe")
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let events = read_spool_events(&spool);
    panic!(
        "expected daemon_writer_probe in spool; saw actions: {:?}",
        events
            .iter()
            .map(|event| event.action.as_str())
            .collect::<Vec<_>>()
    );
}
