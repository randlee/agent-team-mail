# TUI Control Protocol (Phase C / 13)

**Version**: 0.1  
**Date**: 2026-02-20  
**Status**: Draft

---

## 1. Scope

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

---

## 2. Envelope

All control messages are JSON objects with:

- `type` (string): message type
- `v` (integer): schema version (current: `1`)
- `request_id` (string): stable idempotency key for the logical send
- `team` (string): team namespace
- `session_id` (string): Claude session id
- `agent_id` (string): worker session id
- `sent_at` / `acked_at` (RFC 3339 UTC string)

Optional:

- `thread_id` (string): backend-native thread/conversation handle
- `meta` (object): transport/UI metadata

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

- `thread_id`
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

- `thread_id`
- `meta.retry_count`
- `meta.ui_source`

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
- receiver deduplicates by `request_id + session_id + agent_id`
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
  "thread_id": "thread-xyz789",
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
