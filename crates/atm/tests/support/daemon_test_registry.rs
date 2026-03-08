use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryEntry {
    pid: u32,
    daemon_bin: String,
}

fn registry_path() -> std::path::PathBuf {
    std::env::temp_dir().join("atm-test-daemon-registry.json")
}

fn load_entries() -> Vec<RegistryEntry> {
    let path = registry_path();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn save_entries(entries: &[RegistryEntry]) {
    let path = registry_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(entries).unwrap_or_else(|_| "[]".to_string());
    let _ = std::fs::write(path, json);
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: kill with signal 0 only checks process existence.
    unsafe { kill(pid, 0) == 0 }
}

#[cfg(unix)]
fn send_signal(pid: i32, sig: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: best-effort test teardown path.
    let _ = unsafe { kill(pid, sig) };
}

#[cfg(unix)]
fn pid_command(pid: i32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .arg("-o")
        .arg("command=")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let cmd = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if cmd.is_empty() { None } else { Some(cmd) }
}

#[cfg(unix)]
fn command_matches(entry: &RegistryEntry, cmd: &str) -> bool {
    let expected = Path::new(&entry.daemon_bin)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("atm-daemon");
    cmd.contains(expected)
}

/// Best-effort stale test-daemon sweep.
///
/// This only targets PIDs previously registered by this test fixture.
pub fn sweep_stale_test_daemons() {
    let mut entries = load_entries();
    if entries.is_empty() {
        return;
    }

    #[cfg(unix)]
    {
        let mut retained = Vec::new();
        for entry in entries.drain(..) {
            let pid = entry.pid as i32;
            if !pid_alive(pid) {
                continue;
            }
            let Some(cmd) = pid_command(pid) else {
                retained.push(entry);
                continue;
            };
            if !command_matches(&entry, &cmd) {
                retained.push(entry);
                continue;
            }

            send_signal(pid, 15);
            for _ in 0..20 {
                if !pid_alive(pid) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if pid_alive(pid) {
                send_signal(pid, 9);
            }
            if pid_alive(pid) {
                retained.push(entry);
            }
        }
        save_entries(&retained);
        return;
    }

    #[cfg(not(unix))]
    {
        // On non-Unix, daemon process tests are not expected to run.
        let _ = entries;
    }
}

pub fn register_test_daemon(pid: u32, daemon_bin: &Path) {
    let mut entries = load_entries();
    entries.push(RegistryEntry {
        pid,
        daemon_bin: daemon_bin.to_string_lossy().to_string(),
    });
    save_entries(&entries);
}

pub fn unregister_test_daemon(pid: u32) {
    let mut entries = load_entries();
    entries.retain(|entry| entry.pid != pid);
    save_entries(&entries);
}
