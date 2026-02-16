//! Inbox retention policy implementation
//!
//! Provides configurable retention policies to prevent unbounded inbox growth.
//! Supports age-based and count-based policies with archive or delete strategies.

use crate::config::{CleanupStrategy, RetentionConfig};
use crate::io::inbox::inbox_update;
use crate::schema::InboxMessage;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use std::fs;
use std::path::{Path, PathBuf};

/// Result of applying retention policy to an inbox
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionResult {
    /// Number of messages kept in inbox
    pub kept: usize,
    /// Number of messages removed from inbox
    pub removed: usize,
    /// Number of messages archived (subset of removed)
    pub archived: usize,
}

impl RetentionResult {
    /// Create a new retention result
    pub fn new(kept: usize, removed: usize, archived: usize) -> Self {
        Self {
            kept,
            removed,
            archived,
        }
    }
}

/// Apply retention policy to an inbox
///
/// Reads the inbox file, determines which messages should be removed based on
/// the configured policy (max age and/or max count), and either deletes them
/// or archives them based on the cleanup strategy.
///
/// # Arguments
///
/// * `inbox_path` - Path to the inbox.json file
/// * `team` - Team name (used for archive directory structure)
/// * `agent` - Agent name (used for archive directory structure)
/// * `policy` - Retention configuration to apply
/// * `dry_run` - If true, return what would be done without modifying files
///
/// # Returns
///
/// Returns `RetentionResult` with counts of kept, removed, and archived messages.
///
/// # Errors
///
/// Returns error if file operations fail or if duration parsing fails.
pub fn apply_retention(
    inbox_path: &Path,
    team: &str,
    agent: &str,
    policy: &RetentionConfig,
    dry_run: bool,
) -> Result<RetentionResult> {
    // If inbox doesn't exist, nothing to do
    if !inbox_path.exists() {
        return Ok(RetentionResult::new(0, 0, 0));
    }

    // Read current inbox
    let content = fs::read_to_string(inbox_path)
        .with_context(|| format!("Failed to read inbox at {}", inbox_path.display()))?;
    let messages: Vec<InboxMessage> = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse inbox at {}", inbox_path.display()))?;

    // If no retention policy configured, keep all messages
    if policy.max_age.is_none() && policy.max_count.is_none() {
        return Ok(RetentionResult::new(messages.len(), 0, 0));
    }

    let now = Utc::now();
    let max_age_duration = if let Some(ref age_str) = policy.max_age {
        Some(parse_duration(age_str)?)
    } else {
        None
    };

    // Determine which messages to keep
    let mut to_keep = Vec::new();
    let mut to_remove = Vec::new();

    for message in messages {
        let should_remove = should_remove_message(&message, &max_age_duration, now, policy.max_count, to_keep.len());

        if should_remove {
            to_remove.push(message);
        } else {
            to_keep.push(message);
        }
    }

    // If nothing to remove, we're done
    if to_remove.is_empty() {
        return Ok(RetentionResult::new(to_keep.len(), 0, 0));
    }

    // In dry-run mode, just return the counts
    if dry_run {
        let archived = if policy.strategy == CleanupStrategy::Archive {
            to_remove.len()
        } else {
            0
        };
        return Ok(RetentionResult::new(to_keep.len(), to_remove.len(), archived));
    }

    // Archive messages if configured
    let archived = if policy.strategy == CleanupStrategy::Archive {
        let archive_dir = determine_archive_dir(policy)?;
        archive_messages(&to_remove, team, agent, &archive_dir)?;
        to_remove.len()
    } else {
        0
    };

    // Update inbox with only the messages to keep
    inbox_update(inbox_path, team, agent, |messages| {
        messages.clear();
        messages.extend(to_keep.clone());
    })?;

    Ok(RetentionResult::new(to_keep.len(), to_remove.len(), archived))
}

/// Determine if a message should be removed based on retention policy
fn should_remove_message(
    message: &InboxMessage,
    max_age_duration: &Option<Duration>,
    now: DateTime<Utc>,
    max_count: Option<usize>,
    current_kept_count: usize,
) -> bool {
    // Check age-based policy
    if let Some(max_age) = max_age_duration
        && is_expired_by_age(message, max_age, now)
    {
        return true;
    }

    // Check count-based policy
    // We keep the newest messages up to max_count
    // Since messages are processed in order, once we've kept max_count messages,
    // all subsequent messages should be removed
    if let Some(max_count) = max_count
        && current_kept_count >= max_count
    {
        return true;
    }

    false
}

/// Check if a message exceeds the maximum age policy
fn is_expired_by_age(message: &InboxMessage, max_age: &Duration, now: DateTime<Utc>) -> bool {
    // Parse message timestamp
    if let Ok(msg_time) = DateTime::parse_from_rfc3339(&message.timestamp) {
        let msg_time_utc = msg_time.with_timezone(&Utc);
        let age = now.signed_duration_since(msg_time_utc);
        age > *max_age
    } else {
        // If we can't parse the timestamp, treat as expired (safer default)
        true
    }
}

/// Parse duration string into chrono::Duration
///
/// Supports formats like:
/// - "7d" -> 7 days
/// - "24h" -> 24 hours
/// - "30d" -> 30 days
/// - "168h" -> 168 hours (7 days)
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("Empty duration string");
    }

    // Extract number and unit
    let (num_part, unit) = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(idx) => (&s[..idx], &s[idx..]),
        None => anyhow::bail!("Duration must have a unit (h or d): {s}"),
    };

    let num: i64 = num_part.parse()
        .with_context(|| format!("Invalid number in duration: {s}"))?;

    match unit {
        "h" => Ok(Duration::hours(num)),
        "d" => Ok(Duration::days(num)),
        _ => anyhow::bail!("Unknown duration unit '{unit}'. Use 'h' for hours or 'd' for days"),
    }
}

/// Determine the archive directory from config or use default
fn determine_archive_dir(policy: &RetentionConfig) -> Result<PathBuf> {
    if let Some(ref dir_str) = policy.archive_dir {
        Ok(PathBuf::from(dir_str))
    } else {
        // Default: ~/.config/atm/archive/
        // When ATM_HOME is set, use it directly (test-friendly)
        if let Ok(atm_home) = std::env::var("ATM_HOME") {
            return Ok(PathBuf::from(atm_home).join("archive"));
        }
        let home = crate::home::get_home_dir()?;
        Ok(home.join(".config/atm/archive"))
    }
}

/// Archive messages to a timestamped archive file
///
/// Messages are archived to: `{archive_dir}/{team}/{agent}/archive-{timestamp}.json`
fn archive_messages(
    messages: &[InboxMessage],
    team: &str,
    agent: &str,
    archive_dir: &Path,
) -> Result<()> {
    // Create archive directory structure
    let team_agent_dir = archive_dir.join(team).join(agent);
    fs::create_dir_all(&team_agent_dir)
        .with_context(|| format!("Failed to create archive directory: {}", team_agent_dir.display()))?;

    // Create timestamped archive file
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let archive_file = team_agent_dir.join(format!("archive-{timestamp}.json"));

    // Write messages to archive file
    let json = serde_json::to_string_pretty(messages)
        .context("Failed to serialize messages for archiving")?;
    fs::write(&archive_file, json)
        .with_context(|| format!("Failed to write archive file: {}", archive_file.display()))?;

    Ok(())
}

/// Result of cleaning report files
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanReportResult {
    /// Number of files deleted
    pub deleted_count: usize,
    /// Number of files skipped (not expired)
    pub skipped_count: usize,
}

impl CleanReportResult {
    /// Create a new clean report result
    pub fn new(deleted_count: usize, skipped_count: usize) -> Self {
        Self {
            deleted_count,
            skipped_count,
        }
    }
}

/// Clean old report files from the CI monitor report directory
///
/// Scans `report_dir` for `*.json` and `*.md` files older than `max_age`
/// and deletes them.
///
/// # Arguments
///
/// * `report_dir` - Directory containing report files
/// * `max_age` - Maximum age for report files (files older than this are deleted)
///
/// # Returns
///
/// Returns `CleanReportResult` with counts of deleted and skipped files.
///
/// # Errors
///
/// Returns error if directory operations fail or if file metadata cannot be read.
pub fn clean_report_files(report_dir: &Path, max_age: &Duration) -> Result<CleanReportResult> {
    // If directory doesn't exist, nothing to do
    if !report_dir.exists() {
        return Ok(CleanReportResult::new(0, 0));
    }

    let now = Utc::now();
    let mut deleted = 0;
    let mut skipped = 0;

    // Read directory entries
    let entries = fs::read_dir(report_dir)
        .with_context(|| format!("Failed to read report directory: {}", report_dir.display()))?;

    for entry in entries {
        let entry = entry.context("Failed to read directory entry")?;
        let path = entry.path();

        // Only process .json and .md files
        if let Some(ext) = path.extension() {
            let ext_str = ext.to_string_lossy();
            if ext_str != "json" && ext_str != "md" {
                continue;
            }
        } else {
            // No extension, skip
            continue;
        }

        // Get file metadata to check modification time
        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                // File might have been deleted concurrently, just warn and continue
                tracing::warn!("Failed to get metadata for {}: {}", path.display(), e);
                continue;
            }
        };

        // Check if file is old enough to delete
        let modified = metadata
            .modified()
            .context("Failed to get file modification time")?;

        let modified_datetime: DateTime<Utc> = modified.into();
        let age = now.signed_duration_since(modified_datetime);

        if age > *max_age {
            // Delete the file
            match fs::remove_file(&path) {
                Ok(()) => {
                    deleted += 1;
                    tracing::debug!("Deleted old report file: {}", path.display());
                }
                Err(e) => {
                    // Log but don't fail the entire operation
                    tracing::warn!("Failed to delete report file {}: {}", path.display(), e);
                }
            }
        } else {
            skipped += 1;
        }
    }

    Ok(CleanReportResult::new(deleted, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_hours() {
        let duration = parse_duration("24h").unwrap();
        assert_eq!(duration, Duration::hours(24));
    }

    #[test]
    fn test_parse_duration_days() {
        let duration = parse_duration("7d").unwrap();
        assert_eq!(duration, Duration::days(7));
    }

    #[test]
    fn test_parse_duration_large_values() {
        let duration = parse_duration("168h").unwrap();
        assert_eq!(duration, Duration::hours(168));

        let duration = parse_duration("30d").unwrap();
        assert_eq!(duration, Duration::days(30));
    }

    #[test]
    fn test_parse_duration_invalid() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("24").is_err());
        assert!(parse_duration("24m").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn test_is_expired_by_age() {
        let now = Utc::now();
        let max_age = Duration::days(7);

        // Message from 10 days ago (expired)
        let old_message = InboxMessage {
            from: "test".to_string(),
            text: "old message".to_string(),
            timestamp: (now - Duration::days(10)).to_rfc3339(),
            read: false,
            summary: None,
            message_id: None,
            unknown_fields: std::collections::HashMap::new(),
        };
        assert!(is_expired_by_age(&old_message, &max_age, now));

        // Message from 3 days ago (not expired)
        let recent_message = InboxMessage {
            from: "test".to_string(),
            text: "recent message".to_string(),
            timestamp: (now - Duration::days(3)).to_rfc3339(),
            read: false,
            summary: None,
            message_id: None,
            unknown_fields: std::collections::HashMap::new(),
        };
        assert!(!is_expired_by_age(&recent_message, &max_age, now));
    }

    #[test]
    fn test_retention_result() {
        let result = RetentionResult::new(10, 5, 5);
        assert_eq!(result.kept, 10);
        assert_eq!(result.removed, 5);
        assert_eq!(result.archived, 5);
    }

    #[test]
    fn test_clean_report_files_deletes_old_files() {
        use tempfile::TempDir;
        use std::fs::File;
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();
        let report_dir = temp_dir.path().join("reports");
        fs::create_dir_all(&report_dir).unwrap();

        // Create old report files
        let old_json = report_dir.join("old.json");
        let old_md = report_dir.join("old.md");
        File::create(&old_json).unwrap().write_all(b"{}").unwrap();
        File::create(&old_md).unwrap().write_all(b"# Report").unwrap();

        // Wait a bit to ensure files have a measurable age
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Clean files older than 50ms (both should be deleted)
        let max_age = Duration::milliseconds(50);
        let result = super::clean_report_files(&report_dir, &max_age).unwrap();

        assert_eq!(result.deleted_count, 2);
        assert_eq!(result.skipped_count, 0);
        assert!(!old_json.exists());
        assert!(!old_md.exists());
    }

    #[test]
    fn test_clean_report_files_skips_recent_files() {
        use tempfile::TempDir;
        use std::fs::File;
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();
        let report_dir = temp_dir.path().join("reports");
        fs::create_dir_all(&report_dir).unwrap();

        // Create recent report files
        let recent_json = report_dir.join("recent.json");
        let recent_md = report_dir.join("recent.md");
        File::create(&recent_json).unwrap().write_all(b"{}").unwrap();
        File::create(&recent_md).unwrap().write_all(b"# Report").unwrap();

        // Clean files older than 1 hour (both should be skipped)
        let max_age = Duration::hours(1);
        let result = super::clean_report_files(&report_dir, &max_age).unwrap();

        assert_eq!(result.deleted_count, 0);
        assert_eq!(result.skipped_count, 2);
        assert!(recent_json.exists());
        assert!(recent_md.exists());
    }

    #[test]
    fn test_clean_report_files_empty_directory() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let report_dir = temp_dir.path().join("reports");
        fs::create_dir_all(&report_dir).unwrap();

        let max_age = Duration::hours(1);
        let result = super::clean_report_files(&report_dir, &max_age).unwrap();

        assert_eq!(result.deleted_count, 0);
        assert_eq!(result.skipped_count, 0);
    }

    #[test]
    fn test_clean_report_files_nonexistent_directory() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let report_dir = temp_dir.path().join("nonexistent");

        let max_age = Duration::hours(1);
        let result = super::clean_report_files(&report_dir, &max_age).unwrap();

        assert_eq!(result.deleted_count, 0);
        assert_eq!(result.skipped_count, 0);
    }

    #[test]
    fn test_clean_report_files_only_targets_json_and_md() {
        use tempfile::TempDir;
        use std::fs::File;
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();
        let report_dir = temp_dir.path().join("reports");
        fs::create_dir_all(&report_dir).unwrap();

        // Create various file types
        let json_file = report_dir.join("report.json");
        let md_file = report_dir.join("report.md");
        let txt_file = report_dir.join("report.txt");
        let log_file = report_dir.join("report.log");

        File::create(&json_file).unwrap().write_all(b"{}").unwrap();
        File::create(&md_file).unwrap().write_all(b"# Report").unwrap();
        File::create(&txt_file).unwrap().write_all(b"text").unwrap();
        File::create(&log_file).unwrap().write_all(b"log").unwrap();

        // Wait for files to age
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Clean files older than 50ms
        let max_age = Duration::milliseconds(50);
        let result = super::clean_report_files(&report_dir, &max_age).unwrap();

        // Only json and md should be deleted
        assert_eq!(result.deleted_count, 2);
        assert!(!json_file.exists());
        assert!(!md_file.exists());
        assert!(txt_file.exists()); // Should still exist
        assert!(log_file.exists()); // Should still exist
    }

    #[test]
    fn test_clean_report_result() {
        let result = CleanReportResult::new(5, 10);
        assert_eq!(result.deleted_count, 5);
        assert_eq!(result.skipped_count, 10);
    }

    #[test]
    fn test_parse_inbox_filename_simple() {
        // Simple agent name: "agent.json"
        let filename = "agent.json";
        let base = &filename[..filename.len() - 5];
        assert_eq!(base, "agent");
    }

    #[test]
    fn test_parse_inbox_filename_with_hostname() {
        // Bridge inbox: "agent.hostname.json"
        let filename = "agent.hostname.json";
        let base = &filename[..filename.len() - 5];
        assert_eq!(base, "agent.hostname");
        // Retention uses the entire base as agent name
    }

    #[test]
    fn test_parse_inbox_filename_dotted_agent() {
        // Agent name with dots: "dotted.agent.name.json"
        let filename = "dotted.agent.name.json";
        let base = &filename[..filename.len() - 5];
        assert_eq!(base, "dotted.agent.name");
        // Retention uses the entire base as agent name
    }

    #[test]
    fn test_parse_inbox_filename_dotted_agent_with_hostname() {
        // Bridge inbox with dotted agent: "dotted.agent.hostname.json"
        let filename = "dotted.agent.hostname.json";
        let base = &filename[..filename.len() - 5];
        assert_eq!(base, "dotted.agent.hostname");
        // Retention uses the entire base as agent name
    }
}
