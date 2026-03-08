#![cfg(unix)]

use agent_team_mail_core::daemon_client::ensure_daemon_running;
use agent_team_mail_core::logging::{UnifiedLogMode, init_stderr_only, init_unified};
use agent_team_mail_core::logging_event::{LogEventV1, spool_dir};
use std::fs;
use std::os::unix::fs::PermissionsExt;
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
fn autostart_failure_logs_structured_event_with_stderr_tail_context() {
    let tmp = TempDir::new().expect("tempdir");
    let home = tmp.path().to_path_buf();

    let script_path = home.join("fake-daemon-fail.sh");
    let script = r#"#!/bin/sh
set -eu
echo "fatal: invalid plugin config" >&2
exit 42
"#;
    fs::write(&script_path, script).expect("write script");
    let mut perms = fs::metadata(&script_path)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("chmod script");

    let _home_guard = EnvGuard::set("ATM_HOME", &home.to_string_lossy());
    let _bin_guard = EnvGuard::set("ATM_DAEMON_BIN", &script_path.to_string_lossy());
    let _autostart_guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "1");

    let missing_socket = home.join(".claude/daemon/atm-daemon.sock");
    let spool = spool_dir(&home);
    let _guards = init_unified(
        "atm",
        UnifiedLogMode::ProducerFanIn {
            daemon_socket: missing_socket,
            fallback_spool_dir: spool.clone(),
        },
    )
    .unwrap_or_else(|_| init_stderr_only());

    let err = ensure_daemon_running().expect_err("autostart should fail for test script");
    let msg = err.to_string();
    assert!(
        msg.contains("stderr_tail="),
        "returned autostart error must include stderr tail: {msg}"
    );
    assert!(
        msg.contains("invalid plugin config"),
        "stderr tail must preserve daemon startup context: {msg}"
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let events = read_spool_events(&spool);
        if events.iter().any(|event| {
            event.action == "daemon_autostart_failure"
                && event
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("stderr_tail=")
                && event
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("invalid plugin config")
        }) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let events = read_spool_events(&spool);
    panic!(
        "expected daemon_autostart_failure with stderr_tail in structured logs; observed events: {:?}",
        events
            .iter()
            .map(|event| format!(
                "{} :: {}",
                event.action,
                event.error.clone().unwrap_or_default()
            ))
            .collect::<Vec<_>>()
    );
}
