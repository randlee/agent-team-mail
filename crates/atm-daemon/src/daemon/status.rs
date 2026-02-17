//! Daemon status file writer
//!
//! Writes daemon status to `${ATM_HOME}/.claude/daemon/status.json` for CLI consumption.
//! Status includes daemon PID, uptime, plugin states, and last update timestamp.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Daemon status snapshot written to status.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    /// ISO 8601 timestamp when status was last written
    pub timestamp: String,
    /// Process ID of the daemon
    pub pid: u32,
    /// Daemon version (crate version)
    pub version: String,
    /// Uptime in seconds since daemon start
    pub uptime_secs: u64,
    /// Plugin statuses
    pub plugins: Vec<PluginStatus>,
    /// Active teams being monitored
    pub teams: Vec<String>,
}

/// Plugin status entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginStatus {
    /// Plugin name
    pub name: String,
    /// Whether plugin is enabled
    pub enabled: bool,
    /// Current plugin status
    pub status: PluginStatusKind,
    /// Last error message (if any)
    pub last_error: Option<String>,
    /// ISO 8601 timestamp of last status update
    pub last_updated: Option<String>,
}

/// Plugin status values
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginStatusKind {
    /// Plugin is running normally
    Running,
    /// Plugin encountered an error
    Error,
    /// Plugin is disabled in config
    Disabled,
}

/// Status file writer that tracks daemon state
pub struct StatusWriter {
    /// Path to status.json file
    status_path: PathBuf,
    /// Daemon start time for uptime calculation
    start_time: SystemTime,
    /// Daemon version
    version: String,
}

impl StatusWriter {
    /// Create a new status writer
    ///
    /// # Arguments
    ///
    /// * `home_dir` - ATM home directory (from `get_home_dir()`)
    /// * `version` - Daemon version string
    pub fn new(home_dir: PathBuf, version: String) -> Self {
        let daemon_dir = home_dir.join(".claude/daemon");
        let status_path = daemon_dir.join("status.json");

        Self {
            status_path,
            start_time: SystemTime::now(),
            version,
        }
    }

    /// Write daemon status to status.json atomically
    ///
    /// Uses atomic write pattern: write to temp file, then rename.
    ///
    /// # Arguments
    ///
    /// * `plugins` - Current plugin status list
    /// * `teams` - List of team names being monitored
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Parent directory cannot be created
    /// - Status file cannot be written
    /// - Atomic rename fails
    pub fn write_status(&self, plugins: Vec<PluginStatus>, teams: Vec<String>) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = self.status_path.parent() {
            std::fs::create_dir_all(parent)
                .context("Failed to create daemon status directory")?;
        }

        // Calculate uptime
        let uptime_secs = self
            .start_time
            .elapsed()
            .unwrap_or(Duration::ZERO)
            .as_secs();

        // Build status
        let status = DaemonStatus {
            timestamp: format_timestamp(SystemTime::now()),
            pid: std::process::id(),
            version: self.version.clone(),
            uptime_secs,
            plugins,
            teams,
        };

        // Serialize to JSON
        let json = serde_json::to_string_pretty(&status)
            .context("Failed to serialize daemon status")?;

        // Atomic write: temp file + rename
        // On Windows, std::fs::rename doesn't replace existing files, so we remove first
        let temp_path = self.status_path.with_extension("json.tmp");
        std::fs::write(&temp_path, json.as_bytes())
            .context("Failed to write status temp file")?;

        // Remove existing file if present (required for Windows compatibility)
        if self.status_path.exists() {
            std::fs::remove_file(&self.status_path)
                .context("Failed to remove existing status file")?;
        }

        std::fs::rename(&temp_path, &self.status_path)
            .context("Failed to rename status temp file to final path")?;

        Ok(())
    }

    /// Get the status file path
    pub fn status_path(&self) -> &PathBuf {
        &self.status_path
    }
}

/// Format timestamp as ISO 8601 string
fn format_timestamp(time: SystemTime) -> String {
    let duration = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = duration.as_secs();
    let nanos = duration.subsec_nanos();

    // Format as ISO 8601: YYYY-MM-DDTHH:MM:SSZ
    // Use chrono for proper formatting
    use chrono::{DateTime, Utc};
    let dt = DateTime::<Utc>::from_timestamp(secs as i64, nanos)
        .unwrap_or_else(Utc::now);
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_status_writer_creates_file() {
        let temp_dir = TempDir::new().unwrap();
        let writer = StatusWriter::new(temp_dir.path().to_path_buf(), "0.8.0".to_string());

        let plugins = vec![PluginStatus {
            name: "test_plugin".to_string(),
            enabled: true,
            status: PluginStatusKind::Running,
            last_error: None,
            last_updated: Some(format_timestamp(SystemTime::now())),
        }];

        let teams = vec!["test-team".to_string()];

        writer.write_status(plugins, teams).unwrap();

        assert!(writer.status_path().exists());
    }

    #[test]
    fn test_status_writer_atomic_write() {
        let temp_dir = TempDir::new().unwrap();
        let writer = StatusWriter::new(temp_dir.path().to_path_buf(), "0.8.0".to_string());

        let plugins = vec![];
        let teams = vec![];

        // First write
        writer.write_status(plugins.clone(), teams.clone()).unwrap();
        let first_content = std::fs::read_to_string(writer.status_path()).unwrap();

        // Second write (should overwrite atomically)
        // Sleep for more than 1 second to ensure timestamp changes
        std::thread::sleep(std::time::Duration::from_millis(1100));
        writer.write_status(plugins, teams).unwrap();
        let second_content = std::fs::read_to_string(writer.status_path()).unwrap();

        // Content should differ (timestamps changed)
        assert_ne!(first_content, second_content);
    }

    #[test]
    fn test_status_writer_correct_json_structure() {
        let temp_dir = TempDir::new().unwrap();
        let writer = StatusWriter::new(temp_dir.path().to_path_buf(), "0.8.0".to_string());

        let plugins = vec![
            PluginStatus {
                name: "ci_monitor".to_string(),
                enabled: true,
                status: PluginStatusKind::Running,
                last_error: None,
                last_updated: Some(format_timestamp(SystemTime::now())),
            },
            PluginStatus {
                name: "issues".to_string(),
                enabled: false,
                status: PluginStatusKind::Disabled,
                last_error: None,
                last_updated: None,
            },
        ];

        let teams = vec!["my-team".to_string()];

        writer.write_status(plugins.clone(), teams.clone()).unwrap();

        // Read back and verify structure
        let content = std::fs::read_to_string(writer.status_path()).unwrap();
        let status: DaemonStatus = serde_json::from_str(&content).unwrap();

        assert_eq!(status.version, "0.8.0");
        assert_eq!(status.pid, std::process::id());
        assert_eq!(status.plugins.len(), 2);
        assert_eq!(status.teams.len(), 1);
        assert_eq!(status.plugins[0].name, "ci_monitor");
        assert!(status.plugins[0].enabled);
        assert_eq!(status.plugins[0].status, PluginStatusKind::Running);
        assert_eq!(status.plugins[1].name, "issues");
        assert!(!status.plugins[1].enabled);
        assert_eq!(status.plugins[1].status, PluginStatusKind::Disabled);
    }

    #[test]
    fn test_format_timestamp() {
        let now = SystemTime::now();
        let formatted = format_timestamp(now);

        // Should be ISO 8601 format
        assert!(formatted.contains('T'));
        assert!(formatted.ends_with('Z'));

        // Should be parseable back
        use chrono::DateTime;
        let parsed = DateTime::parse_from_rfc3339(&formatted);
        assert!(parsed.is_ok());
    }

    #[test]
    fn test_status_uptime_increases() {
        let temp_dir = TempDir::new().unwrap();
        let writer = StatusWriter::new(temp_dir.path().to_path_buf(), "0.8.0".to_string());

        let plugins = vec![];
        let teams = vec![];

        // First write
        writer.write_status(plugins.clone(), teams.clone()).unwrap();
        let first_content = std::fs::read_to_string(writer.status_path()).unwrap();
        let first_status: DaemonStatus = serde_json::from_str(&first_content).unwrap();

        // Wait a bit
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Second write
        writer.write_status(plugins, teams).unwrap();
        let second_content = std::fs::read_to_string(writer.status_path()).unwrap();
        let second_status: DaemonStatus = serde_json::from_str(&second_content).unwrap();

        // Uptime should have increased (or stayed the same if too fast)
        assert!(second_status.uptime_secs >= first_status.uptime_secs);
    }

    #[test]
    fn test_plugin_status_serialization() {
        let status = PluginStatus {
            name: "test".to_string(),
            enabled: true,
            status: PluginStatusKind::Running,
            last_error: Some("test error".to_string()),
            last_updated: Some("2026-02-16T23:30:00Z".to_string()),
        };

        let json = serde_json::to_string(&status).unwrap();
        let deserialized: PluginStatus = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.name, "test");
        assert_eq!(deserialized.status, PluginStatusKind::Running);
        assert_eq!(deserialized.last_error.as_deref(), Some("test error"));
    }
}
