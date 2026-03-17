//! Canonical daemon launch token issuance for product-owned launcher paths.
//!
//! Phase AU establishes this crate as the only allowed owner for daemon launch
//! token issuance. Other crates may consume the token schema, but they must not
//! issue new launch tokens themselves.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Authorized daemon launch classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaunchClass {
    ProdShared,
    DevShared,
    IsolatedTest,
}

impl LaunchClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProdShared => "prod-shared",
            Self::DevShared => "dev-shared",
            Self::IsolatedTest => "isolated-test",
        }
    }
}

/// Serialized launch token carried from the canonical launcher to `atm-daemon`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonLaunchToken {
    pub launch_class: LaunchClass,
    pub atm_home: PathBuf,
    pub binary_identity: String,
    pub issuer: String,
    pub token_id: String,
    /// RFC3339 UTC timestamp when the launcher issued the token.
    pub issued_at: String,
    /// RFC3339 UTC timestamp after which startup must be rejected.
    pub expires_at: String,
}

/// Issue a launch token from the canonical launcher surface.
pub fn issue_launch_token(
    class: LaunchClass,
    atm_home: &Path,
    binary_identity: impl Into<String>,
    issuer: impl Into<String>,
    ttl: Duration,
) -> DaemonLaunchToken {
    let issued_at = Utc::now();
    let expires_at = issued_at
        + chrono::Duration::from_std(ttl)
            .expect("launch token ttl must fit within chrono::Duration");
    DaemonLaunchToken {
        launch_class: class,
        atm_home: atm_home.to_path_buf(),
        binary_identity: binary_identity.into(),
        issuer: issuer.into(),
        token_id: Uuid::new_v4().to_string(),
        issued_at: issued_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        expires_at: expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_launch_token_populates_required_fields() {
        let atm_home = std::env::temp_dir().join("atm-home");
        let token = issue_launch_token(
            LaunchClass::DevShared,
            &atm_home,
            "dev-channel",
            "au.1-test",
            Duration::from_secs(90),
        );

        assert_eq!(token.launch_class, LaunchClass::DevShared);
        assert_eq!(token.atm_home, atm_home);
        assert_eq!(token.binary_identity, "dev-channel");
        assert_eq!(token.issuer, "au.1-test");
        assert!(!token.token_id.is_empty());
        assert!(chrono::DateTime::parse_from_rfc3339(&token.issued_at).is_ok());
        assert!(chrono::DateTime::parse_from_rfc3339(&token.expires_at).is_ok());
    }

    #[test]
    fn launch_class_serializes_as_kebab_case() {
        let encoded = serde_json::to_string(&LaunchClass::IsolatedTest).unwrap();
        assert_eq!(encoded, "\"isolated-test\"");
    }
}
