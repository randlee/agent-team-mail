#![cfg(unix)]

use agent_team_mail_core::daemon_client::ensure_daemon_running;
use agent_team_mail_core::logging::{UnifiedLogMode, init_stderr_only, init_unified};
use agent_team_mail_core::logging_event::{LogEventV1, spool_dir};
use serial_test::serial;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[path = "support/daemon_process_guard.rs"]
#[allow(dead_code)]
mod daemon_process_guard;
#[path = "support/daemon_test_registry.rs"]
#[allow(dead_code)]
mod daemon_test_registry;
#[path = "support/env_guard.rs"]
#[allow(dead_code)]
mod env_guard;

use daemon_process_guard::DaemonProcessGuard;
use env_guard::EnvGuard;

fn read_spool_events(spool: &Path) -> Vec<LogEventV1> {
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

fn pid_alive(pid: i32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: signal 0 checks process existence without sending a signal.
    unsafe { kill(pid, 0) == 0 }
}

#[test]
#[serial]
fn autostart_failure_logs_structured_event_with_stderr_tail_context() {
    daemon_test_registry::sweep_stale_test_daemons();

    let tmp = TempDir::new().expect("tempdir");
    let home = tmp.path().to_path_buf();

    let script_path = home.join("fake-daemon-fail.sh");
    let leaked_pid_path = home.join("leaked-autostart-child.pid");
    let script = format!(
        "#!/bin/sh\nset -eu\nnohup sleep 30 >/dev/null 2>&1 &\nbgpid=$!\nprintf '%s\\n' \"$bgpid\" > \"{}\"\necho \"fatal: invalid plugin config\" >&2\nexit 42\n",
        leaked_pid_path.display()
    );
    fs::write(&script_path, script).expect("write script");
    let mut perms = fs::metadata(&script_path)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("chmod script");

    let _home_guard = EnvGuard::set("ATM_HOME", &home);
    let _os_home_guard = EnvGuard::set("HOME", &home);
    let _bin_guard = EnvGuard::set("ATM_DAEMON_BIN", &script_path);
    let _autostart_guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "1");
    let _shared_guard = EnvGuard::set("ATM_TEST_SHARED_DAEMON_ADMISSION", "1");

    let missing_socket = home.join(".atm/daemon/atm-daemon.sock");
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
    let leaked_pid_guard = DaemonProcessGuard::adopt_from_pid_file(
        &leaked_pid_path,
        &script_path,
        &home,
        Duration::from_secs(2),
    )
    .expect("autostart failure test should adopt leaked background child");
    let leaked_pid = leaked_pid_guard.pid();
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
            drop(leaked_pid_guard);
            assert!(
                !pid_alive(leaked_pid as i32),
                "adopted leaked autostart child must be reaped before test exit"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    drop(leaked_pid_guard);
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
