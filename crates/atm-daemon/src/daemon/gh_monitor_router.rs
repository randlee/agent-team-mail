#[cfg(unix)]
use crate::plugins::ci_monitor::service;
use agent_team_mail_core::daemon_client::{
    GhMonitorControlRequest, GhMonitorRequest, GhStatusRequest, PROTOCOL_VERSION, SocketError,
    SocketRequest, SocketResponse,
};

const SOCKET_ERROR_INTERNAL_ERROR: &str = "INTERNAL_ERROR";
const SOCKET_ERROR_INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
const SOCKET_ERROR_VERSION_MISMATCH: &str = "VERSION_MISMATCH";

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
    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse gh-monitor request: {e}"),
            );
        }
    };

    if request.version != PROTOCOL_VERSION {
        return make_error_response(
            &request.request_id,
            SOCKET_ERROR_VERSION_MISMATCH,
            &format!(
                "Unsupported protocol version {}; server supports {}",
                request.version, PROTOCOL_VERSION
            ),
        );
    }

    let gh_request: GhMonitorRequest = match serde_json::from_value(request.payload.clone()) {
        Ok(payload) => payload,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                SOCKET_ERROR_INVALID_PAYLOAD,
                &format!("Failed to parse gh-monitor payload: {e}"),
            );
        }
    };

    match service::monitor_request(home, &gh_request).await {
        Ok(status) => make_ok_response(
            &request.request_id,
            serde_json::to_value(status).unwrap_or_default(),
        ),
        Err(err) => make_error_response(&request.request_id, err.code, &err.message),
    }
}

#[cfg(unix)]
pub(crate) async fn handle_gh_monitor_control_command(
    request_str: &str,
    home: &std::path::Path,
) -> SocketResponse {
    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse gh-monitor-control request: {e}"),
            );
        }
    };
    if request.version != PROTOCOL_VERSION {
        return make_error_response(
            &request.request_id,
            SOCKET_ERROR_VERSION_MISMATCH,
            &format!(
                "Unsupported protocol version {}; server supports {}",
                request.version, PROTOCOL_VERSION
            ),
        );
    }

    let control: GhMonitorControlRequest = match serde_json::from_value(request.payload.clone()) {
        Ok(payload) => payload,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                SOCKET_ERROR_INVALID_PAYLOAD,
                &format!("Failed to parse gh-monitor-control payload: {e}"),
            );
        }
    };
    match service::control_request(home, &control).await {
        Ok(health) => make_ok_response(
            &request.request_id,
            serde_json::to_value(health).unwrap_or_default(),
        ),
        Err(err) => make_error_response(&request.request_id, err.code, &err.message),
    }
}

#[cfg(unix)]
pub(crate) async fn handle_gh_monitor_health_command(
    request_str: &str,
    home: &std::path::Path,
) -> SocketResponse {
    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse gh-monitor-health request: {e}"),
            );
        }
    };
    if request.version != PROTOCOL_VERSION {
        return make_error_response(
            &request.request_id,
            SOCKET_ERROR_VERSION_MISMATCH,
            &format!(
                "Unsupported protocol version {}; server supports {}",
                request.version, PROTOCOL_VERSION
            ),
        );
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
    if team.is_empty() {
        return make_error_response(
            &request.request_id,
            "MISSING_PARAMETER",
            "Missing required payload field: 'team'",
        );
    }

    match service::health_request(home, &team, config_cwd.as_deref()) {
        Ok(health) => make_ok_response(
            &request.request_id,
            serde_json::to_value(health).unwrap_or_default(),
        ),
        Err(err) => make_error_response(&request.request_id, err.code, &err.message),
    }
}

#[cfg(unix)]
pub(crate) async fn handle_gh_status_command(
    request_str: &str,
    home: &std::path::Path,
) -> SocketResponse {
    let request: SocketRequest = match serde_json::from_str(request_str) {
        Ok(r) => r,
        Err(e) => {
            return make_error_response(
                "unknown",
                "INVALID_REQUEST",
                &format!("Failed to parse gh-status request: {e}"),
            );
        }
    };

    if request.version != PROTOCOL_VERSION {
        return make_error_response(
            &request.request_id,
            SOCKET_ERROR_VERSION_MISMATCH,
            &format!(
                "Unsupported protocol version {}; server supports {}",
                request.version, PROTOCOL_VERSION
            ),
        );
    }

    let gh_request: GhStatusRequest = match serde_json::from_value(request.payload.clone()) {
        Ok(payload) => payload,
        Err(e) => {
            return make_error_response(
                &request.request_id,
                SOCKET_ERROR_INVALID_PAYLOAD,
                &format!("Failed to parse gh-status payload: {e}"),
            );
        }
    };

    match service::status_request(home, &gh_request) {
        Ok(status) => make_ok_response(
            &request.request_id,
            serde_json::to_value(status).unwrap_or_default(),
        ),
        Err(err) => make_error_response(&request.request_id, err.code, &err.message),
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
