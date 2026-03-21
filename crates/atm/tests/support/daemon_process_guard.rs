use agent_team_mail_daemon_launch::{LaunchClass, attach_launch_token, issue_launch_token};
use std::fs;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
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
    pub fn runtime_home_path(home: &TempDir) -> PathBuf {
        home.path().to_path_buf()
    }

    pub fn spawn(home: &TempDir, team: &str) -> Self {
        let daemon_bin = daemon_binary_path();
        assert!(
            daemon_bin.exists(),
            "atm-daemon binary not found at {}",
            daemon_bin.display()
        );
        daemon_test_registry::sweep_stale_test_daemons();
        let runtime_home = Self::runtime_home_path(home);
        fs::create_dir_all(&runtime_home).expect("create daemon runtime home");
        let daemon_dir = runtime_home.join(".atm").join("daemon");
        let workdir = home.path().join("workdir");
        fs::create_dir_all(&workdir).expect("create daemon test workdir");
        let mut cmd = Command::new(&daemon_bin);
        cmd.env("ATM_HOME", &runtime_home)
            .envs([("HOME", home.path())])
            .env("ATM_TEST_SHARED_DAEMON_ADMISSION", "1")
            .env("ATM_DAEMON_AUTOSTART", "0")
            .env_remove("ATM_CONFIG")
            .env_remove("ATM_DAEMON_BIN") // F-1: prevent inheriting installed binary
            .env_remove("CLAUDE_SESSION_ID")
            .arg("--team")
            .arg(team)
            .current_dir(&workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let launch_token = issue_launch_token(
            LaunchClass::Shared,
            &runtime_home,
            daemon_bin.display().to_string(),
            "DaemonProcessGuard::spawn",
            Duration::from_secs(600),
        );
        attach_launch_token(&mut cmd, &launch_token).expect("encode daemon launch token");

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

    /// Adopt a daemon PID written to `pid_path` into a guard.
    ///
    /// This registers the PID with the daemon test registry as soon as it is
    /// observed, closing the race where a delayed PID file write can panic a
    /// test before cleanup is registered.
    #[allow(dead_code)]
    pub fn adopt_from_pid_file(
        pid_path: &Path,
        daemon_bin: &Path,
        atm_home: &Path,
        timeout: Duration,
    ) -> Option<Self> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(raw) = fs::read_to_string(pid_path)
                && let Ok(pid) = raw.trim().parse::<u32>()
                && pid > 1
                && pid_alive(pid as i32)
            {
                return Some(Self::adopt_registered_pid(pid, daemon_bin, atm_home));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
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

    #[allow(dead_code)]
    pub fn wait_with_output(mut self) -> std::io::Result<Output> {
        let child = self
            .child
            .take()
            .expect("no child handle — use from_child or spawn, not adopt_registered_pid");
        let output = child.wait_with_output();
        daemon_test_registry::unregister_test_daemon(self.pid);
        output
    }

    pub fn wait_ready(&mut self, home: &TempDir) {
        let daemon_dir = Self::runtime_home_path(home).join(".atm").join("daemon");
        let pid_path = daemon_dir.join("atm-daemon.pid");
        let status_path = daemon_dir.join("status.json");
        let socket_path = daemon_dir.join("atm-daemon.sock");
        #[cfg(windows)]
        let timeout_secs = 30;
        #[cfg(not(windows))]
        let timeout_secs = 10;
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        while Instant::now() < deadline {
            if let Some(child) = self.child.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                let output = self
                    .child
                    .take()
                    .expect("child handle should exist after readiness failure")
                    .wait_with_output()
                    .expect("collect daemon failure output");
                daemon_test_registry::unregister_test_daemon(self.pid);
                panic!(
                    "daemon exited before readiness (status={status}); expected pid {} at {}; stdout='{}'; stderr='{}'",
                    self.pid,
                    status_path.display(),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            }
            let status_pid = fs::read_to_string(&status_path)
                .ok()
                .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
                .and_then(|json| json.get("pid").and_then(serde_json::Value::as_u64))
                .map(|pid| pid as u32);
            let pid_from_file = fs::read_to_string(&pid_path)
                .ok()
                .and_then(|content| content.trim().parse::<u32>().ok())
                == Some(self.pid);
            let pid_matches = status_pid == Some(self.pid) || pid_from_file;
            if pid_matches && daemon_socket_ready(&socket_path) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let output = if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            self.child
                .take()
                .and_then(|child| child.wait_with_output().ok())
        } else {
            None
        };
        panic!(
            "daemon readiness timeout waiting for {} (pid path: {}); stdout='{}'; stderr='{}'",
            status_path.display(),
            pid_path.display(),
            output
                .as_ref()
                .map(|value| String::from_utf8_lossy(&value.stdout).into_owned())
                .unwrap_or_default(),
            output
                .as_ref()
                .map(|value| String::from_utf8_lossy(&value.stderr).into_owned())
                .unwrap_or_default()
        );
    }
}

#[cfg(unix)]
fn daemon_socket_ready(socket_path: &Path) -> bool {
    if !socket_path.exists() {
        return false;
    }
    UnixStream::connect(socket_path).is_ok()
}

#[cfg(not(unix))]
fn daemon_socket_ready(_socket_path: &Path) -> bool {
    // Windows daemons use named pipes, not Unix socket files.
    // Readiness is determined by pid_matches alone on non-unix platforms.
    true
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
                for _ in 0..20 {
                    if !pid_alive(self.pid as i32) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            // On the adopt path (`child` is None), `self.pid` is not a direct child of
            // this process. `waitpid` will return ECHILD, which is expected and harmless.
            // We call reap_child_pid_best_effort here to handle the uncommon case where
            // the adopted daemon was originally spawned as a child of a prior test process
            // that has since exited, leaving the daemon as a re-parented orphan.
            reap_child_pid_best_effort(self.pid as i32);
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
#[allow(dead_code)]
pub fn pid_alive(pid: i32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: signal 0 checks process existence without signaling the process.
    unsafe { kill(pid, 0) == 0 }
}

#[cfg(unix)]
#[allow(dead_code)]
pub fn wait_for_pid_exit(pid: i32, timeout: std::time::Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !pid_alive(pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("pid {pid} stayed alive beyond {timeout:?}");
}

#[cfg(unix)]
fn send_signal(pid: i32, sig: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: best-effort test cleanup path.
    let _ = unsafe { kill(pid, sig) };
}

#[cfg(unix)]
fn reap_child_pid_best_effort(pid: i32) {
    unsafe extern "C" {
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    }
    const WNOHANG: i32 = 1;
    for _ in 0..20 {
        let mut status = 0;
        // SAFETY: best-effort reap for test child processes; WNOHANG avoids blocking.
        let waited = unsafe { waitpid(pid, &mut status, WNOHANG) };
        if waited == pid || waited == -1 || !pid_alive(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub fn pid_alive(_pid: i32) -> bool {
    false
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub fn wait_for_pid_exit(_pid: i32, _timeout: std::time::Duration) {
    // No-op on non-unix: process probing is not supported.
}

#[cfg(not(unix))]
fn send_signal(_pid: i32, _sig: i32) {}

#[cfg(not(unix))]
fn reap_child_pid_best_effort(_pid: i32) {}

pub(crate) fn daemon_binary_path() -> PathBuf {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("workspace root from crates/atm")
            .to_path_buf();
        let status = Command::new("cargo")
            .arg("build")
            .arg("-p")
            .arg("agent-team-mail-daemon")
            .arg("--bin")
            .arg("atm-daemon")
            .current_dir(&workspace_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("build atm-daemon binary for integration tests");
        assert!(
            status.success(),
            "cargo build -p agent-team-mail-daemon --bin atm-daemon failed"
        );
    });
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
