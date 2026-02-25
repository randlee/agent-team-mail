# Codex CLI vs ATM TUI Render Gap Analysis (Attached Mode)

Date: 2026-02-25  
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

Coverage note:
- Codex protocol currently exposes 70+ `EventMsg` variants. The matrix above groups events by rendered data class (not one-row-per-variant). Long-tail categories (realtime, collab, skills/list/meta responses) must still be explicitly tracked in fixture coverage before parity sign-off.

## 2.1 Renderer Complexity Assessment (Critical)

The parity effort is constrained by Codex renderer architecture, not only event mapping:

- Codex TUI is a full component/layout system (Renderable traits, column/row/flex/inset composition), not a thin formatter wrapper.
- High-complexity files indicate subsystem scope:
  - `chatwidget.rs` (~305KB)
  - `diff_render.rs` (~53KB)
  - `exec_cell` module (~34KB)
  - `approval_overlay` module (~28KB)
  - `markdown_render.rs` (~25KB)
- `diff_render` is a major subsystem (hunks, wrapping, syntax/highlight behavior, navigation-oriented structure), not a simple red/green text transform.
- Approval UX is also a subsystem (keyboard navigation, multi-option state machine, modal lifecycle), not just one request/response event.

Planning implication:
- O.2 should be treated as the primary implementation-risk sprint due to renderer/layout integration complexity.
- O.3 should absorb golden/hardening closure using the existing M.7 harness, rather than introducing a separate hardening sprint.

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

## 5. Phase O Work Breakdown Alignment

- `O.1`: attach stream/control wiring + typed render-event envelope.
- `O.2`: renderer parity expansion (including diff/tool lifecycle and layout-aligned presentation); highest complexity/risk sprint.
- `O.3`: control-path parity (`approval`, `request_user_input`, `interrupt/cancel`, fault state fidelity) plus golden fixture completion and hardening closure via the existing M.7 parity harness.

## 6. Current-State Audit (2026-02-25)

The following four gaps remain as explicit follow-up planning items for attached mode:

### Gap 1: Structured attach renderer not implemented (generic output path still dominant)

- Current behavior: most events are rendered via one generic formatter as `[class][source_kind] <text-or-event-type>`.
- Evidence:
  - `crates/atm-agent-mcp/src/commands/attach.rs:403` (`print_frame` entrypoint)
  - `crates/atm-agent-mcp/src/commands/attach.rs:415` (generic `println!` path)
  - `crates/atm-agent-mcp/src/commands/attach.rs:410` (only `input.atm_mail` special-cased)
- Impact: required classes do not get Codex-like structured presentation.
- Planned remediation sprint: `O-R.1` (size `M`).

### Gap 2: Required classes are classified but flattened (no dedicated render paths)

- Current behavior: classification maps required classes (`approval`, `elicitation.request`, `tool.exec`, `turn.lifecycle`, `file.edit`) but output still flows through shared generic formatter.
- Evidence:
  - `crates/atm-agent-mcp/src/commands/attach.rs:501` (`classify_event_class`)
  - `crates/atm-agent-mcp/src/commands/attach.rs:509` to `:533` (required-class mapping)
  - `crates/atm-agent-mcp/src/commands/attach.rs:415` (shared generic rendering sink)
- Impact: class-specific semantics are collapsed; parity assertions cannot validate per-class UX.
- Planned remediation sprint: `O-R.2` (size `M`).

### Gap 3: File-edit diff rendering parity missing in attach path

- Current behavior: file-edit events are mapped (`patch_apply_*`, `turn_diff`, `file_change`) but not rendered with red/green diff semantics.
- Evidence:
  - `crates/atm-agent-mcp/src/commands/attach.rs:529` (`file.edit` classification)
  - `crates/atm-agent-mcp/src/commands/attach.rs:415` (no diff-specific renderer in output path)
- Impact: FR-23.9 parity expectation is only partially satisfied in attached CLI UX.
- Planned remediation sprint: `O-R.3` (size `L`).

### Gap 4: Applicability contract drift (fixture expects field, envelope omits it)

- Current behavior: attach envelope schema does not include `applicability`.
- Evidence:
  - `crates/atm-agent-mcp/src/commands/attach.rs:45` to `:59` (`AttachedRenderEnvelope` fields)
  - `crates/atm-agent-mcp/tests/fixtures/parity/attach/class-map.expected.jsonl:1` to `:10` (fixture expects `applicability`)
- Impact: required/degraded/out_of_scope policy is not represented in attached JSON envelope contract.
- Planned remediation sprint: `O-R.4` (size `S`).

## 7. Remediation Summary

| Sprint | Size | GAP IDs | Deliverable focus |
|---|---|---|---|
| O-R.1 | M | GAP-008, GAP-015 | Structured renderer foundation + applicability contract alignment |
| O-R.2 | L | GAP-003, GAP-004 | Required event coverage expansion + unflattened class rendering |
| O-R.3 | L | GAP-002, GAP-005 | Approval/elicitation interaction parity + correlated response routing |
| O-R.4 | L | GAP-001, GAP-006, GAP-012 | Diff + reasoning + markdown parity hardening |
| O-R.5 | M | GAP-009, GAP-010, GAP-011, GAP-013, GAP-014 | Error/replay/telemetry/session hardening closure |

## 8. GAP-ID Verification Matrix (2026-02-25)

Verification policy for this update:
- `Confirmed`: directly verified in local implementation (`crates/atm-agent-mcp/src/commands/attach.rs` and related parity fixtures).
- `Not confirmed`: insufficient direct evidence in this worktree; excluded from new FR/sprint commitments until confirmed.

| GAP ID | Status | Verification summary | Primary evidence |
|---|---|---|---|
| GAP-001 | Confirmed | No diff renderer in attached output path; file-edit class is mapped but printed through generic formatter. | `attach.rs:415`, `attach.rs:529` |
| GAP-002 | Confirmed | No interactive approval modal path; approval/reject are CLI commands routed via stdin control. | `attach.rs:175`, `attach.rs:231`, `attach.rs:246` |
| GAP-003 | Confirmed | Required families like `mcp_tool_call_*`, `plan_*`, `session_configured`, `token_count`, `exec_command_begin` are not classified. | `attach.rs:509-540` |
| GAP-004 | Confirmed | `request_user_input` and `elicitation_request` are collapsed to one class; approval subtypes are flattened into one class. | `attach.rs:517-530` |
| GAP-005 | Confirmed | Approval commands (`:approve`, `:reject`) use `send_stdin_control` (stdin action), not a dedicated correlated elicitation response path in this command. | `attach.rs:175`, `attach.rs:139-149`, `attach.rs:246-261` |
| GAP-006 | Confirmed | Reasoning section-break-specific handling is absent; only delta/content reasoning kinds are mapped. | `attach.rs:514-516` |
| GAP-007 | Not confirmed | Potential end-to-end source-attribution loss was not reproducible from local attach implementation alone. | `attach.rs:429-443`, `attach.rs:490-493` |
| GAP-008 | Confirmed | Attached renderer uses a thin formatter with one generic `println!` path for most classes. | `attach.rs:403-425` |
| GAP-009 | Confirmed | Error output lacks explicit source class (`proxy`/`child`/`upstream`) in emitted payload/human line. | `attach.rs:575-588` |
| GAP-010 | Confirmed | Replay is frame-count bounded only; no turn-boundary awareness or truncation warning emission. | `attach.rs:378-392` |
| GAP-011 | Confirmed | Unsupported counts are tracked, but no detach/session-end summary emission path exists. | `attach.rs:560-566`, `attach.rs:162` |
| GAP-012 | Confirmed | Attached output path does not include markdown-aware rendering stage; raw text/event display is used. | `attach.rs:415-423` |
| GAP-013 | Confirmed | User input is trimmed/parsed but not sanitized before forwarding as stdin payload. | `attach.rs:195-216`, `attach.rs:246-261` |
| GAP-014 | Confirmed | Input help text does not document `Ctrl-C` behavior. | `attach.rs:171-179` |
| GAP-015 | Confirmed | Re-attach uses tail replay scan only; no persisted checkpoint marker in attach command path. | `attach.rs:356-393` |
