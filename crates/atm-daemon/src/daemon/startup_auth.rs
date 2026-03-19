use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use agent_team_mail_core::event_log::{
    EventFields, emit_event_best_effort, emit_event_to_spool_direct,
};
use agent_team_mail_daemon_launch::{
    ATM_LAUNCH_TOKEN_ENV, DaemonLaunchToken, LaunchClass, decode_launch_token,
};
use anyhow::Result;
use chrono::Utc;
use thiserror::Error;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

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
    #[error("isolated-test launch token missing lease fields")]
    MissingIsolatedLeaseFields,
}

#[derive(Debug, Clone)]
pub struct LeaseViolation {
    pub event_name: &'static str,
    pub detail: String,
}

pub type SharedLeaseViolation = Arc<Mutex<Option<LeaseViolation>>>;

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

fn expected_launch_class(home: &Path) -> Result<LaunchClass> {
    Ok(
        match agent_team_mail_core::daemon_client::runtime_kind_for_home(home)? {
            agent_team_mail_core::daemon_client::RuntimeKind::Release => LaunchClass::ProdShared,
            agent_team_mail_core::daemon_client::RuntimeKind::Dev => LaunchClass::DevShared,
            agent_team_mail_core::daemon_client::RuntimeKind::Isolated => LaunchClass::IsolatedTest,
        },
    )
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

    if token.launch_class
        != expected_launch_class(home)
            .map_err(|err| StartupAuthError::InvalidToken(err.to_string()))?
    {
        return Err(StartupAuthError::WrongLaunchClass);
    }

    if token.launch_class == LaunchClass::IsolatedTest
        && (token
            .test_identifier
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
            || token.owner_pid.unwrap_or_default() <= 1)
    {
        return Err(StartupAuthError::MissingIsolatedLeaseFields);
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
                StartupAuthError::MissingIsolatedLeaseFields => {
                    StartupRejectionReason::InvalidToken
                }
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
    let runtime_kind = agent_team_mail_core::daemon_client::runtime_kind_for_home(home)?;
    let metadata = agent_team_mail_core::daemon_client::RuntimeMetadata {
        runtime_kind: runtime_kind.clone(),
        created_at: existing
            .as_ref()
            .map(|value| value.created_at.clone())
            .unwrap_or_else(|| Utc::now().to_rfc3339()),
        expires_at: matches!(
            runtime_kind,
            agent_team_mail_core::daemon_client::RuntimeKind::Isolated
        )
        .then(|| token.expires_at.clone()),
        allow_live_github_polling: existing
            .as_ref()
            .map(|value| value.allow_live_github_polling)
            .unwrap_or(false),
        test_identifier: token.test_identifier.clone(),
        owner_pid: token.owner_pid,
        token_id: Some(token.token_id.clone()),
    };
    agent_team_mail_core::daemon_client::write_runtime_metadata(home, &metadata)
}

pub fn sweep_stale_isolated_runtimes() -> Result<Vec<PathBuf>> {
    let reaped = agent_team_mail_core::daemon_client::reap_expired_isolated_runtime_roots()?;
    for home in &reaped {
        emit_lifecycle_event(
            "warn",
            "janitor_reap",
            None,
            home,
            Some("reaped stale isolated runtime after TTL expiry and dead owner"),
        );
    }
    Ok(reaped)
}

pub fn new_shared_lease_violation() -> SharedLeaseViolation {
    Arc::new(Mutex::new(None))
}

pub fn spawn_isolated_test_lease_monitor(
    home: PathBuf,
    token: DaemonLaunchToken,
    cancel: CancellationToken,
    lease_violation: SharedLeaseViolation,
) -> Option<JoinHandle<()>> {
    if token.launch_class != LaunchClass::IsolatedTest {
        return None;
    }

    Some(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    if let Ok(expires_at) = chrono::DateTime::parse_from_rfc3339(&token.expires_at)
                        && expires_at.with_timezone(&Utc) <= Utc::now()
                    {
                        emit_lifecycle_event(
                            "warn",
                            "ttl_expiry_shutdown",
                            Some(&token),
                            &home,
                            Some("isolated-test daemon reached lease expiry"),
                        );
                        *lease_violation.lock().unwrap() = Some(LeaseViolation {
                            event_name: "ttl_expiry_shutdown",
                            detail: "isolated-test daemon reached lease expiry".to_string(),
                        });
                        cancel.cancel();
                        break;
                    }

                    if let Some(owner_pid) = token.owner_pid
                        && !crate::daemon::is_pid_alive(owner_pid)
                    {
                        emit_lifecycle_event(
                            "warn",
                            "dead_owner_shutdown",
                            Some(&token),
                            &home,
                            Some("isolated-test daemon owner process is no longer alive"),
                        );
                        *lease_violation.lock().unwrap() = Some(LeaseViolation {
                            event_name: "dead_owner_shutdown",
                            detail: format!("owner_pid {owner_pid} is no longer alive"),
                        });
                        cancel.cancel();
                        break;
                    }
                }
            }
        }
    }))
}

#[cfg(test)]
type TestLifecycleHook = Arc<dyn Fn(&'static str) + Send + Sync>;

#[cfg(test)]
fn test_lifecycle_hook_slot() -> &'static Mutex<Option<TestLifecycleHook>> {
    static TEST_LIFECYCLE_HOOK: OnceLock<Mutex<Option<TestLifecycleHook>>> = OnceLock::new();
    TEST_LIFECYCLE_HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn note_test_lifecycle_event(event_name: &'static str) {
    let hook = test_lifecycle_hook_slot()
        .lock()
        .expect("startup_auth test lifecycle hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        hook(event_name);
    }
}

#[cfg(test)]
pub(crate) fn install_test_lifecycle_hook(hook: TestLifecycleHook) {
    *test_lifecycle_hook_slot()
        .lock()
        .expect("startup_auth test lifecycle hook lock poisoned") = Some(hook);
}

#[cfg(test)]
pub(crate) fn clear_test_lifecycle_hook() {
    *test_lifecycle_hook_slot()
        .lock()
        .expect("startup_auth test lifecycle hook lock poisoned") = None;
}

#[cfg(test)]
pub(crate) fn clear_seen_tokens_for_tests() {
    seen_tokens().lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_team_mail_core::daemon_client::{
        RuntimeKind, RuntimeMetadata, write_runtime_metadata,
    };
    use agent_team_mail_core::logging::{RotationConfig, UnifiedLogMode, init_unified};
    use agent_team_mail_daemon_launch::{
        encode_launch_token, issue_isolated_test_launch_token, issue_launch_token,
    };
    use serial_test::serial;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tempfile::{Builder, TempDir};

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set_path(key: &'static str, value: &std::path::Path) -> Self {
            let old = std::env::var(key).ok();
            // SAFETY: test-scoped env mutation guarded by RAII restore in Drop.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: test-scoped env restore.
            unsafe {
                if let Some(old) = &self.old {
                    std::env::set_var(self.key, old);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    fn isolated_runtime_root(name: &str) -> TempDir {
        let root = std::env::temp_dir().join("atm-isolated");
        fs::create_dir_all(&root).expect("create isolated runtime root parent");
        Builder::new()
            .prefix(name)
            .tempdir_in(root)
            .expect("create isolated runtime root tempdir")
    }

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
            test_identifier: (class == LaunchClass::IsolatedTest)
                .then(|| "startup-auth-test".to_string()),
            owner_pid: (class == LaunchClass::IsolatedTest).then_some(4242),
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
            launch_class: LaunchClass::IsolatedTest,
            atm_home: temp.path().to_path_buf(),
            binary_identity: "test-binary".to_string(),
            issuer: "startup-auth-test".to_string(),
            token_id: uuid::Uuid::new_v4().to_string(),
            issued_at: (now - chrono::Duration::seconds(10)).to_rfc3339(),
            expires_at: (now - chrono::Duration::seconds(5)).to_rfc3339(),
            test_identifier: Some("expired-token-test".to_string()),
            owner_pid: Some(4242),
        };
        let err = validate_token_inner(temp.path(), Some(&encode_launch_token(&token).unwrap()))
            .unwrap_err();
        assert!(matches!(err, StartupAuthError::ExpiredToken));
    }

    #[test]
    #[serial]
    fn isolated_test_token_missing_lease_is_rejected() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let mut token = token_for(temp.path(), LaunchClass::IsolatedTest, 30);
        token.test_identifier = None;
        token.owner_pid = None;
        let err = validate_token_inner(temp.path(), Some(&encode_launch_token(&token).unwrap()))
            .unwrap_err();
        assert!(matches!(err, StartupAuthError::MissingIsolatedLeaseFields));
    }

    #[test]
    #[serial]
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
    #[serial]
    fn wrong_class_is_rejected() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let token = token_for(temp.path(), LaunchClass::ProdShared, 30);
        let err = validate_token_inner(temp.path(), Some(&encode_launch_token(&token).unwrap()))
            .unwrap_err();
        assert!(matches!(err, StartupAuthError::WrongLaunchClass));
    }

    #[test]
    #[serial]
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
    #[serial]
    fn valid_token_is_accepted() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let token = issue_isolated_test_launch_token(
            temp.path(),
            "test-binary",
            "startup-auth-test",
            "startup-auth-test",
            std::process::id(),
            Duration::from_secs(30),
        );
        let raw = encode_launch_token(&token).unwrap();
        let accepted = validate_token_inner(temp.path(), Some(&raw)).unwrap();
        assert_eq!(accepted.launch_class, LaunchClass::IsolatedTest);
    }

    #[test]
    #[serial]
    fn non_isolated_tokens_may_omit_lease_fields() {
        clear_seen_tokens_for_tests();
        // ProdShared tokens require the real OS home dir. On Windows,
        // dirs::home_dir() bypasses USERPROFILE env overrides, so TempDir
        // cannot classify as shared runtime.
        let os_home = agent_team_mail_core::home::get_os_home_dir().unwrap();
        let token = issue_launch_token(
            LaunchClass::ProdShared,
            &os_home,
            "test-binary",
            "startup-auth-test",
            Duration::from_secs(30),
        );
        let raw = encode_launch_token(&token).unwrap();
        let accepted = validate_token_inner(&os_home, Some(&raw)).unwrap();
        assert_eq!(accepted.launch_class, LaunchClass::ProdShared);
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn sweep_stale_isolated_runtimes_emits_janitor_reap_event() {
        clear_seen_tokens_for_tests();
        let temp = TempDir::new().unwrap();
        let _home_guard = EnvGuard::set_path("ATM_HOME", temp.path());
        let log_path = temp.path().join("logs/atm-daemon.jsonl");
        let _guards = init_unified(
            "atm-daemon",
            UnifiedLogMode::DaemonWriter {
                file_path: log_path.clone(),
                rotation: RotationConfig::default(),
            },
        )
        .expect("init daemon-writer logging");

        let runtime_home_guard =
            isolated_runtime_root(&format!("startup-auth-janitor-{}", uuid::Uuid::new_v4()));
        let runtime_home = runtime_home_guard.path().to_path_buf();
        fs::create_dir_all(runtime_home.join(".atm").join("daemon")).unwrap();
        let metadata = RuntimeMetadata {
            runtime_kind: RuntimeKind::Isolated,
            created_at: "2026-03-14T00:00:00Z".to_string(),
            expires_at: Some("2026-03-14T00:10:00Z".to_string()),
            test_identifier: Some("startup-auth::janitor".to_string()),
            owner_pid: Some(999_999),
            token_id: Some("token-janitor".to_string()),
            allow_live_github_polling: false,
        };
        write_runtime_metadata(&runtime_home, &metadata).unwrap();

        let observed = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        install_test_lifecycle_hook({
            let observed = Arc::clone(&observed);
            Arc::new(move |event_name| {
                observed
                    .lock()
                    .expect("capture startup_auth lifecycle events")
                    .push(event_name);
            })
        });
        let reaped = sweep_stale_isolated_runtimes().unwrap();
        clear_test_lifecycle_hook();
        assert!(
            reaped.contains(&runtime_home),
            "expected janitor sweep to reap test runtime; saw {reaped:?}"
        );
        assert!(!runtime_home.exists(), "stale runtime should be removed");

        assert!(
            observed
                .lock()
                .expect("read observed lifecycle events")
                .contains(&"janitor_reap"),
            "expected janitor_reap lifecycle event to be emitted synchronously"
        );
    }
}
