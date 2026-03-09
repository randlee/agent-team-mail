use chrono::Utc;
use serde_json::json;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Logger {
    path: Option<PathBuf>,
}

impl Logger {
    pub fn new() -> Self {
        Self {
            path: default_log_path(),
        }
    }

    pub fn emit(&self, action: &str, result: &str, fields: serde_json::Value) {
        let Some(path) = &self.path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let event = json!({
            "timestamp": Utc::now().to_rfc3339(),
            "source": "sc-compose",
            "action": action,
            "result": result,
            "fields": truncate_fields(fields),
        });
        let line = event.to_string();

        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            let _ = writeln!(file, "{line}");
        }
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
        if let Ok(app_data) = std::env::var("APPDATA") {
            return Some(PathBuf::from(app_data).join("sc-compose/logs/sc-compose.log"));
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.trim().is_empty()
    {
        return Some(PathBuf::from(xdg).join("sc-compose/logs/sc-compose.log"));
    }
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".config/sc-compose/logs/sc-compose.log"))
}

fn truncate_fields(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => {
            let max = 512;
            if s.len() > max {
                serde_json::Value::String(format!("{}…", &s[..max]))
            } else {
                serde_json::Value::String(s)
            }
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(truncate_fields).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, truncate_fields(v)))
                .collect(),
        ),
        other => other,
    }
}
