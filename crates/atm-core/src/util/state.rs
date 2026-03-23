use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Shared persisted watermark state for ATM consumers.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SeenState {
    /// Map of team -> agent -> last_seen ISO timestamp.
    #[serde(default)]
    pub last_seen: HashMap<String, HashMap<String, String>>,
}
