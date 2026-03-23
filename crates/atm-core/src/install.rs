use std::fs;
use std::path::Path;

/// Read the dev-install milestone from the install manifest adjacent to a bin/
/// directory. Returns `None` for source builds or installs without a manifest.
pub fn read_install_milestone_from_exe(exe: &Path) -> Option<String> {
    let bin_dir = exe.parent()?;
    if bin_dir.file_name()?.to_str()? != "bin" {
        return None;
    }

    let manifest_path = bin_dir.parent()?.join("manifest.json");
    let raw = fs::read_to_string(manifest_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("milestone_version")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub fn read_active_install_milestone() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    read_install_milestone_from_exe(&exe)
}

/// Prefer the installed milestone label when available so dev installs can
/// report their promoted build identity instead of only the workspace version.
pub fn effective_display_version(default_version: &str) -> String {
    read_active_install_milestone().unwrap_or_else(|| default_version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_milestone_from_bin_adjacent_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let install_root = temp.path().join("installs/0.46.1-dev.6");
        let bin_dir = install_root.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("create bin");
        std::fs::write(
            install_root.join("manifest.json"),
            r#"{"milestone_version":"0.46.1-dev.6"}"#,
        )
        .expect("write manifest");
        let exe = bin_dir.join("atm");
        std::fs::write(&exe, "").expect("write exe");

        let milestone = read_install_milestone_from_exe(&exe);
        assert_eq!(milestone.as_deref(), Some("0.46.1-dev.6"));
    }

    #[test]
    fn returns_none_when_not_running_from_bin_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let exe = temp.path().join("target/release/atm");
        std::fs::create_dir_all(exe.parent().expect("parent")).expect("create parent");
        std::fs::write(&exe, "").expect("write exe");

        assert_eq!(read_install_milestone_from_exe(&exe), None);
    }

    #[test]
    fn returns_none_for_missing_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bin_dir = temp.path().join("install/bin");
        std::fs::create_dir_all(&bin_dir).expect("create bin");
        let exe = bin_dir.join("atm");
        std::fs::write(&exe, "").expect("write exe");

        assert_eq!(read_install_milestone_from_exe(&exe), None);
    }
}
