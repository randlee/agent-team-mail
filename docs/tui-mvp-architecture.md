# Codex TUI MVP Architecture (Phase D / 15, with Phase E Follow-up)

**Version**: 0.3  
**Date**: 2026-02-21  
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

## 1.1 Phase Placement (Normative)

- **Phase D** is the TUI MVP implementation phase:
  - D.1: dashboard + stream viewer
  - D.2: interactive control input (stdin + interrupt request path)
- **Phase E** is recommended for production hardening after D feature-complete:
  - resiliency, perf tuning, accessibility, and operational polish
  - stricter delivery SLOs and failure-injection validation

This keeps D focused on shipping usable end-to-end behavior while E handles operational quality and scaling concerns.

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
- `agent_id`: canonical conversation/session identifier for ATM and TUI surfaces (backend-agnostic)
- `thread_id`: [MCP-internal adapter only] backend-specific conversation handle (e.g. Codex internal thread ID) — not required by TUI; injected by MCP adapter layer when available

**Public identifiers for TUI**: `session_id` and `agent_id`. TUI never requires `thread_id`; it is an MCP-adapter concern.

Correlation:

- all control requests require `request_id`
- retries must reuse the same `request_id` (idempotent)

Backend mapping rule:

- provider-specific ids (for example Codex `threadId`) are MCP-internal details
- outside MCP, callers use `agent_id` only

---

## 10. Non-Goals (MVP)

- replacing daemon as state owner
- introducing a parallel UDP raw stdout transport
- inferring team/member status in TUI without daemon state

---

## 11. State Model and Data Ownership

Source of truth by concern:

- team/member/session status: daemon query APIs
- stream history and MVP live view: session logs
- control acceptance/rejection and dedupe: daemon control receiver
- mail delivery and inbox counts: ATM mail commands and mailbox files

No TUI-local inferred state may override daemon state for liveness decisions.

---

## 12. Failure Modes and Degraded Behavior

Required behavior:

- daemon unavailable:
  - dashboard enters degraded state
  - no control input allowed
  - optional replay-only stream from local logs
- control ack timeout:
  - one retry using same `request_id`
  - then surface explicit timeout result in UI
- stream source interruption:
  - auto-reconnect to log tail
  - if unavailable, freeze pane with explicit source/error indicator
- malformed event row:
  - drop row, emit structured warning event, continue render loop
- invalid UTF-8 input:
  - reject send locally with explicit user-visible error

---

## 13. Security and Policy Baseline

- control actions must remain same-team scoped
- unknown sender/target/session are deny-by-default at receiver
- TUI must display receiver denial details without rewriting meaning
- no silent channel fallback (`control` never auto-converts to `mail`)
- message content remains excluded from logs by default
- verbose mode may include truncated/full payloads per existing log policy

---

## 14. Testing and Acceptance Strategy

Minimum test layers:

- unit tests:
  - keymap/focus routing
  - live-state gating
  - request payload construction and retry identity (`request_id` reuse)
- integration tests:
  - daemon status polling + render updates
  - control send/ack path against local daemon socket
  - replay fallback when live source unavailable
- E2E manual scripts:
  - dashboard mail flow
  - agent terminal stdin flow
  - interrupt flow with expected unsupported/implemented result

Phase D exit gates:

- no panics in normal startup/shutdown/selection/send flows
- dashboard and agent terminal boundaries enforced
- control send path emits sender and ack audit events with correlation ids

Phase E exit gates (recommended):

- stress test: sustained stream + control activity without UI starvation
- failure-injection scenarios pass (daemon restart, stale sessions, queue backlog)
- documented SLO targets met (render responsiveness + ack visibility)

---

## 15. Open Items

- finalize interrupt confirmation policy (`always`, `never`, `configurable`)
- define per-user UI preferences file location and schema
- decide whether Phase E introduces direct MCP JSONL live stream as default with log-tail fallback

## 15.1 D.2 MVP Scope Decisions (Implementation Notes)

**Control input activation model (D.2 MVP)**: The Agent Terminal control input box uses a **single-state activation model** — panel focus equals input active. When `FocusPanel::AgentTerminal` is focused and the session is live, all printable character input goes directly to the control input field. The `control_input_active` field in `App` is reserved for a future two-state model (e.g. pressing Enter to enter edit mode) and is not wired in D.2. The two-state model is deferred to D.3+.

**Interrupt gating (D.2 MVP)**: Ctrl-I interrupt requests are gated **client-side** on `is_live()`. Interrupt is only sent when the target agent is in `{idle, busy}` state. For non-live agents, the interrupt is silently dropped at the client (no socket send). This is consistent with the live-state contract — non-live sessions cannot receive control input.

---

## 16. Implementation Gating and Policy

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
- production hardening, latency/robustness SLO enforcement, and operator tooling polish are recommended Phase E scope
