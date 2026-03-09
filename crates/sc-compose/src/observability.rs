use chrono::{SecondsFormat, Utc};
use sc_observability::{LogConfig as SharedLogConfig, LogLevel, Logger as SharedLogger};
use serde_json::Value;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogFormat {
    Jsonl,
    Human,
}

impl LogFormat {
    fn from_env() -> Self {
        match std::env::var("SC_COMPOSE_LOG_FORMAT")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("human") => Self::Human,
            _ => Self::Jsonl,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Logger {
    inner: SharedLogger,
    log_path: PathBuf,
    threshold: LogLevel,
    format: LogFormat,
}

impl Logger {
    pub fn new() -> Self {
        let mut cfg = sc_compose_config();
        let threshold = parse_level_env().unwrap_or(cfg.level);
        cfg.level = threshold;
        let log_path = cfg.log_path.clone();
        let format = LogFormat::from_env();
        Self {
            inner: SharedLogger::new(cfg),
            log_path,
            threshold,
            format,
        }
    }

    pub fn emit(&self, action: &str, result: &str, fields: Value) {
        let level = event_level(action, result);
        if !should_emit(level, self.threshold) {
            return;
        }

        match self.format {
            LogFormat::Jsonl => {
                // Logging is fail-open by contract.
                let _ = self.inner.emit_action(
                    "sc-compose",
                    "sc_compose::cli",
                    action,
                    Some(result),
                    fields,
                );
            }
            LogFormat::Human => {
                let _ = append_human_line(&self.log_path, level, action, result, &fields);
            }
        }
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
    if let Ok(atm_home) = std::env::var("ATM_HOME")
        && !atm_home.trim().is_empty()
    {
        return Some(PathBuf::from(atm_home));
    }
    dirs::home_dir()
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

fn parse_level_env() -> Option<LogLevel> {
    std::env::var("SC_COMPOSE_LOG_LEVEL")
        .ok()
        .and_then(|v| LogLevel::from_str(&v).ok())
}

fn event_level(action: &str, result: &str) -> LogLevel {
    if result.eq_ignore_ascii_case("error") {
        return LogLevel::Error;
    }
    if action == "resolver_decision" {
        return LogLevel::Debug;
    }
    LogLevel::Info
}

fn should_emit(level: LogLevel, threshold: LogLevel) -> bool {
    level_rank(level) >= level_rank(threshold)
}

fn level_rank(level: LogLevel) -> u8 {
    match level {
        LogLevel::Trace => 0,
        LogLevel::Debug => 1,
        LogLevel::Info => 2,
        LogLevel::Warn => 3,
        LogLevel::Error => 4,
    }
}

fn append_human_line(
    log_path: &Path,
    level: LogLevel,
    action: &str,
    result: &str,
    fields: &Value,
) -> std::io::Result<()> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let ts = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let fields_json = serde_json::to_string(fields).unwrap_or_else(|_| "{}".to_string());
    writeln!(
        file,
        "{ts} level={} action={action} outcome={result} fields={fields_json}",
        level.as_str(),
    )
}
