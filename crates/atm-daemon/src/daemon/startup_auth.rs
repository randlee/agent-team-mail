use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use agent_team_mail_core::event_log::{
    EventFields, emit_event_best_effort, emit_event_to_spool_direct,
};
use agent_team_mail_daemon_launch::{ATM_LAUNCH_TOKEN_ENV, DaemonLaunchToken, decode_launch_token};
use anyhow::Result;
use chrono::Utc;
use thiserror::Error;
#[derive(Debug, Clone, Copy)]
pub enum StartupRejectionReason {
    MissingToken,
    InvalidToken,
    ExpiredToken,
    WrongAtmHome,
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

fn lifecycle_phase(event_name: &'static str) -> &'static str {
    match event_name {
        "launch_accepted" => "startup",
        "clean_owner_shutdown" | "ttl_expiry_shutdown" | "dead_owner_shutdown" => "shutdown",
        "janitor_reap" => "cleanup",
        _ => "lifecycle",
    }
}

fn termination_reason(event_name: &'static str) -> Option<&'static str> {
    match event_name {
        "clean_owner_shutdown" | "ttl_expiry_shutdown" | "dead_owner_shutdown" => Some(event_name),
        _ => None,
    }
}

fn emit_lifecycle_event(
    level: &'static str,
    event_name: &'static str,
    token: Option<&DaemonLaunchToken>,
    atm_home: &Path,
    detail: Option<&str>,
) {
    #[cfg(test)]
    note_test_lifecycle_event(event_name);

    let token_id = token.map(|value| value.token_id.clone());
    let test_identifier = token.and_then(|value| value.test_identifier.clone());
    let owner_pid = token.and_then(|value| value.owner_pid);
    let atm_home_text = canonicalize_lossy(atm_home).display().to_string();
    let request_id = token_id
        .as_ref()
        .map(|token_id| format!("daemon-launch-{token_id}"));
    let trace_id = request_id.as_deref().map(|request_id| {
        agent_team_mail_core::event_log::trace_id_for_request("atm-daemon", request_id)
    });
    let span_id = trace_id
        .as_deref()
        .map(|trace_id| agent_team_mail_core::event_log::span_id_for_action(trace_id, event_name));
    let lifecycle_phase = lifecycle_phase(event_name);
    let termination_reason = termination_reason(event_name);
    let mut extra = serde_json::Map::new();
    extra.insert(
        "event_name".to_string(),
        serde_json::Value::String(event_name.to_string()),
    );
    extra.insert(
        "lifecycle_phase".to_string(),
        serde_json::Value::String(lifecycle_phase.to_string()),
    );
    if let Some(class) = token.map(|value| value.launch_class.as_str()) {
        extra.insert(
            "launch_class".to_string(),
            serde_json::Value::String(class.to_string()),
        );
    }
    if let Some(token_id) = token_id {
        extra.insert("token_id".to_string(), serde_json::Value::String(token_id));
    }
    if let Some(test_identifier) = test_identifier {
        extra.insert(
            "test_identifier".to_string(),
            serde_json::Value::String(test_identifier),
        );
    }
    if let Some(owner_pid) = owner_pid {
        extra.insert(
            "owner_pid".to_string(),
            serde_json::Value::Number(owner_pid.into()),
        );
    }
    extra.insert(
        "atm_home".to_string(),
        serde_json::Value::String(atm_home_text.clone()),
    );
    if let Some(reason) = termination_reason {
        extra.insert(
            "termination_reason".to_string(),
            serde_json::Value::String(reason.to_string()),
        );
    }

    emit_event_best_effort(EventFields {
        level,
        source: "atm-daemon",
        action: event_name,
        result: Some(event_name.to_string()),
        request_id,
        trace_id,
        span_id,
        target: Some(atm_home_text),
        error: detail.map(str::to_string),
        extra_fields: extra,
        ..Default::default()
    });

    export_lifecycle_trace(level, event_name, token, atm_home, detail);
}

fn export_lifecycle_trace(
    level: &'static str,
    event_name: &'static str,
    token: Option<&DaemonLaunchToken>,
    atm_home: &Path,
    detail: Option<&str>,
) {
    let request_id = token
        .map(|value| format!("daemon-launch-{}", value.token_id))
        .unwrap_or_else(|| {
            let unique_suffix = Utc::now()
                .timestamp_nanos_opt()
                .map(|value| value.to_string())
                .unwrap_or_else(|| Utc::now().timestamp_micros().to_string());
            format!("daemon-lifecycle-{event_name}-{}", unique_suffix)
        });
    let trace_id = agent_team_mail_core::event_log::trace_id_for_request("atm-daemon", &request_id);
    let span_id = agent_team_mail_core::event_log::span_id_for_action(&trace_id, event_name);
    let mut record = crate::daemon::observability::LifecycleTraceRecord::new(
        format!("atm-daemon.lifecycle.{event_name}"),
        if level == "warn" {
            crate::daemon::observability::LifecycleTraceStatus::Error
        } else {
            crate::daemon::observability::LifecycleTraceStatus::Ok
        },
        trace_id,
        span_id,
    );
    record
        .attributes
        .insert("event_name".to_string(), event_name.to_string());
    record.attributes.insert(
        "lifecycle_phase".to_string(),
        lifecycle_phase(event_name).to_string(),
    );
    record.attributes.insert(
        "atm_home".to_string(),
        canonicalize_lossy(atm_home).display().to_string(),
    );
    if let Some(reason) = termination_reason(event_name) {
        record
            .attributes
            .insert("termination_reason".to_string(), reason.to_string());
    }
    if let Some(token) = token {
        record.attributes.insert(
            "launch_class".to_string(),
            token.launch_class.as_str().to_string(),
        );
        record
            .attributes
            .insert("token_id".to_string(), token.token_id.clone());
        if let Some(test_identifier) = &token.test_identifier {
            record
                .attributes
                .insert("test_identifier".to_string(), test_identifier.clone());
        }
        if let Some(owner_pid) = token.owner_pid {
            record
                .attributes
                .insert("owner_pid".to_string(), owner_pid.to_string());
        }
    }
    if let Some(detail) = detail {
        record
            .attributes
            .insert("detail".to_string(), detail.to_string());
    }
    crate::daemon::observability::export_lifecycle_trace(record);
}

pub fn log_launch_accepted(home: &Path, token: &DaemonLaunchToken) {
    emit_lifecycle_event(
        "info",
        "launch_accepted",
        Some(token),
        home,
        Some("daemon launch token accepted and runtime lease persisted"),
    );
}

pub fn log_clean_owner_shutdown(home: &Path, token: &DaemonLaunchToken) {
    // Write directly to spool, bypassing the background log-forwarder thread.
    // The forwarder is a fire-and-forget thread that the OS reclaims on process
    // exit; events queued just before exit are silently lost on macOS.
    // log_clean_owner_shutdown() is called after daemon::run() returns, making
    // it the most vulnerable. Writing synchronously guarantees the event is
    // durably persisted before main() returns.
    let event_name = "clean_owner_shutdown";
    let token_id = Some(token.token_id.clone());
    let atm_home_text = canonicalize_lossy(home).display().to_string();
    let request_id = token_id.as_ref().map(|id| format!("daemon-launch-{id}"));
    let trace_id = request_id
        .as_deref()
        .map(|rid| agent_team_mail_core::event_log::trace_id_for_request("atm-daemon", rid));
    let span_id = trace_id
        .as_deref()
        .map(|tid| agent_team_mail_core::event_log::span_id_for_action(tid, event_name));
    let mut extra = serde_json::Map::new();
    extra.insert(
        "event_name".to_string(),
        serde_json::Value::String(event_name.to_string()),
    );
    extra.insert(
        "lifecycle_phase".to_string(),
        serde_json::Value::String("shutdown".to_string()),
    );
    extra.insert(
        "launch_class".to_string(),
        serde_json::Value::String(token.launch_class.as_str().to_string()),
    );
    extra.insert(
        "token_id".to_string(),
        serde_json::Value::String(token.token_id.clone()),
    );
    if let Some(test_identifier) = &token.test_identifier {
        extra.insert(
            "test_identifier".to_string(),
            serde_json::Value::String(test_identifier.clone()),
        );
    }
    if let Some(owner_pid) = token.owner_pid {
        extra.insert(
            "owner_pid".to_string(),
            serde_json::Value::Number(owner_pid.into()),
        );
    }
    extra.insert(
        "atm_home".to_string(),
        serde_json::Value::String(atm_home_text.clone()),
    );
    extra.insert(
        "termination_reason".to_string(),
        serde_json::Value::String(event_name.to_string()),
    );
    let fields = EventFields {
        level: "info",
        source: "atm-daemon",
        action: event_name,
        result: Some(event_name.to_string()),
        request_id,
        trace_id,
        span_id,
        target: Some(atm_home_text),
        error: Some("daemon exited after clean owner-controlled shutdown".to_string()),
        extra_fields: extra,
        ..Default::default()
    };
    emit_event_to_spool_direct(&fields, home);
    export_lifecycle_trace(
        "info",
        event_name,
        Some(token),
        home,
        fields.error.as_deref(),
    );
}

fn emit_startup_rejection(
    reason: StartupRejectionReason,
    token: Option<&DaemonLaunchToken>,
    atm_home: &Path,
    detail: Option<&str>,
) {
    let token_id = token.map(|token| token.token_id.clone());
    let atm_home_text = canonicalize_lossy(atm_home).display().to_string();
    let request_id = token_id
        .as_ref()
        .map(|token_id| format!("daemon-launch-{token_id}"));
    let trace_id = request_id.as_deref().map(|request_id| {
        agent_team_mail_core::event_log::trace_id_for_request("atm-daemon", request_id)
    });
    let span_id = trace_id.as_deref().map(|trace_id| {
        agent_team_mail_core::event_log::span_id_for_action(trace_id, "daemon_start_rejected")
    });
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
        request_id,
        trace_id,
        span_id,
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

    let mut seen = seen_tokens().lock().expect("startup_auth mutex poisoned");
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

pub fn persist_runtime_metadata_from_token(home: &Path, token: &DaemonLaunchToken) -> Result<()> {
    let existing = agent_team_mail_core::daemon_client::read_runtime_metadata(home);
    let runtime_kind = agent_team_mail_core::daemon_client::RuntimeKind::Shared;
    let metadata = agent_team_mail_core::daemon_client::RuntimeMetadata {
        runtime_kind: runtime_kind.clone(),
        created_at: existing
            .as_ref()
            .map(|value| value.created_at.clone())
            .unwrap_or_else(|| Utc::now().to_rfc3339()),
        expires_at: None,
        allow_live_github_polling: existing
            .as_ref()
            .map(|value| value.allow_live_github_polling)
            .unwrap_or(true),
        test_identifier: None,
        owner_pid: None,
        token_id: Some(token.token_id.clone()),
    };
    agent_team_mail_core::daemon_client::write_runtime_metadata(home, &metadata)
}

#[cfg(test)]
fn note_test_lifecycle_event(event_name: &'static str) {
    let _ = event_name;
}

#[cfg(test)]
pub(crate) fn clear_seen_tokens_for_tests() {
    seen_tokens().lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_daemon_launch::{LaunchClass, encode_launch_token, issue_launch_token};
    use serial_test::serial;
    use std::time::Duration;
    use tempfile::TempDir;

    fn token_for(home: &Path, ttl_secs: i64) -> DaemonLaunchToken {
        let now = Utc::now();
        DaemonLaunchToken {
            launch_class: LaunchClass::Shared,
            atm_home: home.to_path_buf(),
            binary_identity: "test-binary".to_string(),
            issuer: "startup-auth-test".to_string(),
            token_id: uuid::Uuid::new_v4().to_string(),
            issued_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::seconds(ttl_secs)).to_rfc3339(),
            test_identifier: None,
            owner_pid: None,
        }
    }

    #[test]
    #[serial]
    fn missing_token_is_rejected() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let err = validate_token_inner(temp.path(), None).unwrap_err();
        assert!(matches!(err, StartupAuthError::MissingToken));
    }

    #[test]
    #[serial]
    fn invalid_token_is_rejected() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let err = validate_token_inner(temp.path(), Some("{not-json")).unwrap_err();
        assert!(matches!(err, StartupAuthError::InvalidToken(_)));
    }

    #[test]
    #[serial]
    fn expired_token_is_rejected() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let now = Utc::now();
        let token = DaemonLaunchToken {
            launch_class: LaunchClass::Shared,
            atm_home: temp.path().to_path_buf(),
            binary_identity: "test-binary".to_string(),
            issuer: "startup-auth-test".to_string(),
            token_id: uuid::Uuid::new_v4().to_string(),
            issued_at: (now - chrono::Duration::seconds(10)).to_rfc3339(),
            expires_at: (now - chrono::Duration::seconds(5)).to_rfc3339(),
            test_identifier: None,
            owner_pid: None,
        };
        let err = validate_token_inner(temp.path(), Some(&encode_launch_token(&token).unwrap()))
            .unwrap_err();
        assert!(matches!(err, StartupAuthError::ExpiredToken));
    }

    #[test]
    #[serial]
    fn wrong_home_is_rejected() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let token = token_for(other.path(), 30);
        let err = validate_token_inner(temp.path(), Some(&encode_launch_token(&token).unwrap()))
            .unwrap_err();
        assert!(matches!(err, StartupAuthError::WrongAtmHome));
    }

    #[test]
    #[serial]
    fn replayed_token_is_rejected() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let token = token_for(temp.path(), 30);
        let raw = encode_launch_token(&token).unwrap();
        assert!(validate_token_inner(temp.path(), Some(&raw)).is_ok());
        let err = validate_token_inner(temp.path(), Some(&raw)).unwrap_err();
        assert!(matches!(err, StartupAuthError::ReplayedToken));
    }

    #[test]
    #[serial]
    fn valid_token_is_accepted() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let token = issue_launch_token(
            LaunchClass::Shared,
            temp.path(),
            "test-binary",
            "startup-auth-test",
            Duration::from_secs(30),
        );
        let raw = encode_launch_token(&token).unwrap();
        let accepted = validate_token_inner(temp.path(), Some(&raw)).unwrap();
        assert_eq!(accepted.launch_class, LaunchClass::Shared);
    }

    #[test]
    #[serial]
    fn non_isolated_tokens_may_omit_lease_fields() {
        clear_seen_tokens_for_tests();
        // Shared tokens require the real OS home dir. On Windows,
        // dirs::home_dir() bypasses USERPROFILE env overrides, so TempDir
        // cannot classify as shared runtime.
        let os_home = agent_team_mail_core::home::get_os_home_dir().unwrap();
        let token = issue_launch_token(
            LaunchClass::Shared,
            &os_home,
            "test-binary",
            "startup-auth-test",
            Duration::from_secs(30),
        );
        let raw = encode_launch_token(&token).unwrap();
        let accepted = validate_token_inner(&os_home, Some(&raw)).unwrap();
        assert_eq!(accepted.launch_class, LaunchClass::Shared);
    }
}
