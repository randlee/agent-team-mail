//! Codex watch-stream adapter for ATM TUI.
//!
//! Maps ATM/MCP watch frames into normalized Codex-style render events while
//! preserving stream order and incremental updates.

use crate::codex_vendor::text_formatting::format_json_compact;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct AdaptedWatchLine {
    pub line: String,
    pub is_turn_boundary: bool,
}

#[derive(Debug, Default)]
pub struct CodexAdapter {
    unknown_events: u64,
    unknown_by_type: BTreeMap<String, u64>,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn unknown_events(&self) -> u64 {
        self.unknown_events
    }

    pub fn unknown_summary(&self, warn_threshold: u64) -> Vec<String> {
        let mut out = Vec::new();
        for (event_type, count) in &self.unknown_by_type {
            out.push(format!("unsupported.summary {event_type}={count}"));
            if *count >= warn_threshold {
                out.push(format!(
                    "stream.warning unsupported event '{event_type}' seen {count} times"
                ));
            }
        }
        out
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

        let raw_text = event
            .pointer("/params/delta")
            .and_then(|v| v.as_str())
            .or_else(|| event.pointer("/params/text").and_then(|v| v.as_str()))
            .or_else(|| event.pointer("/params/output").and_then(|v| v.as_str()))
            .or_else(|| event.pointer("/params/message").and_then(|v| v.as_str()))
            .or_else(|| event.pointer("/params/prompt").and_then(|v| v.as_str()))
            .unwrap_or("");
        let text = format_json_compact(raw_text).unwrap_or_else(|| raw_text.to_string());

        if kind == "user_message" && source_kind == "atm_mail" {
            return AdaptedWatchLine {
                line: format!(
                    "{source_badge} input.atm_mail {} <{}>",
                    format_mail_actor(source_actor),
                    clamp_three_lines(raw_text)
                ),
                is_turn_boundary: false,
            };
        }
        if kind == "user_message" && (source_kind == "user_steer" || source_kind == "tui_user") {
            return AdaptedWatchLine {
                line: format!("{source_badge} input.user_steer {text}"),
                is_turn_boundary: false,
            };
        }
        if kind == "user_message" {
            return AdaptedWatchLine {
                line: format!("{source_badge} input.client {text}"),
                is_turn_boundary: false,
            };
        }

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
                line: if let Some(status) = event.pointer("/params/status").and_then(|v| v.as_str())
                {
                    format!("{source_badge} turn.completed status={status}")
                } else {
                    format!("{source_badge} turn.completed")
                },
                is_turn_boundary: true,
            },
            "turn_aborted" => AdaptedWatchLine {
                line: format!("{source_badge} turn.completed status=failed"),
                is_turn_boundary: true,
            },
            "turn_idle" | "idle" => AdaptedWatchLine {
                line: format!("{source_badge} turn.idle"),
                is_turn_boundary: true,
            },
            "exec_command_begin" | "exec_command_started" => AdaptedWatchLine {
                line: format!("{source_badge} cmd.begin {text}"),
                is_turn_boundary: false,
            },
            "exec_command_output_delta" => AdaptedWatchLine {
                line: format!("{source_badge} cmd.output {text}"),
                is_turn_boundary: false,
            },
            "exec_command_completed" => AdaptedWatchLine {
                line: format!("{source_badge} cmd.completed {text}"),
                is_turn_boundary: false,
            },
            "exec_command_error" => AdaptedWatchLine {
                line: format!("{source_badge} cmd.error {text}"),
                is_turn_boundary: false,
            },
            "exec_approval_request" | "approval_prompt" | "approval_request" => AdaptedWatchLine {
                line: format!("{source_badge} approval.exec.request {text}"),
                is_turn_boundary: true,
            },
            "apply_patch_approval_request" => AdaptedWatchLine {
                line: format!("{source_badge} approval.patch.request {text}"),
                is_turn_boundary: true,
            },
            "entered_review_mode" | "item/enteredReviewMode" => AdaptedWatchLine {
                line: format!("{source_badge} approval.review.entered {text}"),
                is_turn_boundary: true,
            },
            "exited_review_mode" | "item/exitedReviewMode" => AdaptedWatchLine {
                line: format!("{source_badge} approval.review.exited {text}"),
                is_turn_boundary: true,
            },
            "patch_apply_begin" | "patch_apply_end" | "turn_diff" | "file_change" => {
                AdaptedWatchLine {
                    line: format!("{source_badge} file.edit {kind} {text}"),
                    is_turn_boundary: false,
                }
            }
            "approval_rejected" | "reject" | "rejected" => AdaptedWatchLine {
                line: format!("{source_badge} approval.review.rejected {text}"),
                is_turn_boundary: true,
            },
            "approval_approved" | "approved" => AdaptedWatchLine {
                line: format!("{source_badge} approval.review.resolved {text}"),
                is_turn_boundary: true,
            },
            "request_user_input" | "elicitation_request" => AdaptedWatchLine {
                line: format!("{source_badge} elicitation.request {text}"),
                is_turn_boundary: true,
            },
            "mcp_tool_call_begin" => AdaptedWatchLine {
                line: format!("{source_badge} tool.mcp.begin {text}"),
                is_turn_boundary: false,
            },
            "mcp_tool_call_end" => AdaptedWatchLine {
                line: format!("{source_badge} tool.mcp.end {text}"),
                is_turn_boundary: false,
            },
            "web_search_begin" => AdaptedWatchLine {
                line: format!("{source_badge} tool.web_search.begin {text}"),
                is_turn_boundary: false,
            },
            "web_search_end" => AdaptedWatchLine {
                line: format!("{source_badge} tool.web_search.end {text}"),
                is_turn_boundary: false,
            },
            "plan_update" => AdaptedWatchLine {
                line: format!("{source_badge} plan.update {text}"),
                is_turn_boundary: false,
            },
            "plan_delta" => AdaptedWatchLine {
                line: format!("{source_badge} plan.delta {text}"),
                is_turn_boundary: false,
            },
            "session_configured" => AdaptedWatchLine {
                line: format!("{source_badge} session.configured {text}"),
                is_turn_boundary: true,
            },
            "token_count" => AdaptedWatchLine {
                line: format!("{source_badge} session.token_count {text}"),
                is_turn_boundary: false,
            },
            "reasoning_content_delta" | "agent_reasoning_delta" | "reasoning_content" => {
                let normalized = if is_reasoning_section_break(event) {
                    "reasoning.section_break"
                } else {
                    "reasoning"
                };
                AdaptedWatchLine {
                    line: format!("{source_badge} {normalized} {text}"),
                    is_turn_boundary: false,
                }
            }
            "turn_interrupted" | "interrupt" | "cancelled" | "turn_cancelled" => AdaptedWatchLine {
                line: format!("{source_badge} turn.interrupted {text}"),
                is_turn_boundary: true,
            },
            "stream_error" | "error" => {
                let fatal = is_fatal_error(event, &text);
                let rendered = if fatal {
                    format!("{text} [detach/reconnect recommended]")
                } else {
                    text.clone()
                };
                AdaptedWatchLine {
                    line: format!(
                        "{source_badge} stream.error.{}{} {rendered}",
                        error_source(event),
                        if fatal { ".fatal" } else { "" }
                    ),
                    is_turn_boundary: true,
                }
            }
            "stream_warning" => AdaptedWatchLine {
                line: format!("{source_badge} stream.warning {text}"),
                is_turn_boundary: false,
            },
            other => {
                self.unknown_events = self.unknown_events.saturating_add(1);
                let key = sanitize_event_type(other);
                let entry = self.unknown_by_type.entry(key.clone()).or_insert(0);
                *entry = entry.saturating_add(1);
                AdaptedWatchLine {
                    line: format!("{source_badge} unknown.{key}"),
                    is_turn_boundary: false,
                }
            }
        }
    }
}

fn clamp_three_lines(text: &str) -> String {
    let mut lines = text.lines();
    let l1 = lines.next().unwrap_or_default();
    let l2 = lines.next().unwrap_or_default();
    let l3 = lines.next().unwrap_or_default();
    let has_more = lines.next().is_some();

    let mut out: Vec<&str> = Vec::new();
    if !l1.is_empty() {
        out.push(l1);
    }
    if !l2.is_empty() {
        out.push(l2);
    }
    if !l3.is_empty() {
        out.push(l3);
    }
    let mut joined = out.join(" / ");
    if has_more {
        if !joined.is_empty() {
            joined.push_str(" ...");
        } else {
            joined = "...".to_string();
        }
    }
    joined
}

fn format_mail_actor(actor: &str) -> String {
    if actor.contains('@') {
        return actor.to_string();
    }
    let team = std::env::var("ATM_TEAM").unwrap_or_else(|_| "atm-dev".to_string());
    format!("{actor}@{}", team.trim())
}

fn sanitize_event_type(event_type: &str) -> String {
    if event_type.trim().is_empty() {
        return "unknown".to_string();
    }
    event_type
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn error_source(event: &serde_json::Value) -> &'static str {
    let src = event
        .pointer("/params/error_source")
        .and_then(|v| v.as_str())
        .or_else(|| event.pointer("/params/errorSource").and_then(|v| v.as_str()))
        .or_else(|| event.pointer("/params/source").and_then(|v| v.as_str()))
        .unwrap_or("proxy");
    match src {
        "child" => "child",
        "upstream" | "upstream_mcp" => "upstream",
        _ => "proxy",
    }
}

fn is_fatal_error(event: &serde_json::Value, text: &str) -> bool {
    event
        .pointer("/params/fatal")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || text.to_ascii_lowercase().contains("fatal")
}

fn is_reasoning_section_break(event: &serde_json::Value) -> bool {
    if event
        .pointer("/params/section_break")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    if event
        .pointer("/params/is_section_break")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    event
        .pointer("/params/delta_type")
        .and_then(|v| v.as_str())
        .or_else(|| {
            event
                .pointer("/params/reasoning_delta/type")
                .and_then(|v| v.as_str())
        })
        .or_else(|| event.pointer("/params/content/type").and_then(|v| v.as_str()))
        .is_some_and(|t| t.eq_ignore_ascii_case("section_break"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn adapter_fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parity/adapter")
    }

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

    #[test]
    fn maps_approval_and_rejection_events() {
        let mut adapter = CodexAdapter::new();
        let approval = serde_json::json!({
            "source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},
            "event":{"params":{"type":"approval_request","message":"allow command?"}}
        });
        let rejected = serde_json::json!({
            "source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},
            "event":{"params":{"type":"approval_rejected","message":"denied"}}
        });
        let resolved = serde_json::json!({
            "source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},
            "event":{"params":{"type":"approval_approved","message":"granted"}}
        });
        assert!(
            adapter
                .adapt_frame(&approval)
                .line
                .contains("approval.exec.request")
        );
        assert!(
            adapter
                .adapt_frame(&rejected)
                .line
                .contains("approval.review.rejected")
        );
        assert!(
            adapter
                .adapt_frame(&resolved)
                .line
                .contains("approval.review.resolved")
        );
        assert_eq!(adapter.unknown_events(), 0);
    }

    #[test]
    fn maps_new_required_and_degraded_event_families() {
        let mut adapter = CodexAdapter::new();
        let cases = [
            ("exec_command_begin", "cmd.begin"),
            ("mcp_tool_call_begin", "tool.mcp.begin"),
            ("mcp_tool_call_end", "tool.mcp.end"),
            ("web_search_begin", "tool.web_search.begin"),
            ("web_search_end", "tool.web_search.end"),
            ("plan_update", "plan.update"),
            ("plan_delta", "plan.delta"),
            ("session_configured", "session.configured"),
            ("token_count", "session.token_count"),
            ("request_user_input", "elicitation.request"),
            ("exec_approval_request", "approval.exec.request"),
            ("apply_patch_approval_request", "approval.patch.request"),
            ("entered_review_mode", "approval.review.entered"),
            ("exited_review_mode", "approval.review.exited"),
        ];

        for (event_type, expected) in cases {
            let frame = serde_json::json!({
                "source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},
                "event":{"params":{"type":event_type,"message":"m"}}
            });
            let line = adapter.adapt_frame(&frame).line;
            assert!(
                line.contains(expected),
                "event {event_type} expected mapping {expected}, got {line}"
            );
        }
        assert_eq!(adapter.unknown_events(), 0);
    }

    #[test]
    fn maps_interrupt_and_cancel_events() {
        let mut adapter = CodexAdapter::new();
        let interrupted = serde_json::json!({
            "source":{"kind":"user_steer","actor":"randlee","channel":"tui_user"},
            "event":{"params":{"type":"turn_interrupted","message":"cancelled by user"}}
        });
        let line = adapter.adapt_frame(&interrupted).line;
        assert!(line.contains("turn.interrupted"));
        assert_eq!(adapter.unknown_events(), 0);
    }

    #[test]
    fn maps_stream_error_source_and_fatal() {
        let mut adapter = CodexAdapter::new();
        let frame = serde_json::json!({
            "source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},
            "event":{"params":{"type":"stream_error","error_source":"child","fatal":true,"message":"boom"}}
        });
        let out = adapter.adapt_frame(&frame);
        assert!(out.line.contains("stream.error.child.fatal"));
        assert!(out.line.contains("detach/reconnect recommended"));
    }

    #[test]
    fn maps_file_edit_events() {
        let mut adapter = CodexAdapter::new();
        let frame = serde_json::json!({
            "source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},
            "event":{"params":{"type":"patch_apply_begin","message":"patch starts"}}
        });
        let out = adapter.adapt_frame(&frame);
        assert!(out.line.contains("file.edit patch_apply_begin"));
        assert_eq!(adapter.unknown_events(), 0);
    }

    #[test]
    fn maps_reasoning_section_break() {
        let mut adapter = CodexAdapter::new();
        let frame = serde_json::json!({
            "source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},
            "event":{"params":{"type":"reasoning_content_delta","delta_type":"section_break","delta":"analysis"}}
        });
        let out = adapter.adapt_frame(&frame);
        assert!(out.line.contains("reasoning.section_break"));
        assert_eq!(adapter.unknown_events(), 0);
    }

    #[test]
    fn maps_user_input_sources() {
        let mut adapter = CodexAdapter::new();
        let client = serde_json::json!({
            "source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},
            "event":{"params":{"type":"user_message","text":"hi from client"}}
        });
        let steer = serde_json::json!({
            "source":{"kind":"user_steer","actor":"randlee","channel":"tui_user"},
            "event":{"params":{"type":"user_message","text":"cancel this"}}
        });
        let mail = serde_json::json!({
            "source":{"kind":"atm_mail","actor":"arch-atm","channel":"mail_injector"},
            "event":{"params":{"type":"user_message","text":"line1\nline2\nline3\nline4"}}
        });
        assert!(adapter.adapt_frame(&client).line.contains("input.client"));
        assert!(
            adapter
                .adapt_frame(&steer)
                .line
                .contains("input.user_steer")
        );
        assert!(
            adapter
                .adapt_frame(&mail)
                .line
                .contains("input.atm_mail arch-atm@")
        );
        assert!(
            adapter
                .adapt_frame(&mail)
                .line
                .contains("<line1 / line2 / line3 ...>")
        );
    }

    #[test]
    fn unknown_summary_contains_threshold_warning() {
        let mut adapter = CodexAdapter::new();
        let frame = serde_json::json!({
            "source":{"kind":"client_prompt","actor":"arch-atm","channel":"mcp_primary"},
            "event":{"params":{"type":"future/event"}}
        });
        for _ in 0..5 {
            let _ = adapter.adapt_frame(&frame);
        }
        let summary = adapter.unknown_summary(5);
        assert!(summary.iter().any(|l| l.contains("unsupported.summary")));
        assert!(summary.iter().any(|l| l.contains("stream.warning")));
    }

    #[test]
    fn parity_adapter_fixture_golden_scenarios() {
        let base = adapter_fixture_dir();
        let scenarios = [
            "prompt-basic",
            "tool-stream",
            "approval-flow",
            "error-flow",
            "cancel-flow",
            "multi-item",
            "fatal-error",
            "unknown-event",
            "atm-mail",
            "user-steer",
            "session-attach",
            "detach-reattach",
            "cross-transport",
        ];

        for scenario in scenarios {
            let input_path = base.join(scenario).join("input.events.jsonl");
            let expected_path = base.join(scenario).join("expected.normalized.jsonl");

            let input_raw = fs::read_to_string(&input_path).expect("input fixture");
            let expected_raw = fs::read_to_string(&expected_path).expect("expected fixture");

            let mut adapter = CodexAdapter::new();
            let actual: Vec<serde_json::Value> = input_raw
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("valid input frame"))
                .map(|frame| {
                    let out = adapter.adapt_frame(&frame);
                    serde_json::json!({
                        "line": out.line,
                        "is_turn_boundary": out.is_turn_boundary
                    })
                })
                .collect();

            let expected: Vec<serde_json::Value> = expected_raw
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("valid expected line"))
                .collect();

            assert_eq!(
                actual, expected,
                "adapter parity mismatch in scenario {scenario}"
            );
        }
    }
}
