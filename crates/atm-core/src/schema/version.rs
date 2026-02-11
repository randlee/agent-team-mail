//! Schema version detection for Claude Code compatibility

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Schema version detection for Claude Code compatibility
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemaVersion {
    /// Pre-release (2.x) — may change without notice
    PreRelease { claude_version: String },
    /// Post-release (3.x+) — stable, breaking changes unlikely
    Stable { claude_version: String },
    /// Unknown — best effort with latest known schema
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VersionCache {
    version: String,
    detected_at_secs: u64,
    ttl_hours: u64,
}

impl SchemaVersion {
    const DEFAULT_TTL_HOURS: u64 = 24;

    pub fn detect() -> Self {
        if let Some(cached) = Self::read_cache() {
            let now_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::from_secs(0))
                .as_secs();
            let cache_age_hours = (now_secs.saturating_sub(cached.detected_at_secs)) / 3600;

            if cache_age_hours < cached.ttl_hours {
                tracing::debug!(
                    version = %cached.version,
                    age_hours = cache_age_hours,
                    "Using cached Claude Code version"
                );
                return Self::parse_version(&cached.version);
            }
        }
        Self::detect_fresh()
    }

    pub fn detect_fresh() -> Self {
        match Self::run_version_command() {
            Ok(version_str) => {
                tracing::info!(version = %version_str, "Detected Claude Code version");
                if let Err(e) = Self::write_cache(&version_str, Self::DEFAULT_TTL_HOURS) {
                    tracing::warn!(error = %e, "Failed to write version cache");
                }
                Self::parse_version(&version_str)
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to detect Claude Code version");
                SchemaVersion::Unknown
            }
        }
    }

    fn run_version_command() -> Result<String, io::Error> {
        let output = Command::new("claude").arg("--version").output()?;
        if !output.status.success() {
            return Err(io::Error::other("claude --version failed"));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let version = stdout
            .split_whitespace()
            .last()
            .ok_or_else(|| io::Error::other("Could not parse version"))?
            .to_string();
        Ok(version)
    }

    fn parse_version(version_str: &str) -> Self {
        let major_version = version_str
            .split('.')
            .next()
            .and_then(|s| s.parse::<u32>().ok());

        match major_version {
            Some(2) => SchemaVersion::PreRelease {
                claude_version: version_str.to_string(),
            },
            Some(major) if major >= 3 => SchemaVersion::Stable {
                claude_version: version_str.to_string(),
            },
            _ => {
                tracing::warn!(version = %version_str, "Unknown version format");
                SchemaVersion::Unknown
            }
        }
    }

    fn cache_path() -> PathBuf {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".config").join("atm").join("claude-version.json")
    }

    fn read_cache() -> Option<VersionCache> {
        let path = Self::cache_path();
        let contents = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&contents).ok()
    }

    fn write_cache(version: &str, ttl_hours: u64) -> Result<(), io::Error> {
        let path = Self::cache_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::from_secs(0))
            .as_secs();
        let cache = VersionCache {
            version: version.to_string(),
            detected_at_secs: now_secs,
            ttl_hours,
        };
        let json = serde_json::to_string_pretty(&cache)?;
        fs::write(&path, json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_pre_release() {
        let version = SchemaVersion::parse_version("2.1.39");
        assert!(matches!(version, SchemaVersion::PreRelease { .. }));
    }

    #[test]
    fn test_parse_version_stable() {
        let version = SchemaVersion::parse_version("3.0.0");
        assert!(matches!(version, SchemaVersion::Stable { .. }));
    }

    #[test]
    fn test_parse_version_unknown() {
        let version = SchemaVersion::parse_version("invalid");
        assert_eq!(version, SchemaVersion::Unknown);
    }
}
