//! Local state management for ATM CLI (seen tracking).

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SeenState {
    /// Map of team -> agent -> last_seen ISO timestamp
    #[serde(default)]
    pub last_seen: HashMap<String, HashMap<String, String>>,
}

pub fn load_seen_state() -> Result<SeenState> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(SeenState::default());
    }
    let content = std::fs::read_to_string(path)?;
    let state: SeenState = serde_json::from_str(&content)?;
    Ok(state)
}

/// Save seen state to disk.
///
/// Note: concurrent writes from multiple `atm read` processes may race.
/// This is benign — the worst case is a slightly stale last-seen timestamp,
/// causing a few extra messages to appear on the next read. No data is lost.
pub fn save_seen_state(state: &SeenState) -> Result<()> {
    let path = state_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let serialized = serde_json::to_string_pretty(state)?;
    std::fs::write(path, serialized)?;
    Ok(())
}

pub fn get_last_seen(state: &SeenState, team: &str, agent: &str) -> Option<DateTime<Utc>> {
    state
        .last_seen
        .get(team)
        .and_then(|agents| agents.get(agent))
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

pub fn update_last_seen(state: &mut SeenState, team: &str, agent: &str, timestamp: &str) {
    state
        .last_seen
        .entry(team.to_string())
        .or_default()
        .insert(agent.to_string(), timestamp.to_string());
}

pub fn state_path() -> Result<PathBuf> {
    // When ATM_HOME is set, use it directly for state.json (test-friendly)
    // When not set, use platform config directory
    if let Ok(atm_home) = std::env::var("ATM_HOME") {
        return Ok(PathBuf::from(atm_home).join("state.json"));
    }

    // Use platform config directory for production
    let home = agent_team_mail_core::home::get_home_dir()?;
    Ok(home.join(".config/atm/state.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_and_get_last_seen() {
        let mut state = SeenState::default();
        let ts = "2026-02-15T03:00:00Z";
        update_last_seen(&mut state, "team", "agent", ts);

        let seen = get_last_seen(&state, "team", "agent");
        assert!(seen.is_some());
        assert_eq!(seen.unwrap().to_rfc3339(), "2026-02-15T03:00:00+00:00");
    }
}
