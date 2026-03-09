use agent_team_mail_core::logging_event::LogEventV1;
use sc_observability::{LogConfig as SharedLogConfig, Logger as SharedLogger};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Logger {
    inner: SharedLogger,
}

impl Logger {
    pub fn new() -> Self {
        let cfg = sc_compose_config();
        Self {
            inner: SharedLogger::new(cfg),
        }
    }

    pub fn emit(&self, action: &str, result: &str, fields: Value) {
        let mut event = LogEventV1::builder("sc-compose", action, "sc_compose::cli")
            .level("info")
            .build();
        event.outcome = Some(result.to_string());
        event.fields = value_to_map(fields);
        // Logging is fail-open by contract.
        let _ = self.inner.emit(&event);
    }
}

fn sc_compose_config() -> SharedLogConfig {
    let home_dir = resolve_home_dir().unwrap_or_else(|| PathBuf::from("."));
    let mut cfg = SharedLogConfig::from_home(&home_dir);
    cfg.log_path = default_log_path().unwrap_or_else(|| {
        home_dir
            .join(".config")
            .join("sc-compose")
            .join("logs")
            .join("sc-compose.log")
    });
    cfg.spool_dir = default_spool_dir(&cfg.log_path);
    cfg
}

fn resolve_home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE")
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HOME").ok().map(PathBuf::from))
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

fn default_log_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("SC_COMPOSE_LOG_FILE")
        && !explicit.trim().is_empty()
    {
        return Some(PathBuf::from(explicit));
    }
    #[cfg(windows)]
    {
        if let Ok(app_data) = std::env::var("APPDATA")
            && !app_data.trim().is_empty()
        {
            return Some(PathBuf::from(app_data).join("sc-compose/logs/sc-compose.log"));
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.trim().is_empty()
    {
        return Some(PathBuf::from(xdg).join("sc-compose/logs/sc-compose.log"));
    }
    resolve_home_dir().map(|home| home.join(".config/sc-compose/logs/sc-compose.log"))
}

fn default_spool_dir(log_path: &Path) -> PathBuf {
    let parent = log_path.parent().unwrap_or_else(|| Path::new("."));
    if parent
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("logs"))
        .unwrap_or(false)
    {
        parent
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("log-spool")
    } else {
        parent.join("log-spool")
    }
}

fn value_to_map(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("value".to_string(), other);
            map
        }
    }
}
