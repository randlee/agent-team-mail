# TUI Control Protocol (Phase C Receiver Baseline + Phase D TUI Contract)

**Version**: 0.2  
**Date**: 2026-02-21  
**Status**: Draft

---

## 1. Scope and Compatibility Model

Defines message contracts for TUI control actions:

- `control.stdin.request`
- `control.stdin.ack`
- `control.interrupt.request`
- `control.interrupt.ack`

These contracts are for live worker-session control (Codex/Gemini).  
ATM mailbox delivery uses existing mail commands and is not redefined here.

Status:

- this protocol is a target contract for Phase C
- current daemon command surface does not yet expose `control.stdin.*` / `control.interrupt.*` handlers
- implementations must gate send paths on receiver capability discovery

Phase alignment:

- Phase C target is a lightweight receiver contract/stub (`C.3`): endpoint, validation, ack, dedupe skeleton
- full interactive TUI control flows are Phase D scope
- provider-native ids (such as Codex `threadId`) are MCP-internal details and must not appear in public TUI/control payloads

Compatibility note (important):

- the **receiver baseline implemented in C.3** accepts `command = "control"` payloads with:
  - `v`, `request_id`, `team`, `session_id`, `agent_id`, `sender`, `sent_at`
  - `action` (`stdin` or `interrupt`)
  - `payload` or `content_ref`
- the `type = "control.*"` contracts in this document remain the canonical UI/control contract model
- sender/receiver adapters may map between `type` form and current receiver `action` form until a single canonical wire shape is finalized
- provider-native ids (such as Codex `threadId`) are MCP-internal and MUST NOT appear in public TUI/control payloads

---

## 2. Envelope and Transport

Control messages are carried as payloads inside the existing daemon socket envelope.

Socket envelope (existing daemon API):

- `version` (integer): daemon socket protocol version
- `request_id` (string): socket request id
- `command` (string): daemon command
- `payload` (object): control message object described below

Control payload object fields:

- `type` (string): message type
- `v` (integer): schema version (current: `1`)
- `request_id` (string): stable idempotency key for the logical send
- `team` (string): team namespace
- `session_id` (string): Claude session id
- `agent_id` (string): canonical conversation id exposed outside MCP
- `sent_at` / `acked_at` (RFC 3339 UTC string)

Optional:

- `meta` (object): transport/UI metadata

Versioning rule:

- `version` applies to daemon socket framing
- `v` applies only to control payload schema

Transport rule:

- TUI must use daemon socket `command = "control"` for control actions
- ATM mailbox commands are out of scope for this protocol and must not be used as control fallback

---

## 3. Message Types

### 3.1 `control.stdin.request`

Required fields:

- `type = "control.stdin.request"`
- `v`
- `request_id`
- `team`
- `session_id`
- `agent_id`
- `sender`
- `sent_at`
- one of:
  - `content` (UTF-8 text)
  - `content_ref` (reference object for oversize input)

Optional fields:

- `content_encoding` (default: `utf-8`)
- `content_preview`
- `interrupt` (default: `false`)
- `meta.retry_count` (integer)
- `meta.ui_source` (example: `tui`)
- `meta.oversize_strategy` (example: `content_ref`)

Validation:

- multiline text is allowed and sent as one payload
- empty payload is rejected
- payload over hard limit must use `content_ref`

### 3.2 `control.stdin.ack`

Required fields:

- `type = "control.stdin.ack"`
- `v`
- `request_id`
- `team`
- `session_id`
- `agent_id`
- `acked_at`
- `result`
- `duplicate` (boolean)

Optional fields:

- `detail`
- `error`

### 3.3 `control.interrupt.request`

Required fields:

- `type = "control.interrupt.request"`
- `v`
- `request_id`
- `team`
- `session_id`
- `agent_id`
- `sender`
- `sent_at`
- `signal = "interrupt"`

Optional fields:

- `meta.retry_count`
- `meta.ui_source`

Implementation note (current state):

- interrupt contract is defined, but receiver execution path is not implemented yet in lifecycle queue
- until implemented, receiver should return explicit unsupported response (`rejected` or `not_live` with detail)

### 3.4 `control.interrupt.ack`

Required fields:

- `type = "control.interrupt.ack"`
- `v`
- `request_id`
- `team`
- `session_id`
- `agent_id`
- `acked_at`
- `result`
- `duplicate`

Optional fields:

- `detail`
- `error`

---

## 4. Result Codes

`result` enum:

- `ok`
- `not_live`
- `not_found`
- `busy`
- `timeout`
- `rejected`
- `internal_error`

Notes:

- `idle` is a state, not a result code.
- if target session cannot accept live control input, return `not_live`.

---

## 5. Idempotency and Retries

Rules:

- retries of a logical send must reuse the same `request_id`
- receiver deduplicates by `team + request_id + session_id + agent_id`
- duplicate delivery must not re-execute input injection
- duplicate request returns ack with:
  - `result = "ok"`
  - `duplicate = true`

Receiver dedupe store requirements:

- dedupe key: `(team, session_id, agent_id, request_id)`
- keep accepted keys for a bounded TTL window (recommended: 10 minutes)
- dedupe lookup/insert must be atomic per key
- receiver restart behavior must be explicit:
  - MVP allowed: in-memory dedupe only (duplicates possible after restart)
  - follow-up: durable dedupe store for restart-safe semantics

Default retry policy:

- ack timeout target: `2s`
- retries configurable
- recommended default: `1` retry with short backoff

Interrupt special case:

- unsupported interrupt paths must reject before dedupe slot consumption
- repeated unsupported interrupt requests with same `request_id` should not be marked duplicate unless actual execution semantics change

---

## 6. Size Limits and File-Reference Fallback

Defaults:

- soft limit: `64 KiB`
- hard limit: `1 MiB` (configurable)

Behavior:

- above soft limit: sender warns
- above hard limit: sender writes content to file and sends `content_ref`

`content_ref` object:

- `path` (absolute path)
- `size_bytes`
- `sha256`
- `mime`

Optional:

- `expires_at`

`content_ref` receiver constraints:

- path must resolve under an allowed local base directory
- canonicalized path escape (e.g. `..`) must be rejected
- symlink traversal outside allowed base must be rejected
- receiver must verify `size_bytes` and `sha256` before use
- expired references (`expires_at`) must be rejected

---

## 7. Logging and Audit

Both sender and receiver must emit event-log entries for:

- request emission
- ack received/sent
- retry attempts
- duplicate detection outcomes
- failures and timeouts

Message text logging:

- full text only when verbose message logging is enabled
- default mode should avoid full payload logging
- include `request_id` on all related events for correlation

Minimum audit fields for both request and ack events:

- `request_id`
- `team`
- `session_id`
- `agent_id`
- `sender` (if available)
- `result`
- `duplicate`

Operational logging note:

- compact event-log keys on disk are expected; UI should expand to full labels
- by default, message text is omitted
- verbose logging modes may include truncated/full payload text

---

## 8. Example Payloads

### 8.1 Inline stdin request

```json
{
  "type": "control.stdin.request",
  "v": 1,
  "request_id": "req_01HZY8QJ8R7G6K2YJ7V2M9A1P3",
  "session_id": "claude-session-uuid",
  "agent_id": "codex:abc123",
  "team": "atm-dev",
  "sender": "arch-ctm",
  "sent_at": "2026-02-20T21:15:00Z",
  "content": "single payload text",
  "content_encoding": "utf-8",
  "interrupt": false,
  "meta": {
    "ui_source": "tui",
    "retry_count": 0
  }
}
```

### 8.2 Stdin ack

```json
{
  "type": "control.stdin.ack",
  "v": 1,
  "request_id": "req_01HZY8QJ8R7G6K2YJ7V2M9A1P3",
  "session_id": "claude-session-uuid",
  "agent_id": "codex:abc123",
  "team": "atm-dev",
  "acked_at": "2026-02-20T21:15:00Z",
  "result": "ok",
  "detail": "accepted",
  "duplicate": false
}
```

### 8.3 Interrupt request

```json
{
  "type": "control.interrupt.request",
  "v": 1,
  "request_id": "req_01HZY8R2N2S5CXK3Y8E1B4M7T0",
  "session_id": "claude-session-uuid",
  "agent_id": "codex:abc123",
  "team": "atm-dev",
  "sender": "arch-ctm",
  "sent_at": "2026-02-20T21:16:12Z",
  "signal": "interrupt",
  "meta": {
    "ui_source": "tui",
    "retry_count": 0
  }
}
```

### 8.4 Oversize stdin request using `content_ref`

```json
{
  "type": "control.stdin.request",
  "v": 1,
  "request_id": "req_01HZY8S9Q6P4W8F2M1K7R3D5N9",
  "session_id": "claude-session-uuid",
  "agent_id": "codex:abc123",
  "team": "atm-dev",
  "sender": "arch-ctm",
  "sent_at": "2026-02-20T21:17:30Z",
  "content_ref": {
    "path": "/Users/randlee/.config/atm/share/atm-dev/input/req_01HZY8S9Q6P4W8F2M1K7R3D5N9.txt",
    "size_bytes": 143201,
    "sha256": "e3b0c44298fc1c149afbf4c8996fb924...",
    "mime": "text/plain; charset=utf-8"
  },
  "content_preview": "first 240 chars for diagnostics...",
  "interrupt": false,
  "meta": {
    "ui_source": "tui",
    "retry_count": 0,
    "oversize_strategy": "content_ref"
  }
}
```

---

## 9. Future Extensions (Non-MVP)

- policy-based agent control allow/deny lists
- explicit `retry_of` chain metadata for observability
- signed control messages for cross-host relays
- streaming partial-ack protocol for very large reference payload workflows
- canonical convergence of `type` and `action` payload forms

---

## 10. Security and AuthZ Baseline

Receiver must enforce:

- same-team authorization (`sender.team == target.team`)
- sender identity validity and membership
- deny-by-default for unknown sender or unknown target session
- explicit rejection (`result = "rejected"` with detail) for policy violations

Replay safety:

- requests older than a configurable max age should be rejected
- `sent_at` skew tolerance should be validated (recommended: bounded clock-skew window)

---

## 11. MVP vs Hardening Boundary (D vs E)

Phase D (MVP required):

- schema validation
- live-state gating
- bounded retry with stable idempotency key
- ack surface and audit visibility

Recommended Phase E hardening:

- restart-safe dedupe store (durable instead of memory-only)
- explicit sender authorization policy hooks (role-based controls)
- richer timeout/backoff policy by action type
- failure-injection tests for daemon restart, queue backlog, and stale control refs
