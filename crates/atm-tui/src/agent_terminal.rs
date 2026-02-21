//! Agent Terminal panel: session log tail and JSONL key expansion.
//!
//! This module owns the logic for expanding compact JSONL keys to their full
//! column names before display in the Agent Terminal panel.

/// Mapping from compact on-disk event keys to full display column names.
///
/// Ordered to match §6 of the TUI MVP architecture document.
pub const COLUMN_MAP: &[(&str, &str)] = &[
    ("ts", "Timestamp"),
    ("lv", "Level"),
    ("src", "Source"),
    ("act", "Action"),
    ("team", "Team"),
    ("sid", "Session ID"),
    ("aid", "Agent ID"),
    ("anm", "Agent Name"),
    ("target", "Target"),
    ("res", "Result"),
    ("mid", "Message ID"),
    ("rid", "Request ID"),
    ("cnt", "Count"),
    ("err", "Error"),
    ("msg", "Message"),
];

/// Expand compact JSONL keys in a log line to their full column names.
///
/// If the line parses as a JSON object, all known compact keys are replaced by
/// their full names and the object is re-serialised. Unknown keys are left
/// unchanged. If the line is not valid JSON it is returned as-is.
///
/// # Examples
///
/// ```
/// use atm_tui::agent_terminal::expand_keys;
///
/// let line = r#"{"ts":"2026-01-01","lv":"info","act":"send"}"#;
/// let expanded = expand_keys(line);
/// assert!(expanded.contains("Timestamp"));
/// assert!(expanded.contains("Level"));
/// assert!(expanded.contains("Action"));
/// ```
pub fn expand_keys(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return line.to_string();
    }

    let value: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return line.to_string(),
    };

    let obj = match value.as_object() {
        Some(o) => o,
        None => return line.to_string(),
    };

    let mut expanded = serde_json::Map::with_capacity(obj.len());
    for (k, v) in obj {
        let display_key = COLUMN_MAP
            .iter()
            .find(|(compact, _)| *compact == k.as_str())
            .map(|(_, full)| *full)
            .unwrap_or(k.as_str());
        expanded.insert(display_key.to_string(), v.clone());
    }

    serde_json::to_string(&serde_json::Value::Object(expanded))
        .unwrap_or_else(|_| line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_column_expansion() {
        let input = r#"{"ts":"2026-01-01T00:00:00Z","lv":"info","src":"atm-tui","act":"tui_start","team":"atm-dev"}"#;
        let result = expand_keys(input);

        // All known compact keys must be replaced.
        assert!(result.contains("\"Timestamp\""), "expected Timestamp in: {result}");
        assert!(result.contains("\"Level\""), "expected Level in: {result}");
        assert!(result.contains("\"Source\""), "expected Source in: {result}");
        assert!(result.contains("\"Action\""), "expected Action in: {result}");
        assert!(result.contains("\"Team\""), "expected Team in: {result}");

        // Original compact keys must not appear as keys.
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let obj = parsed.as_object().unwrap();
        assert!(!obj.contains_key("ts"), "compact key 'ts' should be gone");
        assert!(!obj.contains_key("lv"), "compact key 'lv' should be gone");
        assert!(!obj.contains_key("src"), "compact key 'src' should be gone");
        assert!(!obj.contains_key("act"), "compact key 'act' should be gone");
        assert!(!obj.contains_key("team"), "compact key 'team' should be gone");
    }

    #[test]
    fn test_non_json_line_returned_as_is() {
        let input = "this is not json at all";
        assert_eq!(expand_keys(input), input);
    }

    #[test]
    fn test_unknown_keys_preserved() {
        let input = r#"{"unknown_key":"value","ts":"2026-01-01"}"#;
        let result = expand_keys(input);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let obj = parsed.as_object().unwrap();
        assert!(obj.contains_key("unknown_key"));
        assert!(obj.contains_key("Timestamp"));
    }

    #[test]
    fn test_all_column_map_entries_expanded() {
        // Build a JSON object with every compact key.
        let mut obj = serde_json::Map::new();
        for (compact, _) in COLUMN_MAP {
            obj.insert(compact.to_string(), serde_json::Value::String("v".into()));
        }
        let input = serde_json::to_string(&serde_json::Value::Object(obj)).unwrap();
        let result = expand_keys(&input);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let out = parsed.as_object().unwrap();

        for (compact, full) in COLUMN_MAP {
            assert!(
                out.contains_key(*full),
                "full key '{full}' missing from output"
            );
            assert!(
                !out.contains_key(*compact),
                "compact key '{compact}' should not remain"
            );
        }
    }
}
