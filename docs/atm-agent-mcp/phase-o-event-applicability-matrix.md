# Phase O Event Applicability Matrix

Date: 2026-02-24  
Owner: `arch-ctm`  
Scope: attached CLI parity (`atm-agent-mcp attach <agent-id>`)

Purpose:
- prevent scope drift by declaring which Codex event classes are required for ATM parity vs intentionally out of scope,
- force explicit handling for non-required events (no silent drops).

Legend:
- `Required`: must render with parity semantics in Phase O.
- `Degraded`: intentionally simplified render allowed in Phase O (must be explicit and tested).
- `Out-of-Scope`: not implemented for Phase O attached UX; must surface as `unsupported.<type>` telemetry, not silently ignored.

## 1. Event-Class Decisions

| Event class | Status | Notes |
|---|---|---|
| User input (`UserMessage`) | Required | Includes distinct MCP client input styling. |
| Assistant output (`AgentMessage*`) | Required | Streaming + final content parity baseline. |
| Reasoning (`AgentReasoning*`, `Reasoning*Delta`) | Required | Section-break fidelity can be degraded if explicitly documented. |
| Turn lifecycle (`TurnStarted`, `TurnComplete`, `TurnAborted`, `StreamError`) | Required | Explicit fault state surfacing required. |
| Command execution (`ExecCommand*`, `TerminalInteraction`) | Required | Include begin/output/end + stdin interaction timeline. |
| Approval and review (`ExecApprovalRequest`, `ApplyPatchApprovalRequest`, `EnteredReviewMode`, `ExitedReviewMode`) | Required | Full control-path parity target. |
| `RequestUserInput` / `ElicitationRequest` | Required | Must not collapse into generic text lines. |
| File edit / patch (`PatchApply*`, `TurnDiff`, `file_change`) | Required | Red/green diff parity is in-scope for O.2/O.3. |
| Source attribution (`client_prompt`, `atm_mail`, `user_steer`) | Required | `atm_mail` summary format and 3-line clamp required. |
| Tool lifecycle (`McpToolCall*`, `WebSearch*`, dynamic tool call notices) | Degraded | Simplified but structured rows acceptable in O.2. |
| Session/meta (`SessionConfigured`, `ThreadNameUpdated`, `TokenCount`, `ModelReroute`, `ContextCompacted`, `ThreadRolledBack`, `Undo*`) | Degraded | Status-region parity first; full transcript parity can defer. |
| Background/warnings/deprecations (`BackgroundEvent`, `Warning`, `Error`, `DeprecationNotice`) | Degraded | Category-specific rows; no silent suppression. |
| Skills/list responses (`McpListToolsResponse`, `List*`, `RemoteSkillDownloaded`, `SkillsUpdateAvailable`) | Out-of-Scope | Non-core attached turn UX for Phase O. |
| Realtime conversation (`RealtimeConversation*`) | Out-of-Scope | Keep telemetry + unsupported marker. |
| Collaboration (`Collab*`) | Out-of-Scope | Defer until multi-agent attached UX phase. |
| Unknown/future event types | Required (handling) | Must increment counters + render `unknown.<type>` fallback. |

## 2. Non-Required Event Rules

For `Degraded` and `Out-of-Scope` classes:
1. Keep ordering intact in stream.
2. Emit a visible placeholder row or structured telemetry record.
3. Count occurrences per event type.
4. Add entry to deviation log if behavior differs from Codex baseline.

## 3. Sprint Gating

- **O.1 gate**: Matrix approved and referenced by requirements + parity test plan.
- **O.2 gate**: All `Required` render classes implemented with fixtures.
- **O.3 gate**: `Degraded`/`Out-of-Scope` classes have explicit fallback behavior, counters, and deviation-log entries; no silent drops.
