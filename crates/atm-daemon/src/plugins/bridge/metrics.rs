//! Bridge plugin metrics tracking

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Bridge metrics tracking
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BridgeMetrics {
    /// Total number of sync cycles completed
    pub total_syncs: u64,

    /// Total messages pushed across all remotes
    pub total_pushed: u64,

    /// Total messages pulled from all remotes
    pub total_pulled: u64,

    /// Total errors encountered
    pub total_errors: u64,

    /// Last sync timestamp (milliseconds since epoch)
    pub last_sync_time: Option<u64>,

    /// Per-remote failure counts (for circuit breaker)
    #[serde(default)]
    pub remote_failures: HashMap<String, u64>,

    /// Remotes temporarily disabled due to repeated failures
    #[serde(default)]
    pub disabled_remotes: Vec<String>,
}

impl BridgeMetrics {
    /// Create a new metrics instance
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a sync cycle completion
    pub fn record_sync(&mut self, pushed: usize, pulled: usize, errors: usize) {
        self.total_syncs += 1;
        self.total_pushed += pushed as u64;
        self.total_pulled += pulled as u64;
        self.total_errors += errors as u64;
        self.last_sync_time = Some(current_time_ms());
    }

    /// Record a failure for a specific remote
    pub fn record_remote_failure(&mut self, remote: &str) {
        let count = self.remote_failures.entry(remote.to_string()).or_insert(0);
        *count += 1;
    }

    /// Reset failure count for a remote (after successful sync)
    pub fn reset_remote_failures(&mut self, remote: &str) {
        self.remote_failures.remove(remote);
        self.disabled_remotes.retain(|r| r != remote);
    }

    /// Get failure count for a remote
    pub fn get_remote_failures(&self, remote: &str) -> u64 {
        self.remote_failures.get(remote).copied().unwrap_or(0)
    }

    /// Mark a remote as temporarily disabled
    pub fn disable_remote(&mut self, remote: &str) {
        if !self.disabled_remotes.contains(&remote.to_string()) {
            self.disabled_remotes.push(remote.to_string());
        }
    }

    /// Check if a remote is disabled
    pub fn is_remote_disabled(&self, remote: &str) -> bool {
        self.disabled_remotes.contains(&remote.to_string())
    }

    /// Load metrics from file
    pub async fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        use tokio::fs;

        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(path).await?;
        let metrics: Self = serde_json::from_str(&content)?;
        Ok(metrics)
    }

    /// Save metrics to file
    pub async fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        use tokio::fs;

        let json = serde_json::to_string_pretty(self)?;
        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, json).await?;
        fs::rename(&temp_path, path).await?;
        Ok(())
    }
}

/// Get current time in milliseconds since epoch
fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_new() {
        let metrics = BridgeMetrics::new();
        assert_eq!(metrics.total_syncs, 0);
        assert_eq!(metrics.total_pushed, 0);
        assert_eq!(metrics.total_pulled, 0);
        assert_eq!(metrics.total_errors, 0);
        assert!(metrics.last_sync_time.is_none());
    }

    #[test]
    fn test_record_sync() {
        let mut metrics = BridgeMetrics::new();
        metrics.record_sync(5, 3, 1);

        assert_eq!(metrics.total_syncs, 1);
        assert_eq!(metrics.total_pushed, 5);
        assert_eq!(metrics.total_pulled, 3);
        assert_eq!(metrics.total_errors, 1);
        assert!(metrics.last_sync_time.is_some());

        metrics.record_sync(2, 4, 0);
        assert_eq!(metrics.total_syncs, 2);
        assert_eq!(metrics.total_pushed, 7);
        assert_eq!(metrics.total_pulled, 7);
        assert_eq!(metrics.total_errors, 1);
    }

    #[test]
    fn test_remote_failure_tracking() {
        let mut metrics = BridgeMetrics::new();

        metrics.record_remote_failure("remote1");
        assert_eq!(metrics.get_remote_failures("remote1"), 1);

        metrics.record_remote_failure("remote1");
        assert_eq!(metrics.get_remote_failures("remote1"), 2);

        metrics.record_remote_failure("remote2");
        assert_eq!(metrics.get_remote_failures("remote2"), 1);
        assert_eq!(metrics.get_remote_failures("remote1"), 2);

        metrics.reset_remote_failures("remote1");
        assert_eq!(metrics.get_remote_failures("remote1"), 0);
        assert_eq!(metrics.get_remote_failures("remote2"), 1);
    }

    #[test]
    fn test_disable_remote() {
        let mut metrics = BridgeMetrics::new();

        assert!(!metrics.is_remote_disabled("remote1"));

        metrics.disable_remote("remote1");
        assert!(metrics.is_remote_disabled("remote1"));

        // Disabling twice should not duplicate
        metrics.disable_remote("remote1");
        assert_eq!(metrics.disabled_remotes.len(), 1);

        metrics.reset_remote_failures("remote1");
        assert!(!metrics.is_remote_disabled("remote1"));
    }

    #[test]
    fn test_serialization() {
        let mut metrics = BridgeMetrics::new();
        metrics.record_sync(10, 5, 0);
        metrics.record_remote_failure("remote1");
        metrics.disable_remote("remote1");

        let json = serde_json::to_string(&metrics).unwrap();
        let deserialized: BridgeMetrics = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.total_syncs, 1);
        assert_eq!(deserialized.total_pushed, 10);
        assert_eq!(deserialized.total_pulled, 5);
        assert!(deserialized.is_remote_disabled("remote1"));
        assert_eq!(deserialized.get_remote_failures("remote1"), 1);
    }
}
