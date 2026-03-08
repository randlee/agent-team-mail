# Gemini Hook System (Implementation Reference)

## Scope

This document describes Gemini CLI hooks as implemented today in `gemini-cli`,
independent of ATM integration strategy.

Primary sources reviewed:
- [index.md](../gemini-cli/docs/hooks/index.md)
- [reference.md](../gemini-cli/docs/hooks/reference.md)
- [hooks/](../gemini-cli/packages/core/src/hooks/)
- [core/](../gemini-cli/packages/core/src/core/)
- [gemini.tsx](../gemini-cli/packages/cli/src/gemini.tsx)
- [AppContainer.tsx](../gemini-cli/packages/cli/src/ui/AppContainer.tsx)

## Hook Architecture

Gemini hook flow is:

1. `HookRegistry`
2. `HookPlanner`
3. `HookRunner`
4. `HookAggregator`
5. `HookEventHandler` / `HookSystem`

Key behavior:
- Hooks are enabled only when `hooksConfig.enabled = true`.
- Project hooks are blocked in untrusted folders.
- Matching hooks are deduplicated (`name:command` key).
- If any matching definition has `sequential: true`, all matching hooks for that
  event run sequentially; otherwise parallel.

## Full Hook Event List

| Event | Trigger Point | Input Extras | Effective Control |
| --- | --- | --- | --- |
| `SessionStart` | app startup, resume, `/clear` | `source` | Context/system message injection; startup not treated as a hard block path |
| `SessionEnd` | cleanup exit, `/clear` | `reason` | Observability/cleanup; best-effort on abnormal termination |
| `BeforeAgent` | before turn planning | `prompt` | Can stop turn, block turn, or inject additional context |
| `AfterAgent` | end of turn | `prompt`, `prompt_response`, `stop_hook_active` | Can stop or block and trigger retry loop; supports `clearContext` |
| `BeforeModel` | before LLM request | `llm_request` | Can stop/block request, rewrite request, or synthesize response |
| `AfterModel` | per streamed response chunk | `llm_request`, `llm_response` | Can stop/block chunk flow or replace chunk |
| `BeforeToolSelection` | before tool decision | `llm_request` | Tool filtering only (`toolConfig` union semantics) |
| `BeforeTool` | before tool execution | `tool_name`, `tool_input`, optional MCP context | Can stop/block tool call and rewrite tool args |
| `AfterTool` | after tool execution | tool request + tool response | Can stop/block returned result, append context, request tail tool call |
| `PreCompress` | before compression attempt | `trigger` | Advisory/telemetry |
| `Notification` | tool-confirmation notification path | `notification_type`, `message`, `details` | Advisory/telemetry |

## Capability Matrix (Injection vs Blocking)

| Event | Context Injection / Rewrite | Guard-Rail / Blocking |
| --- | --- | --- |
| `SessionStart` | Yes (`additionalContext`, `systemMessage`) | No hard block path |
| `SessionEnd` | No | No (best-effort cleanup signal) |
| `BeforeAgent` | Yes (`additionalContext`) | Yes (`decision: deny`, `continue: false`) |
| `AfterAgent` | Yes (`clearContext`) | Yes (deny/retry or stop execution) |
| `BeforeModel` | Yes (`llm_request` rewrite, synthetic `llm_response`) | Yes (block/stop model request) |
| `AfterModel` | Yes (`llm_response` chunk replacement) | Yes (block/stop chunk flow) |
| `BeforeToolSelection` | Yes (`toolConfig` filtering) | Indirect only (tool availability filter; no direct stop/deny) |
| `BeforeTool` | Yes (`tool_input` rewrite) | Yes (block/stop tool execution) |
| `AfterTool` | Yes (`additionalContext`, tail tool call request) | Yes (block/stop result path) |
| `PreCompress` | No | No (advisory only) |
| `Notification` | No | No (advisory only) |

## Where Hooks Are Fired

- Session lifecycle:
  - `packages/cli/src/gemini.tsx`
  - `packages/cli/src/ui/AppContainer.tsx`
  - `packages/cli/src/ui/commands/clearCommand.ts`
- Turn lifecycle:
  - `packages/core/src/core/client.ts` (`BeforeAgent` / `AfterAgent`)
- Model lifecycle:
  - `packages/core/src/core/geminiChat.ts` (`BeforeModel`, `AfterModel`, `BeforeToolSelection`)
- Tool lifecycle:
  - `packages/core/src/core/coreToolHookTriggers.ts` (`BeforeTool`, `AfterTool`)
- Compression:
  - `packages/core/src/services/chatCompressionService.ts` (`PreCompress`)

## IO, Exit Codes, and Parsing

Command hooks run as subprocesses with:
- JSON input via `stdin`
- output read from `stdout` (fallback parse from `stderr` when `stdout` empty)
- timeout default `60000ms`

Current plain-text fallback behavior:
- exit `0` -> `decision: allow` + `systemMessage`
- exit `1` -> non-blocking warning (`decision: allow`)
- other non-zero (including `2`) -> `decision: deny` + `reason`

Environment:
- sanitized parent environment
- sets `GEMINI_PROJECT_DIR` and compatibility alias `CLAUDE_PROJECT_DIR`
- supports command string expansion for those vars

## Merge Semantics Across Multiple Hooks

`HookAggregator` behavior is event-specific:

- OR-style decision merge:
  - `BeforeTool`, `AfterTool`, `BeforeAgent`, `AfterAgent`, `SessionStart`
  - any blocking decision can block; reasons/messages concatenate
- field replacement (last writer wins):
  - `BeforeModel`, `AfterModel`
- tool-selection union:
  - `BeforeToolSelection`
  - `NONE` mode dominates, else `ANY`, else `AUTO`
  - function allowlists are unioned + sorted
- simple merge:
  - remaining event types

## Matching and Ordering Rules

- Tool events matcher: regex (invalid regex falls back to exact string match).
- Lifecycle matcher: exact trigger/source match.
- Empty matcher and `*` match all.
- Duplicate hook configs are collapsed by `name:command`.

## Trust and Safety Model

- Project hooks are disabled in untrusted folders.
- Gemini tracks trusted project hooks in:
  - `${globalGeminiDir}/trusted_hooks.json`
- Hook trust check warns when hook signature changes for a workspace.

## Current Limits / Implementation Notes

- `NotificationType` is currently `ToolPermission`.
- `AfterModel` is chunk-level and may be high-frequency.
- `SessionEnd` is attempted in cleanup paths, but cannot be guaranteed on hard
  termination (e.g., force kill).

## ATM Strategy

Deliberately out of scope for this document.
ATM mapping/design should be defined in a separate Gemini adapter strategy doc.
