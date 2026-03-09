//! Cross-platform process liveness checking.
//!
//! On Unix, uses `kill(pid, 0)` to probe process existence.
//! On Windows, uses `OpenProcess` + `GetExitCodeProcess` to check
//! whether the process is still running.

/// Check whether an OS process with the given PID is currently alive.
///
/// Returns `true` if the process exists and is running, `false` otherwise.
/// On Unix, a process owned by another user that returns EPERM is still
/// considered alive (it exists, we just cannot signal it).
pub fn is_pid_alive(pid: u32) -> bool {
    platform::is_pid_alive_impl(pid)
}

/// Best-effort parent process ID for the current process.
///
/// Returns `Some(ppid)` when available on the current platform, otherwise `None`.
pub fn parent_pid() -> Option<u32> {
    platform::parent_pid_impl()
}

#[cfg(unix)]
mod platform {
    pub fn is_pid_alive_impl(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        // SAFETY: `kill(pid, 0)` is a standard POSIX call that only probes
        // process existence. It does not deliver a signal.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if rc == 0 {
            return true;
        }
        // EPERM means the process exists but we lack permission to signal it.
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    pub fn parent_pid_impl() -> Option<u32> {
        // SAFETY: getppid reads current process metadata and has no side effects.
        let ppid = unsafe { libc::getppid() };
        if ppid > 0 { Some(ppid as u32) } else { None }
    }
}

#[cfg(windows)]
mod platform {
    use std::ptr;

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;

    unsafe extern "system" {
        fn OpenProcess(
            desired_access: u32,
            inherit_handles: i32,
            process_id: u32,
        ) -> *mut core::ffi::c_void;
        fn CloseHandle(handle: *mut core::ffi::c_void) -> i32;
        fn GetExitCodeProcess(handle: *mut core::ffi::c_void, exit_code: *mut u32) -> i32;
    }

    pub fn is_pid_alive_impl(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        // SAFETY: OpenProcess with PROCESS_QUERY_LIMITED_INFORMATION is a
        // read-only operation. We close the handle before returning.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() || handle == ptr::null_mut() {
                return false;
            }
            let mut exit_code: u32 = 0;
            let success = GetExitCodeProcess(handle, &mut exit_code);
            CloseHandle(handle);
            success != 0 && exit_code == STILL_ACTIVE
        }
    }

    pub fn parent_pid_impl() -> Option<u32> {
        None
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    pub fn is_pid_alive_impl(_pid: u32) -> bool {
        false
    }

    pub fn parent_pid_impl() -> Option<u32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        let pid = std::process::id();
        assert!(
            is_pid_alive(pid),
            "current process (PID {pid}) must be detected as alive"
        );
    }

    #[test]
    fn nonexistent_pid_is_dead() {
        // Use a PID just below i32::MAX — well beyond kernel PID range on
        // all platforms, but safe from wrapping to -1 (which `kill` interprets
        // as "all processes" on some Unix systems).
        let dead_pid = (i32::MAX - 1) as u32;
        assert!(
            !is_pid_alive(dead_pid),
            "PID {dead_pid} should not be alive"
        );
    }

    #[test]
    fn pid_zero_is_not_alive() {
        assert!(!is_pid_alive(0), "PID 0 should not be considered alive");
    }

    #[cfg(unix)]
    #[test]
    fn parent_pid_is_available_on_unix() {
        assert!(
            parent_pid().is_some(),
            "parent_pid should be present on unix platforms"
        );
    }
}
