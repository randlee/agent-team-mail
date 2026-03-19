//! Canonical daemon launch token issuance for product-owned launcher paths.
//!
//! Phase AU establishes this crate as the only allowed owner for daemon launch
//! token issuance. Other crates may consume the token schema, but they must not
//! issue new launch tokens themselves.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const ATM_LAUNCH_TOKEN_ENV: &str = "ATM_LAUNCH_TOKEN";
pub const DEFAULT_LAUNCH_TOKEN_TTL_SECS: u64 = 15;

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

/// Lease metadata required for isolated test daemons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolatedTestLease {
    pub test_identifier: String,
    pub owner_pid: u32,
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
    /// Stable test identifier for isolated test daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_identifier: Option<String>,
    /// Owning test-process PID for isolated test daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
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
        test_identifier: None,
        owner_pid: None,
    }
}

/// Issue a launch token for an isolated-test daemon with explicit lease metadata.
pub fn issue_isolated_test_launch_token(
    atm_home: &Path,
    binary_identity: impl Into<String>,
    issuer: impl Into<String>,
    test_identifier: impl Into<String>,
    owner_pid: u32,
    ttl: Duration,
) -> DaemonLaunchToken {
    let mut token = issue_launch_token(
        LaunchClass::IsolatedTest,
        atm_home,
        binary_identity,
        issuer,
        ttl,
    );
    token.test_identifier = Some(test_identifier.into());
    token.owner_pid = Some(owner_pid);
    token
}

pub fn encode_launch_token(token: &DaemonLaunchToken) -> serde_json::Result<String> {
    serde_json::to_string(token)
}

pub fn decode_launch_token(raw: &str) -> serde_json::Result<DaemonLaunchToken> {
    serde_json::from_str(raw)
}

pub fn attach_launch_token(
    command: &mut Command,
    token: &DaemonLaunchToken,
) -> serde_json::Result<()> {
    command.env(ATM_LAUNCH_TOKEN_ENV, encode_launch_token(token)?);
    Ok(())
}

pub struct SpawnDaemonRequest<'a> {
    pub daemon_bin: &'a OsStr,
    pub atm_home: &'a Path,
    pub launch_class: LaunchClass,
    pub issuer: &'a str,
    pub team: Option<&'a str>,
    pub stdin: Stdio,
    pub stdout: Stdio,
    pub stderr: Stdio,
}

fn scrub_shared_runtime_owner_env(command: &mut Command) {
    for key in ["CLAUDE_SESSION_ID", "ATM_IDENTITY", "ATM_TEAM"] {
        command.env_remove(key);
    }
}

fn inherited_shared_runtime_session_id() -> Option<String> {
    for key in ["CLAUDE_SESSION_ID", "ATM_SESSION_ID", "CODEX_THREAD_ID"] {
        if let Some(value) = std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            return Some(value);
        }
    }
    None
}

fn configure_spawn_command(
    command: &mut Command,
    request: SpawnDaemonRequest<'_>,
    token: &DaemonLaunchToken,
) -> serde_json::Result<()> {
    if let Some(team) = request.team {
        command.arg("--team").arg(team);
    }
    command
        .env("ATM_HOME", request.atm_home)
        .stdin(request.stdin)
        .stdout(request.stdout)
        .stderr(request.stderr);
    if request.launch_class != LaunchClass::IsolatedTest {
        if let Some(session_id) = inherited_shared_runtime_session_id() {
            command.env("ATM_SESSION_ID", session_id);
        }
        scrub_shared_runtime_owner_env(command);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
    }
    attach_launch_token(command, token)
}

/// Spawn `atm-daemon` through the canonical launcher surface.
pub fn spawn_daemon_process(request: SpawnDaemonRequest<'_>) -> io::Result<Child> {
    let binary_identity = request.daemon_bin.to_string_lossy().into_owned();
    let token = if request.launch_class == LaunchClass::IsolatedTest {
        issue_isolated_test_launch_token(
            request.atm_home,
            binary_identity,
            request.issuer,
            format!("{}:{}", request.issuer, std::process::id()),
            std::process::id(),
            Duration::from_secs(DEFAULT_LAUNCH_TOKEN_TTL_SECS),
        )
    } else {
        issue_launch_token(
            request.launch_class,
            request.atm_home,
            binary_identity,
            request.issuer,
            Duration::from_secs(DEFAULT_LAUNCH_TOKEN_TTL_SECS),
        )
    };

    let mut command = Command::new(request.daemon_bin);
    configure_spawn_command(&mut command, request, &token)
        .map_err(|e| io::Error::other(format!("failed to encode daemon launch token: {e}")))?;
    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

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
        assert_eq!(token.test_identifier, None);
        assert_eq!(token.owner_pid, None);
    }

    #[test]
    fn launch_class_serializes_as_kebab_case() {
        let encoded = serde_json::to_string(&LaunchClass::IsolatedTest).unwrap();
        assert_eq!(encoded, "\"isolated-test\"");
    }

    #[test]
    fn encode_decode_roundtrip_preserves_token() {
        let prod_home = std::env::temp_dir().join("prod-home");
        let token = issue_launch_token(
            LaunchClass::ProdShared,
            &prod_home,
            "/opt/homebrew/bin/atm-daemon",
            "launcher-test",
            Duration::from_secs(15),
        );
        let raw = encode_launch_token(&token).unwrap();
        let decoded = decode_launch_token(&raw).unwrap();
        assert_eq!(decoded, token);
    }

    #[test]
    fn issue_isolated_test_launch_token_sets_lease_fields() {
        let atm_home = std::env::temp_dir().join("isolated-home");
        let token = issue_isolated_test_launch_token(
            &atm_home,
            "target/debug/atm-daemon",
            "launcher-test",
            "daemon_tests::test_daemon_start_requires_launch_token",
            4242,
            Duration::from_secs(600),
        );

        assert_eq!(token.launch_class, LaunchClass::IsolatedTest);
        assert_eq!(
            token.test_identifier.as_deref(),
            Some("daemon_tests::test_daemon_start_requires_launch_token")
        );
        assert_eq!(token.owner_pid, Some(4242));
    }

    #[test]
    fn shared_runtime_spawn_scrubs_owner_session_env() {
        let atm_home = std::env::temp_dir().join("shared-home");
        let token = issue_launch_token(
            LaunchClass::DevShared,
            &atm_home,
            "target/debug/atm-daemon",
            "launcher-test",
            Duration::from_secs(15),
        );
        let mut command = Command::new("sh");
        command
            .env("CLAUDE_SESSION_ID", "session-123")
            .env("ATM_IDENTITY", "team-lead")
            .env("ATM_TEAM", "atm-dev");

        configure_spawn_command(
            &mut command,
            SpawnDaemonRequest {
                daemon_bin: OsStr::new("atm-daemon"),
                atm_home: &atm_home,
                launch_class: LaunchClass::DevShared,
                issuer: "launcher-test",
                team: Some("atm-dev"),
                stdin: Stdio::null(),
                stdout: Stdio::null(),
                stderr: Stdio::null(),
            },
            &token,
        )
        .unwrap();

        let envs: std::collections::HashMap<_, _> = command.get_envs().collect();
        assert_eq!(envs.get(OsStr::new("CLAUDE_SESSION_ID")), Some(&None));
        assert_eq!(envs.get(OsStr::new("ATM_IDENTITY")), Some(&None));
        assert_eq!(envs.get(OsStr::new("ATM_TEAM")), Some(&None));
    }

    #[test]
    #[serial]
    fn shared_runtime_spawn_preserves_runtime_session_and_otel_env() {
        let atm_home = std::env::temp_dir().join("shared-home");
        let token = issue_launch_token(
            LaunchClass::DevShared,
            &atm_home,
            "target/debug/atm-daemon",
            "launcher-test",
            Duration::from_secs(15),
        );
        let mut command = Command::new("sh");
        command
            .env("CLAUDE_SESSION_ID", "session-123")
            .env("ATM_OTEL_ENABLED", "true")
            .env("ATM_OTEL_ENDPOINT", "http://collector:4318")
            .env("ATM_OTEL_PROTOCOL", "otlp_http")
            .env("ATM_OTEL_AUTH_HEADER", "Authorization: Bearer test-token")
            .env("ATM_OTEL_CA_FILE", "/path/to/ca.pem")
            .env("ATM_OTEL_INSECURE_SKIP_VERIFY", "true")
            .env("ATM_OTEL_DEBUG_LOCAL_EXPORT", "1");
        let old_claude = std::env::var("CLAUDE_SESSION_ID").ok();
        // SAFETY: test-scoped env mutation for launch inheritance check.
        unsafe { std::env::set_var("CLAUDE_SESSION_ID", "session-123") };

        configure_spawn_command(
            &mut command,
            SpawnDaemonRequest {
                daemon_bin: OsStr::new("atm-daemon"),
                atm_home: &atm_home,
                launch_class: LaunchClass::DevShared,
                issuer: "launcher-test",
                team: Some("atm-dev"),
                stdin: Stdio::null(),
                stdout: Stdio::null(),
                stderr: Stdio::null(),
            },
            &token,
        )
        .unwrap();

        let envs: std::collections::HashMap<_, _> = command.get_envs().collect();
        assert_eq!(
            envs.get(OsStr::new("ATM_SESSION_ID")),
            Some(&Some(OsStr::new("session-123")))
        );
        assert_eq!(
            envs.get(OsStr::new("ATM_OTEL_ENABLED")),
            Some(&Some(OsStr::new("true")))
        );
        assert_eq!(
            envs.get(OsStr::new("ATM_OTEL_ENDPOINT")),
            Some(&Some(OsStr::new("http://collector:4318")))
        );
        assert_eq!(
            envs.get(OsStr::new("ATM_OTEL_PROTOCOL")),
            Some(&Some(OsStr::new("otlp_http")))
        );
        assert_eq!(
            envs.get(OsStr::new("ATM_OTEL_AUTH_HEADER")),
            Some(&Some(OsStr::new("Authorization: Bearer test-token")))
        );
        assert_eq!(
            envs.get(OsStr::new("ATM_OTEL_CA_FILE")),
            Some(&Some(OsStr::new("/path/to/ca.pem")))
        );
        assert_eq!(
            envs.get(OsStr::new("ATM_OTEL_INSECURE_SKIP_VERIFY")),
            Some(&Some(OsStr::new("true")))
        );
        assert_eq!(
            envs.get(OsStr::new("ATM_OTEL_DEBUG_LOCAL_EXPORT")),
            Some(&Some(OsStr::new("1")))
        );

        match old_claude {
            Some(value) => {
                // SAFETY: restore prior test-scoped env value.
                unsafe { std::env::set_var("CLAUDE_SESSION_ID", value) };
            }
            None => {
                // SAFETY: restore prior absence.
                unsafe { std::env::remove_var("CLAUDE_SESSION_ID") };
            }
        }
    }

    #[test]
    fn isolated_test_spawn_keeps_caller_env_available() {
        let atm_home = std::env::temp_dir().join("isolated-home");
        let token = issue_isolated_test_launch_token(
            &atm_home,
            "target/debug/atm-daemon",
            "launcher-test",
            "daemon_tests::isolated",
            4242,
            Duration::from_secs(600),
        );
        let mut command = Command::new("sh");
        command
            .env("CLAUDE_SESSION_ID", "session-123")
            .env("ATM_IDENTITY", "team-lead")
            .env("ATM_TEAM", "atm-dev");

        configure_spawn_command(
            &mut command,
            SpawnDaemonRequest {
                daemon_bin: OsStr::new("atm-daemon"),
                atm_home: &atm_home,
                launch_class: LaunchClass::IsolatedTest,
                issuer: "launcher-test",
                team: None,
                stdin: Stdio::null(),
                stdout: Stdio::null(),
                stderr: Stdio::null(),
            },
            &token,
        )
        .unwrap();

        let envs: std::collections::HashMap<_, _> = command.get_envs().collect();
        assert_eq!(
            envs.get(OsStr::new("CLAUDE_SESSION_ID")),
            Some(&Some(OsStr::new("session-123")))
        );
        assert_eq!(
            envs.get(OsStr::new("ATM_IDENTITY")),
            Some(&Some(OsStr::new("team-lead")))
        );
        assert_eq!(
            envs.get(OsStr::new("ATM_TEAM")),
            Some(&Some(OsStr::new("atm-dev")))
        );
    }
}
