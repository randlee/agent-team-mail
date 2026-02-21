# Codex TUI MVP Architecture (Phase C / 13)

**Version**: 0.2  
**Date**: 2026-02-20  
**Status**: Draft

---

## 1. Purpose

Define an MVP TUI for ATM that provides:

- team and member operational visibility
- live/replay agent stream viewing
- message input through two explicit paths:
  - ATM mail send
  - control input to a live Codex/Gemini worker session

This document is intentionally API-first and avoids introducing parallel transport/state systems.

---

## 2. Core Principles

- The TUI is not the primary MCP client.
- Daemon is the source of truth for team/member/session status.
- Session logs are the source of truth for stream history and MVP live rendering.
- Control input uses explicit control protocol messages (not mailbox writes).
- Mail input uses existing ATM send semantics.
- Sender and receiver both emit structured event logs for all sends.

---

## 3. Views

### 3.1 Dashboard

Purpose:

- overview across teams, members, and active sessions
- primary place for ATM mail composition/sending

Content:

- Teams panel (team status summary)
- Members panel (active/idle/offline/error from daemon state)
- Sessions panel (live/idle/error badges)
- Mail composer (`Mail` path only)

Behavior:

- Selecting a **live** agent session enters Agent Terminal view.
- Selecting a non-live agent remains in Dashboard with mail actions available.

### 3.2 Agent Terminal

Purpose:

- focused stream view for one selected live session
- interactive control input for that session

Content:

- stream pane (`LIVE` or `REPLAY` source indicator)
- status line (session state + source)
- control input box (`Control` path only)

Behavior:

- live session: follow stream updates
- idle/disconnected: replay from session log
- control input disabled when target is not live

Live definition (normative):

- `live` means `SessionStatus::Active` AND `AgentState` in `{Idle, Busy}`
- `Launching`, `Killed`, `Stale`, and `Closed` are not live

### 3.3 Event Log

Purpose:

- global operational visibility and troubleshooting

Content:

- append-only event table
- filter bar

Primary filters:

- team (primary)
- agent
- source
- level
- action
- result

---

## 4. Input Paths

### 4.1 Mail Mode

- target: `agent[@team]`
- backend: ATM send path
- semantics: durable async delivery
- available from: Dashboard

### 4.2 Control Mode

- target: live worker session (`session_id` + `agent_id`)
- backend: control protocol (MCP-facing receiver path)
- semantics: immediate interactive input + async ack
- available from: Agent Terminal

Routing safety:

- Dashboard composer sends only mail.
- Agent Terminal composer sends only control input.
- No silent fallback between channels.

---

## 5. Stream Source Policy

MVP:

- use session log tail as live/replay source (`tail -f` behavior)

Planned enhancement:

- direct MCP JSONL live stream for lower latency, with automatic fallback to log replay

---

## 6. Event Log Rendering

On-disk compact keys (existing event log) are mapped to full column names in UI:

- `ts` -> `Timestamp`
- `lv` -> `Level`
- `src` -> `Source`
- `act` -> `Action`
- `team` -> `Team`
- `sid` -> `Session ID`
- `aid` -> `Agent ID`
- `anm` -> `Agent Name`
- `target` -> `Target`
- `res` -> `Result`
- `mid` -> `Message ID`
- `rid` -> `Request ID`
- `cnt` -> `Count`
- `err` -> `Error`
- `msg` -> `Message`

---

## 7. Navigation and Input UX

Baseline controls:

- Arrow keys: navigation
- `Tab` / `Shift+Tab`: focus cycle
- `Enter`: submit active composer
- `Esc`: cancel composer / close prompt
- `PgUp` / `PgDn`, `Home` / `End`: stream and log scrolling
- `/`: search
- `f`: filter editor
- `c`: clear filters
- `F`: follow mode toggle
- `Ctrl+C`: interrupt action in Agent Terminal (with optional confirmation)

Clipboard and text:

- support normal terminal copy/paste
- accept arbitrary UTF-8 text input
- multiline input is sent as one payload

---

## 8. Limits and Oversize Behavior

- soft input limit: `64 KiB`
- hard input limit: `1 MiB` (configurable)
- over soft limit: warn before send
- over hard limit: block inline send and switch to file-reference send path

File-reference path uses shared local storage and sends `content_ref` metadata in control request.

---

## 9. Identity and Correlation

Definitions:

- `session_id`: Claude session identifier
- `agent_id`: ATM worker session identifier (e.g. `codex:...`)
- `thread_id`: [MCP-internal adapter only] backend-specific conversation handle (e.g. Codex internal thread ID) — not required by TUI; injected by MCP adapter layer when available

**Public identifiers for TUI**: `session_id` and `agent_id`. TUI never requires `thread_id`; it is an MCP-adapter concern.

Correlation:

- all control requests require `request_id`
- retries must reuse the same `request_id` (idempotent)

---

## 10. Non-Goals (MVP)

- replacing daemon as state owner
- introducing a parallel UDP raw stdout transport
- inferring team/member status in TUI without daemon state

---

## 11. Open Items

- select canonical control endpoint transport (Unix socket / existing daemon command channel)
- finalize interrupt confirmation policy (`always`, `never`, `configurable`)
- define persistent per-user UI preferences file location and schema

---

## 12. Implementation Gating and Policy

Current capability status:

- control protocol messages in `docs/tui-control-protocol.md` are **draft contracts**
- daemon command handlers for these control message types are not yet implemented
- TUI must feature-gate control actions behind explicit capability checks

Security and policy baseline:

- receiver must enforce same-team scoping for control actions
- receiver must authorize sender identity before accepting control requests
- all denied control attempts must be audit logged with `request_id`, `team`, `agent_id`, and rejection reason
- TUI must never silently downgrade control input to mailbox send

Phase alignment:

- these docs define a contract baseline for Phase C validation work and Phase D UI implementation
- recommended Phase C scope is a lightweight `C.3` receiver stub/contract (endpoint, validation, ack, dedupe skeleton)
- full interactive TUI control UX remains a Phase D deliverable
