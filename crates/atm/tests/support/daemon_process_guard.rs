use assert_cmd::cargo;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

use super::daemon_test_registry;

/// Explicit daemon process lifecycle guard for integration tests.
///
/// Tracks a spawned atm-daemon PID and guarantees teardown via kill+wait.
pub struct DaemonProcessGuard {
    child: Child,
    pid: u32,
}

impl DaemonProcessGuard {
    pub fn spawn(home: &TempDir, team: &str) -> Self {
        let daemon_bin = daemon_binary_path();
        assert!(
            daemon_bin.exists(),
            "atm-daemon binary not found at {}",
            daemon_bin.display()
        );
        daemon_test_registry::sweep_stale_test_daemons();
        let mut cmd = Command::new(&daemon_bin);
        cmd.env("ATM_HOME", home.path())
            .env("ATM_DAEMON_AUTOSTART", "0")
            .env_remove("ATM_CONFIG")
            .env_remove("CLAUDE_SESSION_ID")
            .arg("--team")
            .arg(team)
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = cmd.spawn().expect("failed to spawn atm-daemon");
        let pid = child.id();
        assert!(pid > 1, "spawned daemon PID must be > 1, got {pid}");
        daemon_test_registry::register_test_daemon(pid, &daemon_bin);
        Self { child, pid }
    }

    #[allow(dead_code)]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn wait_ready(&mut self, home: &TempDir) {
        let daemon_dir = home.path().join(".atm").join("daemon");
        let pid_path = daemon_dir.join("atm-daemon.pid");
        let status_path = daemon_dir.join("status.json");
        #[cfg(windows)]
        let timeout_secs = 30;
        #[cfg(not(windows))]
        let timeout_secs = 4;
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "daemon exited before readiness (status={status}); expected pid {} at {}",
                    self.pid,
                    status_path.display()
                );
            }
            let status_pid = fs::read_to_string(&status_path)
                .ok()
                .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
                .and_then(|json| json.get("pid").and_then(serde_json::Value::as_u64))
                .map(|pid| pid as u32);
            if status_pid == Some(self.pid) {
                return;
            }
            if let Ok(content) = fs::read_to_string(&pid_path)
                && let Ok(pid) = content.trim().parse::<u32>()
                && pid == self.pid
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!(
            "daemon readiness timeout waiting for {} (pid path: {})",
            status_path.display(),
            pid_path.display()
        );
    }
}

impl Drop for DaemonProcessGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        daemon_test_registry::unregister_test_daemon(self.pid);
    }
}

fn daemon_binary_path() -> PathBuf {
    let mut candidate = PathBuf::from(cargo::cargo_bin!("atm"));
    #[cfg(windows)]
    candidate.set_file_name("atm-daemon.exe");
    #[cfg(not(windows))]
    candidate.set_file_name("atm-daemon");
    candidate
}
