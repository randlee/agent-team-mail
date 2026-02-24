# Codex CLI vs ATM TUI Render Gap Analysis (Phase N Planning)

Date: 2026-02-24  
Owner: `arch-ctm`  
Scope: visual/rendered data parity for attached CLI mode (`atm-agent-mcp attach <agent-id>`) and current watch-mode baseline

## 1. Baseline Sources

- Codex protocol event surface: `codex-rs/protocol/src/protocol.rs` (`EventMsg` variants).
- Codex TUI render/handling surface: `codex-rs/tui/src/chatwidget.rs`.
- ATM watch event publish gate: `crates/atm-agent-mcp/src/proxy.rs` (`should_publish_watch_event`).
- ATM normalization/render path: `crates/atm-tui/src/codex_adapter.rs` -> `crates/atm-tui/src/codex_watch.rs`.

## 2. Type Mapping (Rendered Data)

Legend:
- `Covered`: ATM already renders equivalent semantics.
- `Partial`: ATM renders a reduced/collapsed form.
- `Gap`: not rendered with Codex-equivalent semantics.

| Render data type | Codex event surface (examples) | ATM watch/adapter status | Gap assessment |
|---|---|---|---|
| User input messages | `UserMessage` | Partial: source badge only; no dedicated input row semantics | Gap: no explicit "client input" rendering contract |
| Assistant text output | `AgentMessage`, `AgentMessageDelta`, `AgentMessageContentDelta` | Covered/Partial: mapped to `item.delta` text line | Gap: content-part structure and richer cell semantics collapsed |
| Reasoning output | `AgentReasoning*`, `ReasoningContentDelta`, `ReasoningRawContentDelta`, section breaks | Partial: mapped to `reasoning` text | Gap: no section-break/structured reasoning presentation |
| Turn lifecycle | `TurnStarted`, `TurnComplete`, `TurnAborted`, `StreamError`, `ShutdownComplete` | Covered for start/complete/interrupt/error; partial for aborted/shutdown nuance | Gap: abort/fatal distinctions are flattened |
| Command execution stream | `ExecCommandBegin`, `ExecCommandOutputDelta`, `TerminalInteraction`, `ExecCommandEnd` | Partial: output/end/error only (`cmd`) | Gap: begin, stdin/terminal interaction timeline, richer status missing |
| Approval/review flows | `ExecApprovalRequest`, `ApplyPatchApprovalRequest`, `EnteredReviewMode`, `ExitedReviewMode`, `RequestUserInput`, `ElicitationRequest` | Partial: request/reject/resolve mapped | Gap: no distinct UI for request-user-input / elicitation prompts |
| Tool call lifecycle | `McpToolCallBegin/End`, `WebSearchBegin/End`, `DynamicToolCallRequest`, `ViewImageToolCall` | Gap | Missing explicit tool begin/end render types |
| File edit / patch lifecycle | `PatchApplyBegin`, `PatchApplyEnd`, `TurnDiff`; cli-json `file_change` | Gap | No red/green diff rendering parity in ATM watch/attach path |
| Plan updates | `PlanUpdate`, `PlanDelta` | Gap | Plan-mode updates are not rendered as first-class events |
| Session/meta updates | `SessionConfigured`, `ThreadNameUpdated`, `TokenCount`, `ModelReroute`, `ContextCompacted`, `ThreadRolledBack`, `Undo*` | Gap/Partial | Status metrics partially surfaced; transcript-level parity missing |
| Background/deprecation/warnings | `BackgroundEvent`, `DeprecationNotice`, `Warning`, `Error` | Partial | Errors/warnings flattened; category-specific styling missing |
| Realtime conversation events | `RealtimeConversation*` | Gap | No direct equivalent rendering in ATM |
| Collaboration events | `Collab*` begin/end events | Gap | No multi-agent collab event rendering in ATM |
| Skills/list responses | `McpListToolsResponse`, `List*Skills*`, `RemoteSkillDownloaded`, `SkillsUpdateAvailable` | Gap | No parity surface in watch transcript |

## 3. Required ATM-Specific Input Types

These are required additions for attached CLI parity planning:

1. MCP client/user input (new ATM render type)
- Must render distinctly from assistant/tool output.
- Recommended format: explicit input row class (for example: `input.client <text>`), unique style token.

2. ATM mail input (new ATM render type)
- Must render as: `sender@team <short-message>`.
- `short-message` must be capped at 3 lines max (truncate with ellipsis on overflow).
- Source should remain attributable as `source.kind = atm_mail`.

## 4. Recommendations

1. Add an explicit render-event normalization layer for attached mode that maps Codex `EventMsg` classes to stable ATM render classes (instead of string-prefix-only mapping).
2. Extend adapter coverage beyond current MVP subset to include tool begin/end, diff/patch, request-user-input, and plan updates.
3. Reuse Codex diff renderer semantics for file edits (`TurnDiff`/`PatchApply*`) so red/green output is parity-accurate.
4. Add source-aware rendering rules:
- `client_prompt`/MCP client input: dedicated input styling.
- `atm_mail`: `sender@team <short-message>` with 3-line clamp.
- `user_steer`: dedicated local-steer styling distinct from both above.
5. Expand parity fixtures to include missing classes: `file_edit_diff`, `tool_begin_end`, `request_user_input`, `plan_update`, `atm_mail_summary`, `mcp_client_input`.

## 5. Phase N Work Breakdown Alignment

- `N.1`: attach stream/control wiring + typed render-event envelope.
- `N.2`: renderer parity expansion (including diff + tool lifecycle).
- `N.3`: control-path parity (`approval`, `request_user_input`, `interrupt/cancel`, fault state fidelity).
- `N.4`: golden parity matrix expansion and deviation enforcement for any remaining non-parity behavior.
