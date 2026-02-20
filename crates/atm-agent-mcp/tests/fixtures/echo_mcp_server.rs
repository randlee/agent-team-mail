//! Mock MCP server for integration testing.
//!
//! Reads newline-delimited JSON from stdin and writes newline-delimited JSON
//! responses to stdout. Implements enough of the Codex MCP server protocol to
//! exercise the proxy's framing, pass-through, interception, and event forwarding.
//!
//! # Supported methods
//!
//! - `initialize` — returns Codex-compatible capabilities
//! - `tools/list` — returns `codex` and `codex-reply` tool schemas
//! - `tools/call` — echoes back with `structuredContent` and sends events
//! - `notifications/initialized` — accepted, no response
//! - `notifications/cancelled` — accepted, no response
//!
//! # Special behaviors
//!
//! - When `tools/call` targets `codex` or `codex-reply`, the server emits 2
//!   `codex/event` notifications before the response.
//! - When `tools/call` arguments contain `"slow": true`, the server sleeps for
//!   5 seconds before responding (for timeout testing).
//! - When `tools/call` targets `crash`, the server exits with code 42.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        handle_message(&msg, &mut writer);
    }
}

fn handle_message(msg: &Value, writer: &mut impl Write) {
    let method = msg.get("method").and_then(|v| v.as_str());
    let id = msg.get("id").cloned();

    match method {
        Some("initialize") => {
            let resp = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {
                        "tools": { "listChanged": true }
                    },
                    "serverInfo": {
                        "name": "echo-mcp-server",
                        "title": "Echo MCP Server (test)",
                        "version": "0.1.0"
                    }
                }
            });
            write_msg(writer, &resp);
        }

        Some("tools/list") => {
            let resp = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [
                        {
                            "name": "codex",
                            "description": "Start new Codex session",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "prompt": {"type": "string"},
                                    "cwd": {"type": "string"}
                                },
                                "required": ["prompt"]
                            }
                        },
                        {
                            "name": "codex-reply",
                            "description": "Continue existing Codex session",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "prompt": {"type": "string"},
                                    "threadId": {"type": "string"}
                                },
                                "required": ["prompt"]
                            }
                        }
                    ]
                }
            });
            write_msg(writer, &resp);
        }

        Some("tools/call") => {
            let tool_name = msg
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let arguments = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(json!({}));

            // Special: crash on demand
            if tool_name == "crash" {
                std::process::exit(42);
            }

            // Special: slow mode for timeout testing
            if arguments.get("slow").and_then(|v| v.as_bool()) == Some(true) {
                std::thread::sleep(std::time::Duration::from_secs(5));
            }

            let req_id = id.clone().unwrap_or(Value::Null);
            let thread_id = arguments
                .get("threadId")
                .and_then(|v| v.as_str())
                .unwrap_or("test-thread-001");

            // Emit 2 codex/event notifications before the response
            if tool_name == "codex" || tool_name == "codex-reply" {
                for i in 0..2 {
                    let event = json!({
                        "jsonrpc": "2.0",
                        "method": "codex/event",
                        "params": {
                            "_meta": {
                                "requestId": req_id,
                                "threadId": thread_id
                            },
                            "id": format!("evt-{i}"),
                            "msg": {
                                "type": if i == 0 { "session_configured" } else { "agent_message" },
                                "text": format!("event {i} for {tool_name}")
                            }
                        }
                    });
                    write_msg(writer, &event);
                }
            }

            let prompt = arguments
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("(no prompt)");

            let resp = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{
                        "type": "text",
                        "text": format!("Echo from {tool_name}: {prompt}")
                    }],
                    "structuredContent": {
                        "threadId": thread_id,
                        "content": format!("Echo from {tool_name}: {prompt}")
                    }
                }
            });
            write_msg(writer, &resp);
        }

        Some("notifications/initialized") | Some("notifications/cancelled") => {
            // Notifications have no response
        }

        Some(unknown) => {
            // Unknown method — return error
            if let Some(req_id) = id {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": {
                        "code": -32601,
                        "message": format!("Method not found: {unknown}")
                    }
                });
                write_msg(writer, &resp);
            }
        }

        None => {
            // Response from client (e.g. elicitation response) — no action needed
        }
    }
}

fn write_msg(writer: &mut impl Write, msg: &Value) {
    let s = serde_json::to_string(msg).expect("serialize JSON");
    writeln!(writer, "{s}").expect("write to stdout");
    writer.flush().expect("flush stdout");
}
