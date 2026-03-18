//! Daemon status file writer
//!
//! Writes daemon status to `${ATM_HOME}/.atm/daemon/status.json` for CLI consumption.
//! Status includes daemon PID, uptime, plugin states, and last update timestamp.

use agent_team_mail_core::daemon_client::{
    DaemonTouchEntry, DaemonTouchSnapshot, RuntimeOwnerMetadata, daemon_touch_path_for,
};
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
    /// Runtime owner metadata for shared-runtime admission and diagnostics.
    #[serde(default)]
    pub owner: RuntimeOwnerMetadata,
    /// Plugin statuses
    pub plugins: Vec<PluginStatus>,
    /// Active teams being monitored
    pub teams: Vec<String>,
    /// Logging pipeline health snapshot
    #[serde(default)]
    pub logging: LoggingHealth,
    /// OTel exporter health snapshot.
    #[serde(default)]
    pub otel: OtelHealth,
}

/// Logging pipeline health snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingHealth {
    /// Logging health state (`healthy|degraded_spooling|degraded_dropping|unavailable`)
    pub state: String,
    /// Total dropped log events from daemon queue.
    pub dropped_counter: u64,
    /// Active spool path used for producer fallback.
    pub spool_path: String,
    /// Last logging error if known.
    pub last_error: Option<String>,
    /// Canonical JSONL sink path.
    pub canonical_log_path: String,
    /// Number of pending spool files.
    pub spool_count: u64,
    /// Oldest spool file age in seconds (if spool files exist).
    pub oldest_spool_age: Option<u64>,
}

/// OTel exporter health snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OtelHealth {
    pub schema_version: String,
    pub enabled: bool,
    pub collector_endpoint: Option<String>,
    pub protocol: String,
    pub collector_state: String,
    pub local_mirror_state: String,
    pub local_mirror_path: String,
    pub debug_local_export: bool,
    pub debug_local_state: String,
    pub last_error: OtelLastError,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OtelLastError {
    pub code: Option<String>,
    pub message: Option<String>,
    pub at: Option<String>,
}

impl Default for OtelHealth {
    fn default() -> Self {
        Self {
            schema_version: "v1".to_string(),
            enabled: true,
            collector_endpoint: None,
            protocol: "otlp_http".to_string(),
            collector_state: "not_configured".to_string(),
            local_mirror_state: "healthy".to_string(),
            local_mirror_path: String::new(),
            debug_local_export: false,
            debug_local_state: "disabled".to_string(),
            last_error: OtelLastError::default(),
        }
    }
}

impl From<crate::daemon::observability::OtelHealthSnapshot> for OtelHealth {
    fn from(value: crate::daemon::observability::OtelHealthSnapshot) -> Self {
        Self {
            schema_version: value.schema_version,
            enabled: value.enabled,
            collector_endpoint: value.collector_endpoint,
            protocol: value.protocol,
            collector_state: value.collector_state,
            local_mirror_state: value.local_mirror_state,
            local_mirror_path: value.local_mirror_path,
            debug_local_export: value.debug_local_export,
            debug_local_state: value.debug_local_state,
            last_error: OtelLastError {
                code: value.last_error.code,
                message: value.last_error.message,
                at: value.last_error.at,
            },
        }
    }
}

impl Default for LoggingHealth {
    fn default() -> Self {
        Self {
            state: "unavailable".to_string(),
            dropped_counter: 0,
            spool_path: String::new(),
            last_error: None,
            canonical_log_path: String::new(),
            spool_count: 0,
            oldest_spool_age: None,
        }
    }
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
    /// Plugin failed to initialize and is disabled for this daemon run
    #[serde(rename = "disabled_init_error")]
    DisabledInitError,
}

/// Status file writer that tracks daemon state
pub struct StatusWriter {
    /// Path to status.json file
    status_path: PathBuf,
    /// Path to daemon-touch.json startup sidecar
    touch_path: PathBuf,
    /// Daemon start time for uptime calculation
    start_time: SystemTime,
    /// RFC3339 startup timestamp reused for daemon-touch sidecar rows.
    started_at: String,
    /// Daemon version
    version: String,
    /// Runtime owner metadata
    owner: RuntimeOwnerMetadata,
}

impl StatusWriter {
    /// Create a new status writer
    ///
    /// # Arguments
    ///
    /// * `home_dir` - ATM home directory (from `get_home_dir()`)
    /// * `version` - Daemon version string
    pub fn new(home_dir: PathBuf, version: String, owner: RuntimeOwnerMetadata) -> Self {
        let daemon_dir = home_dir.join(".atm/daemon");
        let status_path = daemon_dir.join("status.json");
        let touch_path = daemon_touch_path_for(&home_dir);
        let start_time = SystemTime::now();
        let started_at = format_timestamp(start_time);

        Self {
            status_path,
            touch_path,
            start_time,
            started_at,
            version,
            owner,
        }
    }

    /// Write a single-writer daemon startup sidecar under `${ATM_HOME}/.atm/daemon/`.
    ///
    /// This follows the Phase AO runtime path audit convention: shared-runtime
    /// daemon ownership files live under `${ATM_HOME}/.atm/daemon/` and are
    /// written atomically by one writer while readers snapshot-read them.
    pub fn write_daemon_touch(&self, teams: &[String]) -> Result<()> {
        use agent_team_mail_core::io::atomic::atomic_swap;
        use std::io::Write;

        if let Some(parent) = self.touch_path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create daemon touch directory")?;
        }

        let snapshot: DaemonTouchSnapshot = teams
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|team| {
                (
                    team,
                    DaemonTouchEntry {
                        pid: std::process::id(),
                        started_at: self.started_at.clone(),
                        binary: self.owner.executable_path.clone(),
                    },
                )
            })
            .collect();

        let json = serde_json::to_vec_pretty(&snapshot)
            .context("Failed to serialize daemon touch snapshot")?;
        let tmp_path = self.touch_path.with_extension("json.tmp");
        let mut tmp_file =
            std::fs::File::create(&tmp_path).context("Failed to create daemon touch temp file")?;
        tmp_file
            .write_all(&json)
            .context("Failed to write daemon touch temp file")?;
        tmp_file
            .sync_all()
            .context("Failed to fsync daemon touch temp file")?;
        drop(tmp_file);

        if !self.touch_path.exists() {
            let placeholder = std::fs::File::create(&self.touch_path)
                .context("Failed to create daemon touch placeholder")?;
            placeholder
                .sync_all()
                .context("Failed to fsync daemon touch placeholder")?;
        }

        atomic_swap(&self.touch_path, &tmp_path)
            .context("Failed to atomically swap daemon touch snapshot")?;
        if tmp_path.exists() {
            std::fs::remove_file(&tmp_path).context("Failed to remove daemon touch temp file")?;
        }

        Ok(())
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
    pub fn write_status(
        &self,
        plugins: Vec<PluginStatus>,
        teams: Vec<String>,
        logging: LoggingHealth,
        otel: OtelHealth,
    ) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = self.status_path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create daemon status directory")?;
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
            owner: self.owner.clone(),
            plugins,
            teams,
            logging,
            otel,
        };

        // Serialize to JSON
        let json =
            serde_json::to_string_pretty(&status).context("Failed to serialize daemon status")?;

        // Atomic write: temp file + rename
        // On Windows, std::fs::rename doesn't replace existing files, so we remove first
        let temp_path = self.status_path.with_extension("json.tmp");
        std::fs::write(&temp_path, json.as_bytes()).context("Failed to write status temp file")?;

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
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = duration.as_secs();
    let nanos = duration.subsec_nanos();

    // Format as ISO 8601: YYYY-MM-DDTHH:MM:SSZ
    // Use chrono for proper formatting
    use chrono::{DateTime, Utc};
    let dt = DateTime::<Utc>::from_timestamp(secs as i64, nanos).unwrap_or_else(Utc::now);
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_core::daemon_client::{BuildProfile, RuntimeKind};
    use tempfile::TempDir;

    fn logging_health() -> LoggingHealth {
        LoggingHealth {
            state: "healthy".to_string(),
            dropped_counter: 0,
            spool_path: "log-spool".to_string(),
            last_error: None,
            canonical_log_path: "atm.log.jsonl".to_string(),
            spool_count: 0,
            oldest_spool_age: None,
        }
    }

    fn otel_health() -> OtelHealth {
        OtelHealth {
            schema_version: "v1".to_string(),
            enabled: true,
            collector_endpoint: Some("http://collector:4318".to_string()),
            protocol: "otlp_http".to_string(),
            collector_state: "healthy".to_string(),
            local_mirror_state: "healthy".to_string(),
            local_mirror_path: "atm.log.otel.jsonl".to_string(),
            debug_local_export: false,
            debug_local_state: "disabled".to_string(),
            last_error: OtelLastError::default(),
        }
    }

    fn runtime_owner(home: &std::path::Path) -> RuntimeOwnerMetadata {
        RuntimeOwnerMetadata {
            runtime_kind: RuntimeKind::Isolated,
            build_profile: BuildProfile::Release,
            executable_path: home.join("atm-daemon").to_string_lossy().into_owned(),
            home_scope: home.to_string_lossy().into_owned(),
        }
    }

    #[test]
    fn test_status_writer_creates_file() {
        let temp_dir = TempDir::new().unwrap();
        let writer = StatusWriter::new(
            temp_dir.path().to_path_buf(),
            "0.8.0".to_string(),
            runtime_owner(temp_dir.path()),
        );

        let plugins = vec![PluginStatus {
            name: "test_plugin".to_string(),
            enabled: true,
            status: PluginStatusKind::Running,
            last_error: None,
            last_updated: Some(format_timestamp(SystemTime::now())),
        }];

        let teams = vec!["test-team".to_string()];

        writer
            .write_status(plugins, teams, logging_health(), otel_health())
            .unwrap();

        assert!(writer.status_path().exists());
    }

    #[test]
    fn test_status_writer_writes_daemon_touch_snapshot() {
        let temp_dir = TempDir::new().unwrap();
        let writer = StatusWriter::new(
            temp_dir.path().to_path_buf(),
            "0.8.0".to_string(),
            runtime_owner(temp_dir.path()),
        );

        writer
            .write_daemon_touch(&["atm-dev".to_string(), "qa-team".to_string()])
            .expect("write daemon touch");

        let raw =
            std::fs::read_to_string(temp_dir.path().join(".atm/daemon/daemon-touch.json")).unwrap();
        let snapshot: DaemonTouchSnapshot = serde_json::from_str(&raw).unwrap();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot["atm-dev"].pid, std::process::id());
        assert_eq!(
            snapshot["atm-dev"].binary,
            temp_dir.path().join("atm-daemon").to_string_lossy()
        );
        assert_eq!(
            snapshot["qa-team"].started_at,
            snapshot["atm-dev"].started_at
        );
    }

    #[test]
    fn test_status_writer_atomic_write() {
        let temp_dir = TempDir::new().unwrap();
        let writer = StatusWriter::new(
            temp_dir.path().to_path_buf(),
            "0.8.0".to_string(),
            runtime_owner(temp_dir.path()),
        );

        let plugins = vec![];
        let teams = vec![];

        // First write
        writer
            .write_status(
                plugins.clone(),
                teams.clone(),
                logging_health(),
                otel_health(),
            )
            .unwrap();
        let first_content = std::fs::read_to_string(writer.status_path()).unwrap();

        // Second write (should overwrite atomically)
        // Sleep for more than 1 second to ensure timestamp changes
        std::thread::sleep(std::time::Duration::from_millis(1100));
        writer
            .write_status(plugins, teams, logging_health(), otel_health())
            .unwrap();
        let second_content = std::fs::read_to_string(writer.status_path()).unwrap();

        // Content should differ (timestamps changed)
        assert_ne!(first_content, second_content);
    }

    #[test]
    fn test_status_writer_correct_json_structure() {
        let temp_dir = TempDir::new().unwrap();
        let writer = StatusWriter::new(
            temp_dir.path().to_path_buf(),
            "0.8.0".to_string(),
            runtime_owner(temp_dir.path()),
        );

        let plugins = vec![
            PluginStatus {
                name: "gh_monitor".to_string(),
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

        writer
            .write_status(
                plugins.clone(),
                teams.clone(),
                logging_health(),
                otel_health(),
            )
            .unwrap();

        // Read back and verify structure
        let content = std::fs::read_to_string(writer.status_path()).unwrap();
        let status: DaemonStatus = serde_json::from_str(&content).unwrap();

        assert_eq!(status.version, "0.8.0");
        assert_eq!(status.pid, std::process::id());
        assert_eq!(status.plugins.len(), 2);
        assert_eq!(status.teams.len(), 1);
        assert_eq!(status.plugins[0].name, "gh_monitor");
        assert!(status.plugins[0].enabled);
        assert_eq!(status.plugins[0].status, PluginStatusKind::Running);
        assert_eq!(status.plugins[1].name, "issues");
        assert!(!status.plugins[1].enabled);
        assert_eq!(status.plugins[1].status, PluginStatusKind::Disabled);
        assert_eq!(status.logging.state, "healthy");
        assert_eq!(status.logging.spool_count, 0);
        assert!(status.logging.canonical_log_path.contains("atm.log"));
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
        let writer = StatusWriter::new(
            temp_dir.path().to_path_buf(),
            "0.8.0".to_string(),
            runtime_owner(temp_dir.path()),
        );

        let plugins = vec![];
        let teams = vec![];

        // First write
        writer
            .write_status(
                plugins.clone(),
                teams.clone(),
                logging_health(),
                otel_health(),
            )
            .unwrap();
        let first_content = std::fs::read_to_string(writer.status_path()).unwrap();
        let first_status: DaemonStatus = serde_json::from_str(&first_content).unwrap();

        // Wait a bit
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Second write
        writer
            .write_status(plugins, teams, logging_health(), otel_health())
            .unwrap();
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
