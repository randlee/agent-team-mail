use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use agent_team_mail_core::event_log::{EventFields, emit_event_best_effort};
use agent_team_mail_daemon_launch::{
    ATM_LAUNCH_TOKEN_ENV, DaemonLaunchToken, LaunchClass, decode_launch_token,
};
use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub enum StartupRejectionReason {
    MissingToken,
    InvalidToken,
    ExpiredToken,
    WrongAtmHome,
    WrongLaunchClass,
    ReplayedToken,
    SharedRuntimeAlreadyRunning,
}

impl StartupRejectionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingToken => "missing_token",
            Self::InvalidToken => "invalid_token",
            Self::ExpiredToken => "expired_token",
            Self::WrongAtmHome => "wrong_atm_home",
            Self::WrongLaunchClass => "wrong_launch_class",
            Self::ReplayedToken => "replayed_token",
            Self::SharedRuntimeAlreadyRunning => "shared_runtime_already_running",
        }
    }
}

#[derive(Debug, Error)]
pub enum StartupAuthError {
    #[error("missing launch token")]
    MissingToken,
    #[error("invalid launch token ({0})")]
    InvalidToken(String),
    #[error("launch token expired")]
    ExpiredToken,
    #[error("token ATM_HOME does not match runtime")]
    WrongAtmHome,
    #[error("wrong launch class for runtime")]
    WrongLaunchClass,
    #[error("launch token replayed")]
    ReplayedToken,
}

fn seen_tokens() -> &'static Mutex<HashSet<String>> {
    static TOKENS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    TOKENS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn canonicalize_lossy(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn expected_launch_class(home: &Path) -> Result<LaunchClass> {
    Ok(
        match agent_team_mail_core::daemon_client::runtime_kind_for_home(home)? {
            agent_team_mail_core::daemon_client::RuntimeKind::Release => LaunchClass::ProdShared,
            agent_team_mail_core::daemon_client::RuntimeKind::Dev => LaunchClass::DevShared,
            agent_team_mail_core::daemon_client::RuntimeKind::Isolated => LaunchClass::IsolatedTest,
        },
    )
}

fn emit_startup_rejection(
    reason: StartupRejectionReason,
    token: Option<&DaemonLaunchToken>,
    atm_home: &Path,
    detail: Option<&str>,
) {
    let launch_class = token.map(|token| token.launch_class.as_str().to_string());
    let token_id = token.map(|token| token.token_id.clone());
    let atm_home_text = canonicalize_lossy(atm_home).display().to_string();
    let payload = json!({
        "source": "atm-daemon",
        "action": "daemon_start_rejected",
        "rejection_reason": reason.as_str(),
        "launch_class": launch_class,
        "token_id": token_id,
        "atm_home": atm_home_text,
        "timestamp": Utc::now().to_rfc3339(),
        "detail": detail,
    });
    eprintln!("{payload}");

    let mut extra = serde_json::Map::new();
    extra.insert(
        "rejection_reason".to_string(),
        serde_json::Value::String(reason.as_str().to_string()),
    );
    if let Some(class) = token.map(|token| token.launch_class.as_str()) {
        extra.insert(
            "launch_class".to_string(),
            serde_json::Value::String(class.to_string()),
        );
    }
    if let Some(token_id) = token_id {
        extra.insert("token_id".to_string(), serde_json::Value::String(token_id));
    }
    extra.insert(
        "atm_home".to_string(),
        serde_json::Value::String(atm_home_text.clone()),
    );

    emit_event_best_effort(EventFields {
        level: "error",
        source: "atm-daemon",
        action: "daemon_start_rejected",
        result: Some(reason.as_str().to_string()),
        target: Some(atm_home_text),
        error: detail.map(str::to_string),
        extra_fields: extra,
        ..Default::default()
    });
}

fn validate_token_inner(
    home: &Path,
    raw: Option<&str>,
) -> std::result::Result<DaemonLaunchToken, StartupAuthError> {
    let raw = raw.ok_or(StartupAuthError::MissingToken)?;
    let token =
        decode_launch_token(raw).map_err(|err| StartupAuthError::InvalidToken(err.to_string()))?;

    if token.binary_identity.trim().is_empty()
        || token.issuer.trim().is_empty()
        || token.token_id.trim().is_empty()
    {
        return Err(StartupAuthError::InvalidToken(
            "required token fields must be non-empty".to_string(),
        ));
    }

    let issued_at = chrono::DateTime::parse_from_rfc3339(&token.issued_at)
        .map_err(|err| StartupAuthError::InvalidToken(err.to_string()))?;
    let expires_at = chrono::DateTime::parse_from_rfc3339(&token.expires_at)
        .map_err(|err| StartupAuthError::InvalidToken(err.to_string()))?;
    if expires_at < issued_at {
        return Err(StartupAuthError::InvalidToken(
            "expires_at precedes issued_at".to_string(),
        ));
    }
    if expires_at.with_timezone(&Utc) <= Utc::now() {
        return Err(StartupAuthError::ExpiredToken);
    }

    if canonicalize_lossy(home) != canonicalize_lossy(&token.atm_home) {
        return Err(StartupAuthError::WrongAtmHome);
    }

    if token.launch_class
        != expected_launch_class(home)
            .map_err(|err| StartupAuthError::InvalidToken(err.to_string()))?
    {
        return Err(StartupAuthError::WrongLaunchClass);
    }

    let mut seen = seen_tokens().lock().unwrap();
    if !seen.insert(token.token_id.clone()) {
        return Err(StartupAuthError::ReplayedToken);
    }

    Ok(token)
}

pub fn validate_startup_token(home: &Path) -> Result<DaemonLaunchToken> {
    let raw = std::env::var(ATM_LAUNCH_TOKEN_ENV).ok();
    match validate_token_inner(home, raw.as_deref()) {
        Ok(token) => Ok(token),
        Err(err) => {
            let reason = match &err {
                StartupAuthError::MissingToken => StartupRejectionReason::MissingToken,
                StartupAuthError::InvalidToken(_) => StartupRejectionReason::InvalidToken,
                StartupAuthError::ExpiredToken => StartupRejectionReason::ExpiredToken,
                StartupAuthError::WrongAtmHome => StartupRejectionReason::WrongAtmHome,
                StartupAuthError::WrongLaunchClass => StartupRejectionReason::WrongLaunchClass,
                StartupAuthError::ReplayedToken => StartupRejectionReason::ReplayedToken,
            };
            let parsed = raw.as_deref().and_then(|raw| decode_launch_token(raw).ok());
            emit_startup_rejection(reason, parsed.as_ref(), home, Some(&err.to_string()));
            Err(err.into())
        }
    }
}

pub fn log_shared_runtime_rejection(home: &Path, token: &DaemonLaunchToken, detail: &str) {
    emit_startup_rejection(
        StartupRejectionReason::SharedRuntimeAlreadyRunning,
        Some(token),
        home,
        Some(detail),
    );
}

#[cfg(test)]
pub(crate) fn clear_seen_tokens_for_tests() {
    seen_tokens().lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_daemon_launch::{encode_launch_token, issue_launch_token};
    use std::time::Duration;
    use tempfile::TempDir;

    fn token_for(home: &Path, class: LaunchClass, ttl_secs: i64) -> DaemonLaunchToken {
        let now = Utc::now();
        DaemonLaunchToken {
            launch_class: class,
            atm_home: home.to_path_buf(),
            binary_identity: "test-binary".to_string(),
            issuer: "startup-auth-test".to_string(),
            token_id: uuid::Uuid::new_v4().to_string(),
            issued_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::seconds(ttl_secs)).to_rfc3339(),
        }
    }

    #[test]
    fn missing_token_is_rejected() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let err = validate_token_inner(temp.path(), None).unwrap_err();
        assert!(matches!(err, StartupAuthError::MissingToken));
    }

    #[test]
    fn invalid_token_is_rejected() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let err = validate_token_inner(temp.path(), Some("{not-json")).unwrap_err();
        assert!(matches!(err, StartupAuthError::InvalidToken(_)));
    }

    #[test]
    fn expired_token_is_rejected() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let token = token_for(temp.path(), LaunchClass::IsolatedTest, -1);
        let err = validate_token_inner(temp.path(), Some(&encode_launch_token(&token).unwrap()))
            .unwrap_err();
        assert!(matches!(err, StartupAuthError::ExpiredToken));
    }

    #[test]
    fn wrong_home_is_rejected() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let token = token_for(other.path(), LaunchClass::IsolatedTest, 30);
        let err = validate_token_inner(temp.path(), Some(&encode_launch_token(&token).unwrap()))
            .unwrap_err();
        assert!(matches!(err, StartupAuthError::WrongAtmHome));
    }

    #[test]
    fn wrong_class_is_rejected() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let token = token_for(temp.path(), LaunchClass::ProdShared, 30);
        let err = validate_token_inner(temp.path(), Some(&encode_launch_token(&token).unwrap()))
            .unwrap_err();
        assert!(matches!(err, StartupAuthError::WrongLaunchClass));
    }

    #[test]
    fn replayed_token_is_rejected() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let token = token_for(temp.path(), LaunchClass::IsolatedTest, 30);
        let raw = encode_launch_token(&token).unwrap();
        assert!(validate_token_inner(temp.path(), Some(&raw)).is_ok());
        let err = validate_token_inner(temp.path(), Some(&raw)).unwrap_err();
        assert!(matches!(err, StartupAuthError::ReplayedToken));
    }

    #[test]
    fn valid_token_is_accepted() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let token = issue_launch_token(
            LaunchClass::IsolatedTest,
            temp.path(),
            "test-binary",
            "startup-auth-test",
            Duration::from_secs(30),
        );
        let raw = encode_launch_token(&token).unwrap();
        let accepted = validate_token_inner(temp.path(), Some(&raw)).unwrap();
        assert_eq!(accepted.launch_class, LaunchClass::IsolatedTest);
    }
}
