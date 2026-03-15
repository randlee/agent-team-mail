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

struct AutostartPidGuard {
    pid: u32,
}

impl AutostartPidGuard {
    fn adopt_from_pid_file(pid_path: &Path, timeout: Duration) -> Option<Self> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(raw) = fs::read_to_string(pid_path)
                && let Ok(pid) = raw.trim().parse::<u32>()
                && pid > 1
                && pid_alive(pid as i32)
            {
                return Some(Self { pid });
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
    }

    fn pid(&self) -> u32 {
        self.pid
    }
}

impl Drop for AutostartPidGuard {
    fn drop(&mut self) {
        cleanup_pid(self.pid);
    }
}

fn cleanup_pid(pid: u32) {
    send_signal(pid as i32, 15);
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if !pid_alive(pid as i32) {
            reap_child_pid_best_effort(pid as i32);
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    send_signal(pid as i32, 9);
    reap_child_pid_best_effort(pid as i32);
}

fn pid_alive(pid: i32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: signal 0 checks process existence without sending a signal.
    unsafe { kill(pid, 0) == 0 }
}

fn send_signal(pid: i32, sig: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: best-effort test cleanup path.
    let _ = unsafe { kill(pid, sig) };
}

fn reap_child_pid_best_effort(pid: i32) {
    unsafe extern "C" {
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    }
    const WNOHANG: i32 = 1;
    for _ in 0..20 {
        let mut status = 0;
        // SAFETY: best-effort reap for test child processes; WNOHANG avoids blocking.
        let waited = unsafe { waitpid(pid, &mut status, WNOHANG) };
        if waited == pid || !pid_alive(pid) || waited == -1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
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
#[serial]
fn autostart_failure_logs_structured_event_with_stderr_tail_context() {
    let tmp = TempDir::new().expect("tempdir");
    let home = tmp.path().to_path_buf();

    let script_path = home.join("fake-daemon-fail.sh");
    let leaked_pid_path = home.join("leaked-autostart-child.pid");
    let script = format!(
        "#!/bin/sh\nset -eu\n(sleep 30) &\nbgpid=$!\nprintf '%s\\n' \"$bgpid\" > \"{}\"\necho \"fatal: invalid plugin config\" >&2\nexit 42\n",
        leaked_pid_path.display()
    );
    fs::write(&script_path, script).expect("write script");
    let mut perms = fs::metadata(&script_path)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("chmod script");

    let _home_guard = EnvGuard::set("ATM_HOME", &home.to_string_lossy());
    let _bin_guard = EnvGuard::set("ATM_DAEMON_BIN", &script_path.to_string_lossy());
    let _autostart_guard = EnvGuard::set("ATM_DAEMON_AUTOSTART", "1");

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
    let leaked_pid_guard =
        AutostartPidGuard::adopt_from_pid_file(&leaked_pid_path, Duration::from_secs(1))
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
