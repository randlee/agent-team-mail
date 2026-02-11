//! Content hashing for conflict detection

use blake3;

/// Compute BLAKE3 hash of content for conflict detection
///
/// Uses BLAKE3 (fast, cryptographically secure hash function).
/// Returns a hex-encoded string for easy comparison and logging.
pub fn compute_hash(content: &[u8]) -> String {
    let hash = blake3::hash(content);
    hash.to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_hash_empty() {
        let hash = compute_hash(b"");
        // BLAKE3 of empty string is deterministic
        assert_eq!(
            hash,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn test_compute_hash_content() {
        let content = b"test content";
        let hash = compute_hash(content);
        assert_eq!(hash.len(), 64); // BLAKE3 produces 32-byte (256-bit) hash -> 64 hex chars
    }

    #[test]
    fn test_compute_hash_deterministic() {
        let content = b"deterministic test";
        let hash1 = compute_hash(content);
        let hash2 = compute_hash(content);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_different_content() {
        let hash1 = compute_hash(b"content 1");
        let hash2 = compute_hash(b"content 2");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_json_array() {
        let json = r#"[{"from":"team-lead","text":"message","timestamp":"2026-02-11T14:30:00Z","read":false}]"#;
        let hash = compute_hash(json.as_bytes());
        assert_eq!(hash.len(), 64);
    }
}
