//! Unicode-safe text utilities for message handling.

/// Truncate `text` to at most `max_chars` Unicode scalar values,
/// appending `suffix` if truncation occurred.
///
/// Uses `char_indices().nth(max_chars)` — compatible with MSRV 1.85.
/// Do NOT use `str::floor_char_boundary` (requires Rust 1.91).
pub fn truncate_chars(text: &str, max_chars: usize, suffix: &str) -> String {
    match text.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => {
            let mut result = text[..byte_idx].to_string();
            result.push_str(suffix);
            result
        }
        None => text.to_string(),
    }
}

/// Return a slice of `text` containing at most `max_chars` Unicode scalar values.
///
/// Uses `char_indices().nth(max_chars)` — compatible with MSRV 1.85.
pub fn truncate_chars_slice(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &text[..byte_idx],
        None => text,
    }
}

/// Validate message text before delivery.
///
/// Returns `Err` with a user-facing error message when:
/// - `text` contains null bytes (`\0`) — these corrupt JSON files
/// - `text.len()` (in bytes) exceeds `max_bytes`
///
/// # Errors
///
/// Returns `Err(String)` with a human-readable message on validation failure.
pub fn validate_message_text(text: &str, max_bytes: usize) -> Result<(), String> {
    if text.contains('\0') {
        return Err(
            "Message contains null bytes (\\0) which are not allowed. \
             Remove null bytes and retry."
                .to_string(),
        );
    }
    if text.len() > max_bytes {
        return Err(format!(
            "Message size ({} bytes) exceeds maximum allowed ({} bytes). \
             Consider using --file <path> for large payloads.",
            text.len(),
            max_bytes,
        ));
    }
    Ok(())
}

/// Default maximum message size in bytes (1 MiB).
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 1_048_576;

#[cfg(test)]
mod tests {
    use super::*;

    // truncate_chars tests

    #[test]
    fn truncate_chars_ascii_exact() {
        assert_eq!(truncate_chars("hello", 5, "..."), "hello");
    }

    #[test]
    fn truncate_chars_ascii_truncates() {
        assert_eq!(truncate_chars("hello world", 5, "..."), "hello...");
    }

    #[test]
    fn truncate_chars_empty() {
        assert_eq!(truncate_chars("", 10, "..."), "");
    }

    #[test]
    fn truncate_chars_zero_max() {
        assert_eq!(truncate_chars("hello", 0, "..."), "...");
    }

    #[test]
    fn truncate_chars_em_dash_boundary() {
        // em dash is 3 bytes; truncating at 1 char should give "—" not panic
        let text = "—hello";
        assert_eq!(truncate_chars(text, 1, ""), "—");
    }

    #[test]
    fn truncate_chars_cjk_boundary() {
        let text = "中文测试";
        assert_eq!(truncate_chars(text, 2, ""), "中文");
    }

    #[test]
    fn truncate_chars_emoji_boundary() {
        let text = "🦀🦀🦀";
        assert_eq!(truncate_chars(text, 2, "..."), "🦀🦀...");
    }

    #[test]
    fn truncate_chars_grapheme_combining() {
        // e + combining acute accent = 2 Unicode scalar values
        let text = "e\u{0301}hello";
        // Truncating at 2 chars gives "e\u{0301}" (e + combining accent)
        let result = truncate_chars(text, 2, "");
        assert_eq!(result.chars().count(), 2);
    }

    #[test]
    fn truncate_chars_all_emoji() {
        let text = "🦀🐍🎯🦄";
        assert_eq!(truncate_chars(text, 3, "!"), "🦀🐍🎯!");
    }

    // truncate_chars_slice tests

    #[test]
    fn truncate_chars_slice_ascii() {
        assert_eq!(truncate_chars_slice("hello world", 5), "hello");
    }

    #[test]
    fn truncate_chars_slice_exact() {
        assert_eq!(truncate_chars_slice("hello", 5), "hello");
    }

    #[test]
    fn truncate_chars_slice_multibyte() {
        let text = "中文ABC";
        assert_eq!(truncate_chars_slice(text, 2), "中文");
    }

    #[test]
    fn truncate_chars_slice_empty() {
        assert_eq!(truncate_chars_slice("", 10), "");
    }

    // validate_message_text tests

    #[test]
    fn validate_ok() {
        assert!(validate_message_text("hello world", DEFAULT_MAX_MESSAGE_BYTES).is_ok());
    }

    #[test]
    fn validate_null_byte_rejected() {
        let text = "hello\0world";
        let err = validate_message_text(text, DEFAULT_MAX_MESSAGE_BYTES).unwrap_err();
        assert!(err.contains("null byte"), "error was: {err}");
    }

    #[test]
    fn validate_oversize_rejected() {
        let text = "a".repeat(DEFAULT_MAX_MESSAGE_BYTES + 1);
        let err = validate_message_text(&text, DEFAULT_MAX_MESSAGE_BYTES).unwrap_err();
        assert!(err.contains("exceeds maximum"), "error was: {err}");
        assert!(err.contains("--file"), "error was: {err}");
    }

    #[test]
    fn validate_exactly_max_bytes_ok() {
        let text = "a".repeat(DEFAULT_MAX_MESSAGE_BYTES);
        assert!(validate_message_text(&text, DEFAULT_MAX_MESSAGE_BYTES).is_ok());
    }

    #[test]
    fn validate_empty_ok() {
        assert!(validate_message_text("", DEFAULT_MAX_MESSAGE_BYTES).is_ok());
    }
}
