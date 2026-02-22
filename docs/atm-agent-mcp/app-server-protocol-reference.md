# Codex App-Server Protocol Reference (for `atm-agent-mcp`)

> Version anchor: Codex source snapshot used by this document (2026-02-22)
> - `codex-rs/app-server-protocol/src/protocol/common.rs`
> - `codex-rs/app-server-protocol/src/protocol/v2.rs`
> - `codex-rs/app-server/src/codex_message_processor.rs`
> - `codex-rs/app-server/src/transport.rs`

This is a protocol-accurate reference for implementing `transport = "app-server"` in `atm-agent-mcp`.
For mode selection and high-level tradeoffs, see `docs/atm-agent-mcp/codex-execution-modes.md`.
For `transport = "cli-json"`, see `docs/codex-json-schema.md`.

## 1. Wire Protocol

- Transport is newline-delimited JSON over stdio (one JSON message per line).
- Protocol shape is JSON-RPC-like request/response/notification.
- Messages omit the `jsonrpc` field.
- Requests include `id`; notifications do not.

Request example:

```json
{"id":1,"method":"thread/start","params":{"model":"gpt-5-codex"}}
```

Response example:

```json
{"id":1,"result":{"thread":{"id":"..."}}}
```

Notification example:

```json
{"method":"turn/completed","params":{"turnId":"...","status":"completed"}}
```

## 2. Initialization and Session Lifecycle

Expected startup flow:

1. Client sends `initialize`.
2. Client sends `initialized` notification.
3. Client starts or resumes a thread (`thread/start` or `thread/resume`).
4. Client starts a turn (`turn/start`).
5. Server streams `item/*` and `turn/*` notifications.
6. Server emits `turn/completed` terminal event for the turn.

The app server auto-subscribes the connection to thread notifications when a thread is created/resumed in the message processor.

## 3. Core Methods Required by ATM

Thread methods:

- `thread/start`
  - Params: `model`, optional `cwd`, optional `baseInstructions`, optional `approvalPolicy`, optional `sandboxPolicy`, optional profile/config fields.
  - Returns `ThreadStartResponse` with `thread`.
- `thread/resume`
  - Params: `threadId`.
  - Returns resumed `thread`.
- `thread/fork`
  - Params: `threadId`, optional `turnId`.
  - Returns new forked `thread`.
- `thread/list`
  - Params: pagination (`before`, `limit`), optional filters.
- `thread/read`
  - Params: `threadId`.

Turn methods:

- `turn/start`
  - Params: `threadId`, `input` (`Vec<InputItem>`), optional cwd/options.
  - Returns initial `turn` metadata.
- `turn/steer`
  - Params: `threadId`, `expectedTurnId`, `input`.
  - Used to inject additional user input into an in-flight turn.
- `turn/interrupt`
  - Params: `threadId`, optional reason/context.

Direct command method:

- `command/exec`
  - Params include `threadId`, command string/argv, cwd/env and policy-related options.

## 4. `turn/steer` Contract (Critical)

`codex_message_processor` enforces:

- `expectedTurnId` must be non-empty.
- Steer only succeeds with an active turn.
- Turn ID mismatch is rejected.
- Empty steer input is rejected.

Error mapping is returned as invalid request errors at the app-server boundary.
ATM implications:

- `atm-agent-mcp` must track the currently active `turnId` per managed thread for steer calls.
- For queued mail/interrupt interactions, avoid stale `expectedTurnId` by refreshing on `turn/started` and clearing on `turn/completed`.

## 5. Server Notifications

Primary turn notifications:

- `turn/started`
- `turn/completed`

Primary item lifecycle notifications:

- `item/started`
- `item/completed`

Common delta/progress notifications used for streaming UI and logs:

- `item/agentMessage/delta`
- `item/plan/delta`
- `item/commandExecution/outputDelta`
- `item/fileChange/outputDelta`
- `item/mcpToolCall/progress`
- `item/reasoning/summaryTextDelta`
- `item/reasoning/summaryPartAdded`
- `item/reasoning/textDelta`

## 6. `ThreadItem` Variants (v2)

`ThreadItem` includes (non-exhaustive for ATM use):

- `UserMessage`
- `AgentMessage`
- `Plan`
- `Reasoning`
- `CommandExecution`
- `FileChange`
- `McpToolCall`
- `CollabAgentToolCall`
- `WebSearch`
- `ImageView`
- `EnteredReviewMode`
- `ExitedReviewMode`
- `ContextCompaction`

These variants should be preserved in ATM logs/events rather than flattened, so future UI features can fan out richer state.

## 7. Turn Status State Machine

Terminal and non-terminal statuses exposed in protocol v2:

- `in_progress`
- `completed`
- `interrupted`
- `failed`

ATM should treat only `completed`, `interrupted`, and `failed` as turn-terminal for queue drain and lifecycle transitions.

## 8. Notification Subscription and Opt-Out

- App server supports notification suppression via `initialize.capabilities.optOutNotificationMethods`.
- ATM should not opt out of notifications needed for:
  - active turn tracking (`turn/started`, `turn/completed`),
  - streaming output (`item/*/delta`, `item/completed`),
  - lifecycle/health signaling.

## 9. Backpressure and Capacity

Transport channel capacity in current app-server transport is bounded (`CHANNEL_CAPACITY = 128`).
When saturated, app-server returns a server overload error (code `-32001`).

ATM implications:

- Treat overload as retryable with bounded backoff.
- Prefer coalescing high-frequency outbound control writes when possible.
- Keep mail injection queue bounded per thread to avoid burst amplification.

## 10. Config Read/Write Surface

App-server includes config endpoints (for example `config/read`, `config/value/write`) that can mutate Codex config state.
For ATM integration:

- Default policy should be read-only for config unless explicit ATM feature requires write.
- If writes are enabled, emit audit entries with key path and caller context.

## 11. Mapping to ATM Transport Abstraction

Recommended transport-neutral operations in `atm-agent-mcp`:

- `start_thread`
- `resume_thread`
- `start_turn`
- `steer_turn`
- `interrupt_turn`
- `stream_events` (turn/item notifications)

This keeps `mcp`, `cli-json`, and `app-server` behind one adapter boundary with identical queue/lifecycle semantics.

## 12. Open Gaps to Track in Phase Plan

- Socket-level integration tests validating full app-server framing and notification order.
- Approval/elicitation parity behavior between `mcp` and `app-server` transports.
- TUI fanout contract for app-server deltas (`udp`/pubsub normalization).

---

## Source Notes

This document is derived directly from Codex source types and handlers listed at the top.
If Codex protocol types change, update this reference before changing ATM transport behavior.
