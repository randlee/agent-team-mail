use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};

use agent_team_mail_core::daemon_client::{
    RuntimeKind, daemon_is_running, daemon_socket_path, runtime_kind_for_home,
};
use agent_team_mail_core::home::get_home_dir;
use agent_team_mail_daemon_launch::{LaunchClass, SpawnDaemonRequest, spawn_daemon_process};

pub(crate) fn ensure_daemon_running(team: &str) -> Option<String> {
    let socket_path =
        daemon_socket_path().unwrap_or_else(|_| std::env::temp_dir().join("atm-daemon.sock"));
    if daemon_is_running() && socket_path.exists() {
        return None;
    }

    let home = get_home_dir().ok()?;
    let daemon_bin = resolve_daemon_binary_for_home(&home)?;
    let launch_class = match runtime_kind_for_home(&home).ok()? {
        RuntimeKind::Shared => LaunchClass::Shared,
        RuntimeKind::Isolated => LaunchClass::IsolatedTest,
    };
    let child = match spawn_daemon_process(SpawnDaemonRequest {
        daemon_bin: daemon_bin.as_os_str(),
        atm_home: &home,
        launch_class,
        issuer: "agent-team-mail-tui::ensure_daemon_running",
        team: Some(team),
        stdin: Stdio::null(),
        stdout: Stdio::null(),
        stderr: Stdio::null(),
    }) {
        Ok(child) => child,
        Err(_) => {
            return Some(format!(
                "daemon unavailable: failed to start via '{}'; run `{} --team {team}`",
                daemon_bin.display(),
                daemon_bin.display()
            ));
        }
    };

    // The guard owns the child only during startup. Once the shared daemon is
    // confirmed healthy we deliberately release the handle so Drop does not
    // kill the persistent shared daemon process.
    let mut child_guard = SpawnedDaemonGuard::new(child);

    for _ in 0..20 {
        if let Some(status) = child_guard.try_wait() {
            return Some(format!(
                "daemon startup failed via '{}': exited with {status}",
                daemon_bin.display()
            ));
        }
        if daemon_is_running() && socket_path.exists() {
            child_guard.disarm();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Some("daemon startup incomplete: socket unavailable".to_string())
}

pub(crate) struct SpawnedDaemonGuard {
    child: Option<Child>,
}

impl SpawnedDaemonGuard {
    pub(crate) fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    pub(crate) fn try_wait(&mut self) -> Option<std::process::ExitStatus> {
        self.child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
    }

    pub(crate) fn disarm(&mut self) {
        // Disarm only after the shared daemon has taken over; from that point
        // forward this guard must release ownership rather than tear it down.
        self.child.take();
    }
}

impl Drop for SpawnedDaemonGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut()
            && child.try_wait().ok().flatten().is_none()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(crate) fn resolve_daemon_binary_for_home(home: &Path) -> Option<PathBuf> {
    if let Some(override_bin) = std::env::var_os("ATM_DAEMON_BIN")
        && !override_bin.is_empty()
    {
        return Some(PathBuf::from(override_bin));
    }

    let runtime_kind = runtime_kind_for_home(home).ok()?;
    match runtime_kind {
        RuntimeKind::Shared | RuntimeKind::Isolated => scoped_daemon_binary_from_current_exe()
            .or_else(|| Some(PathBuf::from(OsStr::new("atm-daemon")))),
    }
}

fn scoped_daemon_binary_from_current_exe() -> Option<PathBuf> {
    let current_exe = std::env::current_exe().ok()?;
    Some(current_exe.parent()?.join("atm-daemon"))
}

#[cfg(test)]
pub(crate) fn default_dev_runtime_root_for(os_home: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        os_home.join("AppData").join("Local").join("atm-dev")
    }

    #[cfg(not(windows))]
    {
        os_home.join(".local").join("atm-dev")
    }
}
