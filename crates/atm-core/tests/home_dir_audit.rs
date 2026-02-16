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

    for (line_num, line) in content.lines().enumerate() {
        let line_num = line_num + 1; // 1-indexed

        // Skip comments and doc comments
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("///") {
            continue;
        }

        // Check for forbidden calls
        if line.contains("dirs::home_dir()") {
            violations.push(format!(
                "{}:{}: Found raw `dirs::home_dir()` call - use `agent_team_mail_core::home::get_home_dir()` instead",
                path.display(),
                line_num
            ));
        }

        if line.contains("dirs::config_dir()") {
            violations.push(format!(
                "{}:{}: Found raw `dirs::config_dir()` call - use `agent_team_mail_core::home::get_home_dir()` instead",
                path.display(),
                line_num
            ));
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

    println!("Auditing {} Rust files for raw home directory calls...", rust_files.len());

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
        eprintln!("\nAll home directory resolution must use `agent_team_mail_core::home::get_home_dir()`");
        eprintln!("This ensures consistent behavior across platforms and respect for ATM_HOME.\n");
        panic!("Home directory audit failed with {} violations", all_violations.len());
    }

    println!("✓ Home directory audit passed - all {} files use canonical get_home_dir()", rust_files.len());
}
