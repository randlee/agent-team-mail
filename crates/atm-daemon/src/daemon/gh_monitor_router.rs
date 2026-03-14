//! Daemon transport adapter for CI monitor commands.
//!
//! This module is the only place that should know about daemon-client wire payloads
//! (`SocketRequest`, `SocketResponse`, `GhMonitorRequest`, etc.). It translates those
//! transport types into the CI monitor core request/status types, delegates to the
//! transport-free `plugins::ci_monitor::service` APIs, then maps the results back to
//! wire responses.

#[cfg(unix)]
use crate::plugins::ci_monitor::service;
#[cfg(unix)]
use crate::plugins::ci_monitor::types::{
    CiMonitorControlRequest, CiMonitorHealth, CiMonitorLifecycleAction, CiMonitorRequest,
    CiMonitorStatus, CiMonitorStatusRequest, CiMonitorTargetKind,
};
use agent_team_mail_core::daemon_client::{
    GhMonitorControlRequest, GhMonitorRequest, GhStatusRequest, PROTOCOL_VERSION, SocketError,
    SocketRequest, SocketResponse,
};

const SOCKET_ERROR_INTERNAL_ERROR: &str = "INTERNAL_ERROR";
const SOCKET_ERROR_INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
const SOCKET_ERROR_VERSION_MISMATCH: &str = "VERSION_MISMATCH";

#[cfg(unix)]
struct ParsedWireRequest<T> {
    request_id: String,
    payload: T,
}

#[cfg(unix)]
struct ParsedHealthRequest {
    team: String,
    config_cwd: Option<String>,
    repo: Option<String>,
}

#[cfg(unix)]
fn target_kind_from_wire(
    kind: agent_team_mail_core::daemon_client::GhMonitorTargetKind,
) -> CiMonitorTargetKind {
    match kind {
        agent_team_mail_core::daemon_client::GhMonitorTargetKind::Pr => CiMonitorTargetKind::Pr,
        agent_team_mail_core::daemon_client::GhMonitorTargetKind::Workflow => {
            CiMonitorTargetKind::Workflow
        }
        agent_team_mail_core::daemon_client::GhMonitorTargetKind::Run => CiMonitorTargetKind::Run,
    }
}

#[cfg(unix)]
fn target_kind_to_wire(
    kind: CiMonitorTargetKind,
) -> agent_team_mail_core::daemon_client::GhMonitorTargetKind {
    match kind {
        CiMonitorTargetKind::Pr => agent_team_mail_core::daemon_client::GhMonitorTargetKind::Pr,
        CiMonitorTargetKind::Workflow => {
            agent_team_mail_core::daemon_client::GhMonitorTargetKind::Workflow
        }
        CiMonitorTargetKind::Run => agent_team_mail_core::daemon_client::GhMonitorTargetKind::Run,
    }
}

#[cfg(unix)]
fn lifecycle_action_from_wire(
    action: agent_team_mail_core::daemon_client::GhMonitorLifecycleAction,
) -> CiMonitorLifecycleAction {
    match action {
        agent_team_mail_core::daemon_client::GhMonitorLifecycleAction::Start => {
            CiMonitorLifecycleAction::Start
        }
        agent_team_mail_core::daemon_client::GhMonitorLifecycleAction::Stop => {
            CiMonitorLifecycleAction::Stop
        }
        agent_team_mail_core::daemon_client::GhMonitorLifecycleAction::Restart => {
            CiMonitorLifecycleAction::Restart
        }
    }
}

#[cfg(unix)]
fn monitor_request_from_wire(
    request: GhMonitorRequest,
) -> (
    CiMonitorRequest,
    Option<String>,
    Option<String>,
    Vec<String>,
) {
    let repo = request.repo;
    let caller_agent = request.caller_agent;
    let cc = request.cc;
    (
        CiMonitorRequest {
            team: request.team,
            target_kind: target_kind_from_wire(request.target_kind),
            target: request.target,
            reference: request.reference,
            start_timeout_secs: request.start_timeout_secs,
            config_cwd: request.config_cwd,
        },
        repo,
        caller_agent,
        cc,
    )
}

#[cfg(unix)]
#[allow(clippy::result_large_err)]
fn parse_wire_request<T>(
    request_str: &str,
    request_label: &str,
) -> Result<ParsedWireRequest<T>, SocketResponse>
where
    T: serde::de::DeserializeOwned,
{
    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return Err(make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse {request_label} request: {e}"),
            ));
        }
    };

    if request.version != PROTOCOL_VERSION {
        return Err(make_error_response(
            &request.request_id,
            SOCKET_ERROR_VERSION_MISMATCH,
            &format!(
                "Unsupported protocol version {}; server supports {}",
                request.version, PROTOCOL_VERSION
            ),
        ));
    }

    let payload: T = match serde_json::from_value(request.payload) {
        Ok(payload) => payload,
        Err(e) => {
            return Err(make_error_response(
                &request.request_id,
                SOCKET_ERROR_INVALID_PAYLOAD,
                &format!("Failed to parse {request_label} payload: {e}"),
            ));
        }
    };

    Ok(ParsedWireRequest {
        request_id: request.request_id,
        payload,
    })
}

#[cfg(unix)]
#[allow(clippy::result_large_err)]
fn parse_health_request(
    request_str: &str,
) -> Result<ParsedWireRequest<ParsedHealthRequest>, SocketResponse> {
    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return Err(make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse gh-monitor-health request: {e}"),
            ));
        }
    };
    if request.version != PROTOCOL_VERSION {
        return Err(make_error_response(
            &request.request_id,
            SOCKET_ERROR_VERSION_MISMATCH,
            &format!(
                "Unsupported protocol version {}; server supports {}",
                request.version, PROTOCOL_VERSION
            ),
        ));
    }

    let team = request
        .payload
        .get("team")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let config_cwd = request
        .payload
        .get("config_cwd")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let repo = request
        .payload
        .get("repo")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if team.is_empty() {
        return Err(make_error_response(
            &request.request_id,
            "MISSING_PARAMETER",
            "Missing required payload field: 'team'",
        ));
    }

    Ok(ParsedWireRequest {
        request_id: request.request_id,
        payload: ParsedHealthRequest {
            team,
            config_cwd,
            repo,
        },
    })
}

#[cfg(unix)]
fn status_request_from_wire(request: GhStatusRequest) -> (CiMonitorStatusRequest, Option<String>) {
    let repo = request.repo;
    (
        CiMonitorStatusRequest {
            team: request.team,
            target_kind: target_kind_from_wire(request.target_kind),
            target: request.target,
            reference: request.reference,
            config_cwd: request.config_cwd,
        },
        repo,
    )
}

#[cfg(unix)]
fn control_request_from_wire(request: GhMonitorControlRequest) -> CiMonitorControlRequest {
    CiMonitorControlRequest {
        team: request.team,
        action: lifecycle_action_from_wire(request.action),
        drain_timeout_secs: request.drain_timeout_secs,
        config_cwd: request.config_cwd,
    }
}

#[cfg(unix)]
fn status_to_wire(status: CiMonitorStatus) -> agent_team_mail_core::daemon_client::GhMonitorStatus {
    agent_team_mail_core::daemon_client::GhMonitorStatus {
        team: status.team,
        configured: status.configured,
        enabled: status.enabled,
        config_source: status.config_source,
        config_path: status.config_path,
        target_kind: target_kind_to_wire(status.target_kind),
        target: status.target,
        state: status.state,
        run_id: status.run_id,
        reference: status.reference,
        updated_at: status.updated_at,
        message: status.message,
        repo_state_updated_at: status.repo_state_updated_at,
    }
}

#[cfg(unix)]
fn health_to_wire(health: CiMonitorHealth) -> agent_team_mail_core::daemon_client::GhMonitorHealth {
    agent_team_mail_core::daemon_client::GhMonitorHealth {
        team: health.team,
        configured: health.configured,
        enabled: health.enabled,
        config_source: health.config_source,
        config_path: health.config_path,
        lifecycle_state: health.lifecycle_state,
        availability_state: health.availability_state,
        in_flight: health.in_flight,
        updated_at: health.updated_at,
        message: health.message,
        repo_state_updated_at: health.repo_state_updated_at,
        budget_limit_per_hour: health.budget_limit_per_hour,
        budget_used_in_window: health.budget_used_in_window,
        rate_limit_remaining: health.rate_limit_remaining,
        rate_limit_limit: health.rate_limit_limit,
        poll_owner: health.poll_owner,
    }
}

pub(crate) fn is_gh_monitor_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"gh-monitor""#)
        || request_str.contains(r#""command": "gh-monitor""#)
}

pub(crate) fn is_gh_status_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"gh-status""#)
        || request_str.contains(r#""command": "gh-status""#)
}

pub(crate) fn is_gh_monitor_control_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"gh-monitor-control""#)
        || request_str.contains(r#""command": "gh-monitor-control""#)
}

pub(crate) fn is_gh_monitor_health_command(request_str: &str) -> bool {
    request_str.contains(r#""command":"gh-monitor-health""#)
        || request_str.contains(r#""command": "gh-monitor-health""#)
}

#[cfg(unix)]
pub(crate) async fn maybe_route_async_command(
    request_str: &str,
    home: &std::path::Path,
) -> Option<SocketResponse> {
    if is_gh_monitor_command(request_str) {
        Some(handle_gh_monitor_command(request_str, home).await)
    } else if is_gh_monitor_control_command(request_str) {
        Some(handle_gh_monitor_control_command(request_str, home).await)
    } else if is_gh_monitor_health_command(request_str) {
        Some(handle_gh_monitor_health_command(request_str, home).await)
    } else if is_gh_status_command(request_str) {
        Some(handle_gh_status_command(request_str, home).await)
    } else {
        None
    }
}

#[cfg(not(unix))]
pub(crate) async fn maybe_route_async_command(
    _request_str: &str,
    _home: &std::path::Path,
) -> Option<SocketResponse> {
    None
}

pub(crate) fn async_dispatch_error(request_id: &str, command: &str) -> Option<SocketResponse> {
    let message = match command {
        "gh-monitor" => "gh-monitor command should have been handled by the async path",
        "gh-status" => "gh-status command should have been handled by the async path",
        "gh-monitor-control" => {
            "gh-monitor-control command should have been handled by the async path"
        }
        "gh-monitor-health" => {
            "gh-monitor-health command should have been handled by the async path"
        }
        _ => return None,
    };
    Some(make_error_response(
        request_id,
        SOCKET_ERROR_INTERNAL_ERROR,
        message,
    ))
}

#[cfg(unix)]
pub(crate) async fn handle_gh_monitor_command(
    request_str: &str,
    home: &std::path::Path,
) -> SocketResponse {
    let ParsedWireRequest {
        request_id,
        payload: gh_request,
    } = match parse_wire_request::<GhMonitorRequest>(request_str, "gh-monitor") {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let (gh_request, repo, caller_agent, cc) = monitor_request_from_wire(gh_request);

    match service::monitor_request(
        home,
        &gh_request,
        repo.as_deref(),
        caller_agent.as_deref(),
        &cc,
    )
    .await
    {
        Ok(status) => make_ok_response(
            &request_id,
            serde_json::to_value(status_to_wire(status)).unwrap_or_default(),
        ),
        Err(err) => make_error_response(&request_id, err.code, &err.message),
    }
}

#[cfg(unix)]
pub(crate) async fn handle_gh_monitor_control_command(
    request_str: &str,
    home: &std::path::Path,
) -> SocketResponse {
    let ParsedWireRequest {
        request_id,
        payload: control,
    } = match parse_wire_request::<GhMonitorControlRequest>(request_str, "gh-monitor-control") {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    match service::control_request(home, &control_request_from_wire(control)).await {
        Ok(health) => make_ok_response(
            &request_id,
            serde_json::to_value(health_to_wire(health)).unwrap_or_default(),
        ),
        Err(err) => make_error_response(&request_id, err.code, &err.message),
    }
}

#[cfg(unix)]
pub(crate) async fn handle_gh_monitor_health_command(
    request_str: &str,
    home: &std::path::Path,
) -> SocketResponse {
    let ParsedWireRequest {
        request_id,
        payload:
            ParsedHealthRequest {
                team,
                config_cwd,
                repo: _repo,
            },
    } = match parse_health_request(request_str) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };

    match service::health_request(home, &team, config_cwd.as_deref(), _repo.as_deref()) {
        Ok(health) => make_ok_response(
            &request_id,
            serde_json::to_value(health_to_wire(health)).unwrap_or_default(),
        ),
        Err(err) => make_error_response(&request_id, err.code, &err.message),
    }
}

#[cfg(unix)]
pub(crate) async fn handle_gh_status_command(
    request_str: &str,
    home: &std::path::Path,
) -> SocketResponse {
    let ParsedWireRequest {
        request_id,
        payload: gh_request,
    } = match parse_wire_request::<GhStatusRequest>(request_str, "gh-status") {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let (gh_request, repo) = status_request_from_wire(gh_request);

    match service::status_request(home, &gh_request, repo.as_deref()) {
        Ok(status) => make_ok_response(
            &request_id,
            serde_json::to_value(status_to_wire(status)).unwrap_or_default(),
        ),
        Err(err) => make_error_response(&request_id, err.code, &err.message),
    }
}

#[cfg(not(unix))]
pub(crate) async fn handle_gh_monitor_command(
    request_str: &str,
    _home: &std::path::Path,
) -> SocketResponse {
    let request_id = serde_json::from_str::<serde_json::Value>(request_str)
        .ok()
        .and_then(|value| {
            value
                .get("request_id")
                .and_then(|request_id| request_id.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    make_error_response(
        &request_id,
        "UNSUPPORTED_PLATFORM",
        "gh-monitor commands require Unix daemon transport",
    )
}

#[cfg(not(unix))]
pub(crate) async fn handle_gh_monitor_control_command(
    request_str: &str,
    _home: &std::path::Path,
) -> SocketResponse {
    let request_id = serde_json::from_str::<serde_json::Value>(request_str)
        .ok()
        .and_then(|value| {
            value
                .get("request_id")
                .and_then(|request_id| request_id.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    make_error_response(
        &request_id,
        "UNSUPPORTED_PLATFORM",
        "gh-monitor-control commands require Unix daemon transport",
    )
}

#[cfg(not(unix))]
pub(crate) async fn handle_gh_monitor_health_command(
    request_str: &str,
    _home: &std::path::Path,
) -> SocketResponse {
    let request_id = serde_json::from_str::<serde_json::Value>(request_str)
        .ok()
        .and_then(|value| {
            value
                .get("request_id")
                .and_then(|request_id| request_id.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    make_error_response(
        &request_id,
        "UNSUPPORTED_PLATFORM",
        "gh-monitor-health commands require Unix daemon transport",
    )
}

#[cfg(not(unix))]
pub(crate) async fn handle_gh_status_command(
    request_str: &str,
    _home: &std::path::Path,
) -> SocketResponse {
    let request_id = serde_json::from_str::<serde_json::Value>(request_str)
        .ok()
        .and_then(|value| {
            value
                .get("request_id")
                .and_then(|request_id| request_id.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    make_error_response(
        &request_id,
        "UNSUPPORTED_PLATFORM",
        "gh-status commands require Unix daemon transport",
    )
}

fn make_ok_response(request_id: &str, payload: serde_json::Value) -> SocketResponse {
    SocketResponse {
        version: PROTOCOL_VERSION,
        request_id: request_id.to_string(),
        status: "ok".to_string(),
        payload: Some(payload),
        error: None,
    }
}

fn make_error_response(request_id: &str, code: &str, message: &str) -> SocketResponse {
    SocketResponse {
        version: PROTOCOL_VERSION,
        request_id: request_id.to_string(),
        status: "error".to_string(),
        payload: None,
        error: Some(SocketError {
            code: code.to_string(),
            message: message.to_string(),
        }),
    }
}

#[cfg(all(test, unix))]
#[path = "gh_monitor_router_tests.rs"]
mod tests;
