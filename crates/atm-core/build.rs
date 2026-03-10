//! Build script for `agent-team-mail-core`.
//!
//! Validates that the workspace `Cargo.lock` contains an entry for
//! `agent-team-mail-core` at the same version as `CARGO_PKG_VERSION`.
//!
//! If the lockfile is stale or missing the entry, the build fails with a clear
//! message directing the developer to run `cargo generate-lockfile`.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    // Only re-run this build script when the lockfile or our own Cargo.toml changes.
    println!("cargo:rerun-if-changed=../../Cargo.lock");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let pkg_version =
        env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION must be set by cargo during build");

    // Cargo.lock is at the workspace root, two levels above the crate root.
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR must be set by cargo during build");
    let lockfile_path = Path::new(&manifest_dir).join("../../Cargo.lock");

    let lockfile_contents = match fs::read_to_string(&lockfile_path) {
        Ok(contents) => contents,
        Err(err) => {
            panic!(
                "RELEASE ERROR: Could not read Cargo.lock at {path}: {err}\n\
                 Run `cargo generate-lockfile` to generate it.",
                path = lockfile_path.display(),
            )
        }
    };

    let found_version = find_lockfile_version(&lockfile_contents, "agent-team-mail-core");

    match found_version {
        Some(ref locked_version) if locked_version == &pkg_version => {
            // Lockfile is fresh — nothing to do.
        }
        Some(locked_version) => {
            println!(
                "cargo:warning=RELEASE ERROR: Cargo.lock is stale for agent-team-mail-core. \
                 Run `cargo generate-lockfile` to update."
            );
            panic!(
                "RELEASE ERROR: Cargo.lock is stale.\n\
                 Run `cargo generate-lockfile` to update.\n\
                 Expected agent-team-mail-core version {pkg_version} in Cargo.lock \
                 but found {locked_version}."
            );
        }
        None => {
            println!(
                "cargo:warning=RELEASE ERROR: agent-team-mail-core is missing from Cargo.lock. \
                 Run `cargo generate-lockfile` to update."
            );
            panic!(
                "RELEASE ERROR: Cargo.lock is stale.\n\
                 Run `cargo generate-lockfile` to update.\n\
                 Expected agent-team-mail-core version {pkg_version} in Cargo.lock but it is missing."
            );
        }
    }
}

/// Scans `lockfile_contents` for the first `[[package]]` block whose `name`
/// field equals `package_name` and returns its `version` value, or `None` if
/// no such package is present.
///
/// The TOML lockfile format repeats `[[package]]` table headers; we parse
/// manually with `std` only to avoid adding a build-time dependency.
fn find_lockfile_version<'a>(lockfile_contents: &'a str, package_name: &str) -> Option<&'a str> {
    // State machine: scan for [[package]] headers, then extract name/version
    // from the lines that follow until the next blank line or [[package]].
    let mut in_target_block = false;
    let mut found_name = false;
    for line in lockfile_contents.lines() {
        let trimmed = line.trim();

        if trimmed == "[[package]]" {
            // Start a new package block. Reset per-block state.
            in_target_block = false;
            found_name = false;
            continue;
        }

        if trimmed.is_empty() {
            // Blank lines separate blocks in some lockfile styles; reset.
            if in_target_block {
                // We were in the target block but haven't finished — shouldn't
                // happen in a well-formed lockfile, but handle gracefully.
                in_target_block = false;
                found_name = false;
            }
            continue;
        }

        if let Some(name_val) = extract_toml_string(trimmed, "name") {
            if name_val == package_name {
                in_target_block = true;
                found_name = true;
            } else if in_target_block {
                // Different name — wrong block.
                in_target_block = false;
                found_name = false;
            }
        }

        if in_target_block && found_name {
            if let Some(ver_val) = extract_toml_string(trimmed, "version") {
                // Return the extracted value directly. This avoids byte-offset
                // reconstruction bugs on CRLF lockfiles (Windows).
                return Some(ver_val);
            }
        }
    }

    None
}

/// Extracts the string value from a simple TOML assignment `key = "value"`.
/// Returns `Some(value)` if the line matches, `None` otherwise.
fn extract_toml_string<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let line = line.trim();
    // Expect: `key = "value"`
    let prefix = key;
    if !line.starts_with(prefix) {
        return None;
    }
    let after_key = line[prefix.len()..].trim_start();
    if !after_key.starts_with('=') {
        return None;
    }
    let after_eq = after_key[1..].trim_start();
    if !after_eq.starts_with('"') {
        return None;
    }
    let inner = &after_eq[1..];
    let end = inner.find('"')?;
    Some(&inner[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_correct_version() {
        let lockfile = r#"
[[package]]
name = "some-other-crate"
version = "1.0.0"

[[package]]
name = "agent-team-mail-core"
version = "0.43.0"
dependencies = [
 "anyhow",
]

[[package]]
name = "yet-another"
version = "2.0.0"
"#;
        assert_eq!(
            find_lockfile_version(lockfile, "agent-team-mail-core"),
            Some("0.43.0")
        );
    }

    #[test]
    fn returns_none_when_missing() {
        let lockfile = r#"
[[package]]
name = "some-other-crate"
version = "1.0.0"
"#;
        assert_eq!(
            find_lockfile_version(lockfile, "agent-team-mail-core"),
            None
        );
    }

    #[test]
    fn finds_correct_version_with_crlf_line_endings() {
        let lockfile = "[[package]]\r\nname = \"agent-team-mail\"\r\nversion = \"0.43.0\"\r\n\r\n\
[[package]]\r\nname = \"agent-team-mail-core\"\r\nversion = \"0.43.1\"\r\n";

        assert_eq!(
            find_lockfile_version(lockfile, "agent-team-mail-core"),
            Some("0.43.1")
        );
    }

    #[test]
    fn extracts_toml_string() {
        assert_eq!(extract_toml_string(r#"name = "foo""#, "name"), Some("foo"));
        assert_eq!(
            extract_toml_string(r#"version = "1.2.3""#, "version"),
            Some("1.2.3")
        );
        assert_eq!(extract_toml_string(r#"other = "bar""#, "name"), None);
    }
}
