//! Model identifier registry for ATM external agent support.
//!
//! [`ModelId`] is a strongly-typed enum covering well-known AI model identifiers
//! used by Claude Code, Codex, Gemini, and other backends.  Unknown or
//! unlisted models may be expressed using the `Custom(String)` variant
//! (serialised as `"custom:<identifier>"`).
//!
//! # Serialisation
//!
//! All variants serialise/deserialise via their display string, enabling
//! transparent round-trip through JSON team config files.
//!
//! # Examples
//!
//! ```rust
//! use agent_team_mail_core::model_registry::ModelId;
//! use std::str::FromStr;
//!
//! // Parse a known model
//! let m = ModelId::from_str("claude-opus-4-6").unwrap();
//! assert_eq!(m, ModelId::ClaudeOpus46);
//! assert_eq!(m.to_string(), "claude-opus-4-6");
//!
//! // Parse a custom model
//! let c = ModelId::from_str("custom:my-model-v2").unwrap();
//! assert!(matches!(c, ModelId::Custom(_)));
//!
//! // Reject unknown strings
//! assert!(ModelId::from_str("totally-unknown").is_err());
//! ```

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// A validated AI model identifier.
///
/// Known variants cover models in active use at the time of writing.
/// [`ModelId::Custom`] accommodates unlisted or future models without
/// requiring a code change.  Use `"custom:<identifier>"` in any context
/// that accepts a [`ModelId`] string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ModelId {
    /// `claude-opus-4-6`
    ClaudeOpus46,
    /// `claude-sonnet-4-6`
    ClaudeSonnet46,
    /// `claude-haiku-4-5`
    ClaudeHaiku45,
    /// `gpt5.3-codex`
    Gpt53Codex,
    /// `gpt5.3-codex-spark`
    Gpt53CodexSpark,
    /// `o3`
    O3,
    /// `o4-mini`
    O4Mini,
    /// `gemini-2.5-pro`
    Gemini25Pro,
    /// `gemini-2.5-flash`
    Gemini25Flash,
    /// Unknown model — serialised as `"unknown"`.
    ///
    /// This is the default and is used when no model has been specified.
    #[default]
    Unknown,
    /// An unlisted model expressed as `"custom:<identifier>"`.
    ///
    /// The identifier portion must be non-empty.
    Custom(String),
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelId::ClaudeOpus46 => write!(f, "claude-opus-4-6"),
            ModelId::ClaudeSonnet46 => write!(f, "claude-sonnet-4-6"),
            ModelId::ClaudeHaiku45 => write!(f, "claude-haiku-4-5"),
            ModelId::Gpt53Codex => write!(f, "gpt5.3-codex"),
            ModelId::Gpt53CodexSpark => write!(f, "gpt5.3-codex-spark"),
            ModelId::O3 => write!(f, "o3"),
            ModelId::O4Mini => write!(f, "o4-mini"),
            ModelId::Gemini25Pro => write!(f, "gemini-2.5-pro"),
            ModelId::Gemini25Flash => write!(f, "gemini-2.5-flash"),
            ModelId::Unknown => write!(f, "unknown"),
            ModelId::Custom(id) => write!(f, "custom:{id}"),
        }
    }
}

impl FromStr for ModelId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude-opus-4-6" => Ok(ModelId::ClaudeOpus46),
            "claude-sonnet-4-6" => Ok(ModelId::ClaudeSonnet46),
            "claude-haiku-4-5" => Ok(ModelId::ClaudeHaiku45),
            "gpt5.3-codex" => Ok(ModelId::Gpt53Codex),
            "gpt5.3-codex-spark" => Ok(ModelId::Gpt53CodexSpark),
            "o3" => Ok(ModelId::O3),
            "o4-mini" => Ok(ModelId::O4Mini),
            "gemini-2.5-pro" => Ok(ModelId::Gemini25Pro),
            "gemini-2.5-flash" => Ok(ModelId::Gemini25Flash),
            "unknown" => Ok(ModelId::Unknown),
            s if s.starts_with("custom:") => {
                let id = &s["custom:".len()..];
                if id.is_empty() {
                    Err("'custom:' requires a non-empty identifier (e.g., 'custom:my-model')".to_string())
                } else {
                    Ok(ModelId::Custom(id.to_string()))
                }
            }
            other => Err(format!(
                "Unknown model '{other}'. Use 'custom:<identifier>' for unlisted models."
            )),
        }
    }
}

impl Serialize for ModelId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ModelId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        ModelId::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    // ── Display / FromStr round-trips ─────────────────────────────────────

    #[test]
    fn known_variants_display_and_parse() {
        let cases = [
            (ModelId::ClaudeOpus46, "claude-opus-4-6"),
            (ModelId::ClaudeSonnet46, "claude-sonnet-4-6"),
            (ModelId::ClaudeHaiku45, "claude-haiku-4-5"),
            (ModelId::Gpt53Codex, "gpt5.3-codex"),
            (ModelId::Gpt53CodexSpark, "gpt5.3-codex-spark"),
            (ModelId::O3, "o3"),
            (ModelId::O4Mini, "o4-mini"),
            (ModelId::Gemini25Pro, "gemini-2.5-pro"),
            (ModelId::Gemini25Flash, "gemini-2.5-flash"),
            (ModelId::Unknown, "unknown"),
        ];

        for (variant, s) in &cases {
            assert_eq!(variant.to_string(), *s, "Display mismatch for {variant:?}");
            assert_eq!(
                ModelId::from_str(s).unwrap(),
                *variant,
                "FromStr mismatch for '{s}'"
            );
        }
    }

    #[test]
    fn custom_simple_parses() {
        let m = ModelId::from_str("custom:foo").unwrap();
        assert_eq!(m, ModelId::Custom("foo".to_string()));
        assert_eq!(m.to_string(), "custom:foo");
    }

    #[test]
    fn custom_hyphenated_parses() {
        let m = ModelId::from_str("custom:my-model-v2").unwrap();
        assert_eq!(m, ModelId::Custom("my-model-v2".to_string()));
        assert_eq!(m.to_string(), "custom:my-model-v2");
    }

    #[test]
    fn custom_empty_identifier_rejected() {
        let err = ModelId::from_str("custom:").unwrap_err();
        assert!(err.contains("non-empty identifier"), "error was: {err}");
    }

    #[test]
    fn unknown_string_rejected() {
        let err = ModelId::from_str("totally-unknown").unwrap_err();
        assert!(err.contains("Unknown model"), "error was: {err}");
        assert!(err.contains("custom:"), "error was: {err}");
    }

    #[test]
    fn another_unknown_string_rejected() {
        let err = ModelId::from_str("gpt-4o").unwrap_err();
        assert!(err.contains("Unknown model"), "error was: {err}");
    }

    // ── Default ───────────────────────────────────────────────────────────

    #[test]
    fn default_is_unknown() {
        assert_eq!(ModelId::default(), ModelId::Unknown);
    }

    // ── Serde round-trips ────────────────────────────────────────────────

    #[test]
    fn serde_roundtrip_known() {
        let original = ModelId::ClaudeOpus46;
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(json, r#""claude-opus-4-6""#);
        let parsed: ModelId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn serde_roundtrip_custom() {
        let original = ModelId::Custom("my-special-model".to_string());
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(json, r#""custom:my-special-model""#);
        let parsed: ModelId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn serde_roundtrip_unknown() {
        let original = ModelId::Unknown;
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(json, r#""unknown""#);
        let parsed: ModelId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn serde_rejects_invalid_model_in_json() {
        let result: Result<ModelId, _> = serde_json::from_str(r#""not-a-real-model""#);
        assert!(result.is_err());
    }
}
