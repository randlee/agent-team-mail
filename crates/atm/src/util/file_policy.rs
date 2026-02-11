//! File access policy enforcement for file references

use anyhow::Result;
use atm_core::config::resolve_settings;
use std::path::Path;

/// Check if a file path is allowed for the destination team
///
/// Returns (is_allowed, rewritten_message).
/// If not allowed, copies file to share directory and returns rewritten message.
pub fn check_file_reference(
    file_path: &Path,
    message_text: &str,
    team_name: &str,
    current_dir: &Path,
    home_dir: &Path,
) -> Result<(bool, String)> {
    // Try to resolve settings for the destination repo
    let settings = resolve_settings(None, current_dir, home_dir);

    // Default policy: only files inside current repo are allowed
    let is_allowed = if let Some(ref settings) = settings {
        // Check permissions in settings
        check_permissions_allow_file(settings, file_path, current_dir)
    } else {
        // No settings found - default to checking if file is in current repo
        is_file_in_repo(file_path, current_dir)
    };

    if is_allowed {
        // File is allowed - return as-is
        Ok((true, String::new()))
    } else {
        // File is not allowed - copy to share directory
        let share_dir = home_dir.join(".config/atm/share").join(team_name);
        std::fs::create_dir_all(&share_dir)?;

        let file_name = file_path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid file path"))?;
        let copy_path = share_dir.join(file_name);

        std::fs::copy(file_path, &copy_path)?;

        // Create rewritten message with notice
        let rewritten_message = format!(
            "{}\n\n[atm] File path rewritten to a local share copy for destination access.\nOriginal: {}\nCopy: {}",
            message_text,
            file_path.display(),
            copy_path.display()
        );

        Ok((false, rewritten_message))
    }
}

/// Check if file is within the repo root
fn is_file_in_repo(file_path: &Path, repo_root: &Path) -> bool {
    file_path
        .canonicalize()
        .ok()
        .and_then(|canonical| {
            repo_root
                .canonicalize()
                .ok()
                .map(|repo| canonical.starts_with(repo))
        })
        .unwrap_or(false)
}

/// Check if settings permissions allow the file
fn check_permissions_allow_file(
    settings: &atm_core::schema::SettingsJson,
    file_path: &Path,
    _current_dir: &Path,
) -> bool {
    // If no permissions section, default to allow
    let Some(ref permissions) = settings.permissions else {
        return true;
    };

    // Check deny rules first
    if !permissions.deny.is_empty() {
        for rule in &permissions.deny {
            if file_matches_rule(file_path, rule) {
                return false;
            }
        }
    }

    // Check allow rules
    if !permissions.allow.is_empty() {
        for rule in &permissions.allow {
            if file_matches_rule(file_path, rule) {
                return true;
            }
        }
        // If allow list exists but doesn't match, deny
        return false;
    }

    // No explicit rules - allow by default
    true
}

/// Check if a file path matches a permission rule
fn file_matches_rule(file_path: &Path, rule: &str) -> bool {
    // Simple pattern matching - for MVP, just check if rule is a Read() pattern
    if let Some(inner) = rule.strip_prefix("Read(").and_then(|s| s.strip_suffix(')')) {
        // Extract path pattern from Read() rule
        let pattern = inner.trim();

        // Normalize pattern: remove leading ./
        let normalized_pattern = pattern.strip_prefix("./").unwrap_or(pattern);

        // Handle glob patterns (basic support for **)
        if normalized_pattern.contains("**") {
            // Pattern with ** - match any subdirectory
            let base = normalized_pattern.trim_end_matches("**").trim_end_matches('/');
            return file_path.to_str().map(|p| p.contains(base)).unwrap_or(false);
        }

        // Simple prefix match
        file_path
            .to_str()
            .map(|p| p.contains(normalized_pattern))
            .unwrap_or(false)
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_is_file_in_repo() {
        let temp_dir = TempDir::new().unwrap();
        let repo_root = temp_dir.path();
        let file_path = repo_root.join("test.txt");

        fs::write(&file_path, "test").unwrap();

        assert!(is_file_in_repo(&file_path, repo_root));
    }

    #[test]
    fn test_is_file_outside_repo() {
        let temp_dir1 = TempDir::new().unwrap();
        let temp_dir2 = TempDir::new().unwrap();

        let repo_root = temp_dir1.path();
        let file_path = temp_dir2.path().join("test.txt");

        fs::write(&file_path, "test").unwrap();

        assert!(!is_file_in_repo(&file_path, repo_root));
    }

    #[test]
    fn test_check_file_reference_allowed() {
        let temp_dir = TempDir::new().unwrap();
        let home_dir = temp_dir.path();
        let current_dir = temp_dir.path();

        let file_path = current_dir.join("allowed.txt");
        fs::write(&file_path, "test content").unwrap();

        let (is_allowed, _) = check_file_reference(
            &file_path,
            "Test message",
            "test-team",
            current_dir,
            home_dir,
        )
        .unwrap();

        assert!(is_allowed);
    }

    #[test]
    fn test_check_file_reference_not_allowed_copies() {
        let temp_dir = TempDir::new().unwrap();
        let home_dir = temp_dir.path();
        let current_dir = temp_dir.path().join("repo");
        let external_dir = temp_dir.path().join("external");

        fs::create_dir_all(&current_dir).unwrap();
        fs::create_dir_all(&external_dir).unwrap();

        let file_path = external_dir.join("external.txt");
        fs::write(&file_path, "test content").unwrap();

        let (is_allowed, rewritten) = check_file_reference(
            &file_path,
            "Test message",
            "test-team",
            &current_dir,
            home_dir,
        )
        .unwrap();

        assert!(!is_allowed);
        assert!(rewritten.contains("[atm] File path rewritten"));
        assert!(rewritten.contains("Original:"));
        assert!(rewritten.contains("Copy:"));

        // Verify file was copied
        let share_dir = home_dir.join(".config/atm/share/test-team");
        let copy_path = share_dir.join("external.txt");
        assert!(copy_path.exists());
        assert_eq!(fs::read_to_string(&copy_path).unwrap(), "test content");
    }

    #[test]
    fn test_file_matches_rule_read_pattern() {
        let file_path = Path::new("/home/user/secrets/key.txt");

        assert!(file_matches_rule(file_path, "Read(secrets)"));
        assert!(file_matches_rule(file_path, "Read(./secrets/**)"));
        assert!(!file_matches_rule(file_path, "Read(config)"));
    }
}
