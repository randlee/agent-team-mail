//! Audit test to ensure all home directory resolution uses the canonical function
//!
//! This test verifies that no code in the ATM codebase directly calls `dirs::home_dir()`
//! or `dirs::config_dir()` outside of the canonical `home::get_home_dir()` function.
//!
//! This ensures:
//! - Consistent behavior across all platforms (Linux, macOS, Windows)
//! - All code respects the `ATM_HOME` environment variable
//! - Integration tests work correctly on Windows

use std::fs;
use std::path::{Path, PathBuf};

/// Recursively find all .rs files in a directory
fn find_rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Recurse into subdirectories
                files.extend(find_rust_files(&path));
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }

    files
}

/// Check if a file contains forbidden home directory calls
fn check_file(path: &Path) -> Result<(), Vec<String>> {
    let content = fs::read_to_string(path).expect("Failed to read file");

    // Skip the home.rs module itself - it's allowed to call dirs::home_dir()
    if path.ends_with("home.rs") {
        return Ok(());
    }

    // Skip the audit test itself - it needs to reference these functions in error messages
    if path.ends_with("home_dir_audit.rs") {
        return Ok(());
    }

    let mut violations = Vec::new();
    let mut pending_cfg_test = false;
    let mut in_cfg_test_module = false;
    let mut test_module_brace_depth: isize = 0;

    for (line_num, line) in content.lines().enumerate() {
        let line_num = line_num + 1; // 1-indexed

        // Skip comments and doc comments
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("///") {
            continue;
        }

        if trimmed == "#[cfg(test)]" {
            pending_cfg_test = true;
            continue;
        }

        if pending_cfg_test && trimmed.starts_with("mod tests") {
            in_cfg_test_module = true;
            pending_cfg_test = false;
        } else if !trimmed.is_empty() {
            pending_cfg_test = false;
        }

        // Check for forbidden calls
        if line.contains("dirs::home_dir()") {
            violations.push(format!(
                "{}:{}: Found raw `dirs::home_dir()` call - use `agent_team_mail_core::home::get_home_dir()` or `get_os_home_dir()` instead",
                path.display(),
                line_num
            ));
        }

        if line.contains("dirs::config_dir()") {
            violations.push(format!(
                "{}:{}: Found raw `dirs::config_dir()` call - use config-root helpers from `agent_team_mail_core::home` instead",
                path.display(),
                line_num
            ));
        }

        let raw_home_lookup = line.contains("std::env::var(\"HOME\")")
            || line.contains("std::env::var(\"USERPROFILE\")")
            || line.contains("env::var(\"HOME\")")
            || line.contains("env::var(\"USERPROFILE\")");
        let allow_raw_home_lookup =
            path.to_string_lossy().contains("/tests/") || in_cfg_test_module;
        if raw_home_lookup && !allow_raw_home_lookup {
            violations.push(format!(
                "{}:{}: Found raw HOME/USERPROFILE env lookup - use `agent_team_mail_core::home::get_home_dir()` or `get_os_home_dir()` instead",
                path.display(),
                line_num
            ));
        }

        if in_cfg_test_module {
            test_module_brace_depth += line.matches('{').count() as isize;
            test_module_brace_depth -= line.matches('}').count() as isize;
            if test_module_brace_depth <= 0 && trimmed == "}" {
                in_cfg_test_module = false;
                test_module_brace_depth = 0;
            }
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

#[test]
fn audit_no_raw_home_dir_calls() {
    // Get workspace root (3 levels up from this test file)
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("Failed to find workspace root");

    let crates_dir = workspace_root.join("crates");

    if !crates_dir.exists() {
        panic!("Crates directory not found: {}", crates_dir.display());
    }

    // Find all Rust source files
    let rust_files = find_rust_files(&crates_dir);

    if rust_files.is_empty() {
        panic!("No Rust files found in crates directory");
    }

    println!(
        "Auditing {} Rust files for raw home directory calls...",
        rust_files.len()
    );

    // Check each file
    let mut all_violations = Vec::new();
    for file in &rust_files {
        if let Err(violations) = check_file(file) {
            all_violations.extend(violations);
        }
    }

    // Report violations
    if !all_violations.is_empty() {
        eprintln!("\n❌ Home directory audit FAILED\n");
        eprintln!("Found {} violation(s):\n", all_violations.len());
        for violation in &all_violations {
            eprintln!("  {}", violation);
        }
        eprintln!(
            "\nAll production home directory resolution must use canonical helpers in `agent_team_mail_core::home`"
        );
        eprintln!(
            "This ensures runtime-root vs config-root behavior stays explicit across platforms.\n"
        );
        panic!(
            "Home directory audit failed with {} violations",
            all_violations.len()
        );
    }

    println!(
        "✓ Home directory audit passed - all {} files use canonical home helpers",
        rust_files.len()
    );
}
