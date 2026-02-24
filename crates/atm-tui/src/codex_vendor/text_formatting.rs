//! Vendored from: /Users/randlee/Documents/github/codex/codex-rs/tui/src/text_formatting.rs
//! (adapted for ATM TUI integration)

/// Format JSON text in a compact single-line format with spaces suitable for
/// terminal wrapping.
///
/// Returns the formatted JSON string if the input is valid JSON, otherwise
/// returns `None`.
pub(crate) fn format_json_compact(text: &str) -> Option<String> {
    let json = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let json_pretty = serde_json::to_string_pretty(&json).unwrap_or_else(|_| json.to_string());

    let mut result = String::new();
    let mut chars = json_pretty.chars().peekable();
    let mut in_string = false;
    let mut escape_next = false;

    while let Some(ch) = chars.next() {
        match ch {
            '"' if !escape_next => {
                in_string = !in_string;
                result.push(ch);
            }
            '\\' if in_string => {
                escape_next = !escape_next;
                result.push(ch);
            }
            '\n' | '\r' if !in_string => {}
            ' ' | '\t' if !in_string => {
                if let Some(&next_ch) = chars.peek()
                    && let Some(last_ch) = result.chars().last()
                    && (last_ch == ':' || last_ch == ',')
                    && !matches!(next_ch, '}' | ']')
                {
                    result.push(' ');
                }
            }
            _ => {
                if escape_next && in_string {
                    escape_next = false;
                }
                result.push(ch);
            }
        }
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::format_json_compact;

    #[test]
    fn formats_json_compact() {
        let input = "{\"a\":1,\"b\":[2,3]}";
        let formatted = format_json_compact(input).expect("valid json");
        assert_eq!(formatted, "{\"a\": 1, \"b\": [2, 3]}");
    }
}
