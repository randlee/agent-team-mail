//! Codex watch-stream adapter for ATM TUI.
//!
//! Maps ATM/MCP watch frames into normalized Codex-style render events while
//! preserving stream order and incremental updates.

use crate::codex_vendor::text_formatting::format_json_compact;

#[derive(Debug, Clone)]
pub struct AdaptedWatchLine {
    pub line: String,
    pub is_turn_boundary: bool,
}

#[derive(Debug, Default)]
pub struct CodexAdapter {
    unknown_events: u64,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn unknown_events(&self) -> u64 {
        self.unknown_events
    }

    pub fn adapt_frame(&mut self, frame: &serde_json::Value) -> AdaptedWatchLine {
        let source_kind = frame
            .pointer("/source/kind")
            .and_then(|v| v.as_str())
            .unwrap_or("client_prompt");
        let source_actor = frame
            .pointer("/source/actor")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let source_channel = frame
            .pointer("/source/channel")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let source_badge = format!("[{source_kind}|{source_actor}|{source_channel}]");

        let event = frame.get("event").unwrap_or(frame);
        let kind = event
            .pointer("/params/type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let text = event
            .pointer("/params/delta")
            .and_then(|v| v.as_str())
            .or_else(|| event.pointer("/params/text").and_then(|v| v.as_str()))
            .or_else(|| event.pointer("/params/output").and_then(|v| v.as_str()))
            .or_else(|| event.pointer("/params/message").and_then(|v| v.as_str()))
            .unwrap_or("");
        let text = format_json_compact(text).unwrap_or_else(|| text.to_string());

        match kind {
            "turn_started" => AdaptedWatchLine {
                line: format!("{source_badge} turn.started"),
                is_turn_boundary: true,
            },
            "item_started" => AdaptedWatchLine {
                line: format!("{source_badge} item.started"),
                is_turn_boundary: false,
            },
            "item_delta" | "agent_message_delta" | "agent_message_chunk" => AdaptedWatchLine {
                line: format!("{source_badge} item.delta {text}"),
                is_turn_boundary: false,
            },
            "item_completed" => AdaptedWatchLine {
                line: format!("{source_badge} item.completed"),
                is_turn_boundary: false,
            },
            "turn_completed" | "task_complete" | "done" => AdaptedWatchLine {
                line: format!("{source_badge} turn.completed"),
                is_turn_boundary: true,
            },
            "turn_idle" | "idle" => AdaptedWatchLine {
                line: format!("{source_badge} turn.idle"),
                is_turn_boundary: true,
            },
            "exec_command_output_delta" | "exec_command_completed" | "exec_command_error" => {
                AdaptedWatchLine {
                    line: format!("{source_badge} cmd {text}"),
                    is_turn_boundary: false,
                }
            }
            "reasoning_content_delta" | "agent_reasoning_delta" | "reasoning_content" => {
                AdaptedWatchLine {
                    line: format!("{source_badge} reasoning {text}"),
                    is_turn_boundary: false,
                }
            }
            "stream_error" | "error" => AdaptedWatchLine {
                line: format!("{source_badge} stream.error {text}"),
                is_turn_boundary: true,
            },
            other => {
                self.unknown_events = self.unknown_events.saturating_add(1);
                AdaptedWatchLine {
                    line: format!("{source_badge} unknown.{other}"),
                    is_turn_boundary: false,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_core_lifecycle_sequence() {
        let mut adapter = CodexAdapter::new();
        let frames = [
            serde_json::json!({"source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},"event":{"params":{"type":"turn_started"}}}),
            serde_json::json!({"source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},"event":{"params":{"type":"item_started"}}}),
            serde_json::json!({"source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},"event":{"params":{"type":"item_delta","delta":"hello"}}}),
            serde_json::json!({"source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},"event":{"params":{"type":"item_completed"}}}),
            serde_json::json!({"source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},"event":{"params":{"type":"turn_completed"}}}),
            serde_json::json!({"source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},"event":{"params":{"type":"turn_idle"}}}),
        ];

        let out: Vec<String> = frames.iter().map(|f| adapter.adapt_frame(f).line).collect();

        assert!(out[0].contains("turn.started"));
        assert!(out[2].contains("item.delta"));
        assert!(out[4].contains("turn.completed"));
        assert!(out[5].contains("turn.idle"));
        assert_eq!(adapter.unknown_events(), 0);
    }

    #[test]
    fn increments_unknown_counter() {
        let mut adapter = CodexAdapter::new();
        let frame = serde_json::json!({
            "source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},
            "event":{"params":{"type":"future_new_kind"}}
        });
        let out = adapter.adapt_frame(&frame);
        assert!(out.line.contains("unknown.future_new_kind"));
        assert_eq!(adapter.unknown_events(), 1);
    }
}
