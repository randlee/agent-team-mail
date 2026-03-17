use std::ffi::{OsStr, OsString};

/// RAII guard that restores an environment variable to its previous value on drop.
///
/// Use this whenever a test mutates an env var (e.g. `ATM_HOME`) to ensure the
/// original value is restored even if the test panics.
///
/// # Example
///
/// ```rust,ignore
/// let _guard = EnvGuard::set("ATM_HOME", temp_dir.path());
/// // ATM_HOME is restored when _guard drops, even on panic.
/// ```
pub struct EnvGuard {
    key: &'static str,
    old: Option<OsString>,
}

impl EnvGuard {
    /// Set `key` to `value` and return a guard that restores the original value on drop.
    pub fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let old = std::env::var_os(key);
        // SAFETY: test-scoped env mutation restored by Drop.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, old }
    }

    /// Remove `key` and return a guard that restores the original value on drop.
    #[allow(dead_code)]
    pub fn unset(key: &'static str) -> Self {
        let old = std::env::var_os(key);
        // SAFETY: test-scoped env mutation restored by Drop.
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: test-scoped env mutation restored by Drop.
        unsafe {
            if let Some(old) = &self.old {
                std::env::set_var(self.key, old);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}
