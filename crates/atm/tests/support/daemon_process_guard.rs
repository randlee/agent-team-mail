use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

use super::daemon_test_registry;

/// Explicit daemon process lifecycle guard for integration tests.
///
/// Tracks a spawned atm-daemon PID and guarantees teardown via kill+wait.
pub struct DaemonProcessGuard {
    child: Option<Child>,
    pid: u32,
    daemon_dir: PathBuf,
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
        let daemon_dir = home.path().join(".atm").join("daemon");
        let mut cmd = Command::new(&daemon_bin);
        cmd.env("ATM_HOME", home.path())
            .env("ATM_DAEMON_AUTOSTART", "0")
            .env_remove("ATM_CONFIG")
            .env_remove("ATM_DAEMON_BIN") // F-1: prevent inheriting installed binary
            .env_remove("CLAUDE_SESSION_ID")
            .arg("--team")
            .arg(team)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = cmd.spawn().expect("failed to spawn atm-daemon");
        let pid = child.id();
        assert!(pid > 1, "spawned daemon PID must be > 1, got {pid}");
        daemon_test_registry::register_test_daemon(pid, &daemon_bin);
        Self {
            child: Some(child),
            pid,
            daemon_dir,
        }
    }

    /// Adopt an already-running daemon PID into a guard.
    ///
    /// `atm_home` must be the test's isolated home directory, not the ambient
    /// `ATM_HOME` env var, to ensure the correct daemon lock path is used.
    #[allow(dead_code)]
    pub fn adopt_registered_pid(pid: u32, daemon_bin: &Path, atm_home: &Path) -> Self {
        assert!(pid > 1, "adopted daemon PID must be > 1, got {pid}");
        daemon_test_registry::register_test_daemon(pid, daemon_bin);
        let daemon_dir = atm_home.join(".atm").join("daemon");
        Self {
            child: None,
            pid,
            daemon_dir,
        }
    }

    /// Adopt an already-spawned `Child` into a guard.
    ///
    /// Use this instead of a hand-rolled kill+wait struct. Registers the child
    /// with the test daemon registry so sweep can find it.
    #[allow(dead_code)]
    pub fn from_child(child: Child, daemon_bin: &Path, atm_home: &Path) -> Self {
        let pid = child.id();
        assert!(pid > 1, "adopted daemon PID must be > 1, got {pid}");
        daemon_test_registry::register_test_daemon(pid, daemon_bin);
        let daemon_dir = atm_home.join(".atm").join("daemon");
        Self {
            child: Some(child),
            pid,
            daemon_dir,
        }
    }

    #[allow(dead_code)]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Return a mutable reference to the inner `Child`.
    ///
    /// Panics if this guard was created via `adopt_registered_pid` (no child handle).
    #[allow(dead_code)]
    pub fn child_mut(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("no child handle — use from_child or spawn, not adopt_registered_pid")
    }

    pub fn wait_ready(&mut self, home: &TempDir) {
        let daemon_dir = home.path().join(".atm").join("daemon");
        let pid_path = daemon_dir.join("atm-daemon.pid");
        let status_path = daemon_dir.join("status.json");
        let socket_path = daemon_dir.join("atm-daemon.sock");
        #[cfg(windows)]
        let timeout_secs = 30;
        #[cfg(not(windows))]
        let timeout_secs = 4;
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        while Instant::now() < deadline {
            if let Some(child) = self.child.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
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
            if socket_path.exists() {
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
        if let Some(child) = self.child.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
        } else if pid_alive(self.pid as i32) {
            send_signal(self.pid as i32, 15);
            for _ in 0..20 {
                if !pid_alive(self.pid as i32) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if pid_alive(self.pid as i32) {
                send_signal(self.pid as i32, 9);
            }
        }
        let lock_path = self.daemon_dir.join("daemon.lock");
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(lock) = agent_team_mail_core::io::lock::acquire_lock(&lock_path, 0) {
                drop(lock);
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        daemon_test_registry::unregister_test_daemon(self.pid);
    }
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: signal 0 checks process existence without signaling the process.
    unsafe { kill(pid, 0) == 0 }
}

#[cfg(unix)]
fn send_signal(pid: i32, sig: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: best-effort test cleanup path.
    let _ = unsafe { kill(pid, sig) };
}

#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    false
}

#[cfg(not(unix))]
fn send_signal(_pid: i32, _sig: i32) {}

fn daemon_binary_path() -> PathBuf {
    // Locate the build output directory from the current test binary path.
    // For integration tests, `current_exe()` is in `target/<profile>/deps/`.
    // The daemon binary lives one level up in `target/<profile>/`.
    let exe = std::env::current_exe().expect("current_exe");
    let deps_dir = exe.parent().expect("parent of test binary");
    let target_dir = if deps_dir.ends_with("deps") {
        deps_dir.parent().expect("parent of deps dir")
    } else {
        deps_dir
    };
    #[cfg(windows)]
    let name = "atm-daemon.exe";
    #[cfg(not(windows))]
    let name = "atm-daemon";
    target_dir.join(name)
}
