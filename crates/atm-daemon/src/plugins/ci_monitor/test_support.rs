use std::path::Path;

use tempfile::TempDir;

pub(crate) fn write_gh_monitor_config(home: &Path, team: &str) {
    let cfg_dir = home.join(".config/atm");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let config = format!(
        r#"[core]
default_team = "{team}"
identity = "daemon-test"

[plugins.gh_monitor]
enabled = true
team = "{team}"
agent = "gh-monitor"
repo = "o/r"
poll_interval_secs = 60
"#
    );
    std::fs::write(cfg_dir.join("config.toml"), config).unwrap();
}

pub(crate) fn write_repo_gh_monitor_config(repo_dir: &Path, team: &str) {
    std::fs::create_dir_all(repo_dir).unwrap();
    let config = format!(
        r#"[core]
default_team = "{team}"
identity = "daemon-test"

[plugins.gh_monitor]
enabled = true
team = "{team}"
agent = "gh-monitor"
repo = "o/r"
poll_interval_secs = 60
"#
    );
    std::fs::write(repo_dir.join(".atm.toml"), config).unwrap();
}

pub(crate) fn write_invalid_gh_monitor_config(home: &Path, team: &str) {
    let cfg_dir = home.join(".config/atm");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let config = format!(
        r#"[core]
default_team = "{team}"
identity = "daemon-test"

[plugins.gh_monitor]
enabled = true
team = "{team}"
agent = "gh-monitor"
poll_interval_secs = 1
"#
    );
    std::fs::write(cfg_dir.join("config.toml"), config).unwrap();
}

pub(crate) struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: test-only env mutation, guarded by #[serial] on callers.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: test-only env mutation, guarded by #[serial] on callers.
        unsafe {
            match &self.previous {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

#[cfg(unix)]
pub(crate) fn install_fake_gh_script(temp: &TempDir, script_body: &str) -> EnvGuard {
    use std::os::unix::fs::PermissionsExt;

    let script_path = temp.path().join("gh");
    let body = script_body
        .strip_prefix("#!/bin/sh\n")
        .unwrap_or(script_body);
    let wrapped = format!(
        r#"#!/bin/sh
if [ "$1" = "-R" ]; then
  shift
  if [ -n "$1" ]; then
    shift
  fi
fi
{body}
"#
    );
    std::fs::write(&script_path, wrapped).expect("write fake gh script");
    let mut perms = std::fs::metadata(&script_path)
        .expect("stat fake gh script")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms).expect("chmod fake gh script");

    let previous_path = std::env::var("PATH").unwrap_or_default();
    let composed = if previous_path.is_empty() {
        temp.path().display().to_string()
    } else {
        format!("{}:{previous_path}", temp.path().display())
    };
    EnvGuard::set("PATH", &composed)
}
