//! Synthetic MCP tool definitions for ATM integration.
//!
//! These tool schemas are appended to `tools/list` responses from the Codex child,
//! making ATM messaging and session management tools available to Claude alongside
//! the native `codex` and `codex-reply` tools.
//!
//! Tool implementations are stubbed for Sprint A.2 (schemas only). Actual execution
//! logic will be added in Sprint A.4+.

use serde_json::{Value, json};

/// Number of synthetic tools that the proxy appends to `tools/list` responses.
pub const SYNTHETIC_TOOL_COUNT: usize = 7;

/// Return all synthetic tool definitions as JSON values.
///
/// These are appended to the `result.tools` array in `tools/list` responses
/// from the child process.
pub fn synthetic_tools() -> Vec<Value> {
    vec![
        atm_send_schema(),
        atm_read_schema(),
        atm_broadcast_schema(),
        atm_pending_count_schema(),
        agent_sessions_schema(),
        agent_status_schema(),
        agent_close_schema(),
    ]
}

fn atm_send_schema() -> Value {
    json!({
        "name": "atm_send",
        "description": "Send a message to an ATM team member",
        "inputSchema": {
            "type": "object",
            "properties": {
                "to": {"type": "string", "description": "Recipient agent name or agent@team"},
                "message": {"type": "string", "description": "Message text"},
                "summary": {"type": "string", "description": "Optional message summary"},
                "identity": {"type": "string", "description": "Explicit sender identity (required outside thread context)"}
            },
            "required": ["to", "message"]
        }
    })
}

fn atm_read_schema() -> Value {
    json!({
        "name": "atm_read",
        "description": "Read unread ATM messages from inbox",
        "inputSchema": {
            "type": "object",
            "properties": {
                "all": {"type": "boolean", "description": "Include already-read messages (default: false)"},
                "mark_read": {"type": "boolean", "description": "Mark returned messages as read (default: true)"},
                "limit": {"type": "integer", "description": "Max messages to return"},
                "since": {"type": "string", "description": "ISO 8601 timestamp filter"},
                "from": {"type": "string", "description": "Filter by sender name"},
                "identity": {"type": "string", "description": "Explicit identity (required outside thread context)"}
            }
        }
    })
}

fn atm_broadcast_schema() -> Value {
    json!({
        "name": "atm_broadcast",
        "description": "Broadcast a message to all ATM team members",
        "inputSchema": {
            "type": "object",
            "properties": {
                "message": {"type": "string", "description": "Message text"},
                "summary": {"type": "string", "description": "Optional message summary"},
                "team": {"type": "string", "description": "Override target team"},
                "identity": {"type": "string", "description": "Explicit sender identity (required outside thread context)"}
            },
            "required": ["message"]
        }
    })
}

fn atm_pending_count_schema() -> Value {
    json!({
        "name": "atm_pending_count",
        "description": "Get count of unread ATM messages without marking them read",
        "inputSchema": {
            "type": "object",
            "properties": {
                "identity": {"type": "string", "description": "Explicit identity (required outside thread context)"}
            }
        }
    })
}

fn agent_sessions_schema() -> Value {
    json!({
        "name": "agent_sessions",
        "description": "List active and resumable Codex agent sessions",
        "inputSchema": {
            "type": "object",
            "properties": {
                "include_closed": {"type": "boolean", "description": "Include closed sessions (default: false)"}
            }
        }
    })
}

fn agent_status_schema() -> Value {
    json!({
        "name": "agent_status",
        "description": "Get proxy health and active session information",
        "inputSchema": {
            "type": "object",
            "properties": {}
        }
    })
}

fn agent_close_schema() -> Value {
    json!({
        "name": "agent_close",
        "description": "Close an active agent session and release its identity",
        "inputSchema": {
            "type": "object",
            "properties": {
                "agent_id": {"type": "string", "description": "Agent ID to close"},
                "identity": {"type": "string", "description": "Identity to close (alternative to agent_id)"}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_synthetic_tools_count() {
        assert_eq!(synthetic_tools().len(), SYNTHETIC_TOOL_COUNT);
    }

    #[test]
    fn test_all_tools_have_name_and_schema() {
        for tool in synthetic_tools() {
            assert!(tool.get("name").is_some(), "tool missing name");
            assert!(tool.get("description").is_some(), "tool missing description");
            let schema = tool.get("inputSchema").expect("tool missing inputSchema");
            assert_eq!(
                schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "inputSchema must have type: object"
            );
        }
    }

    #[test]
    fn test_atm_send_required_fields() {
        let tool = atm_send_schema();
        let required = tool["inputSchema"]["required"]
            .as_array()
            .expect("atm_send must have required fields");
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"to"));
        assert!(names.contains(&"message"));
    }

    #[test]
    fn test_atm_broadcast_required_fields() {
        let tool = atm_broadcast_schema();
        let required = tool["inputSchema"]["required"]
            .as_array()
            .expect("atm_broadcast must have required fields");
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"message"));
    }
}
