# Phase O.1 Attach Design Notes

Date: 2026-02-24  
Owner: `arch-ctm`

## 1. Attach vs Watch Interaction Model

- `watch`: read-only stream consumption.
- `attach`: read + write binding to one `agent_id`.

Input routing contract in attached mode:
- Plain text line: routed as agent input (`control.stdin.request`).
- `:`-prefixed command: routed as local attach control command.
  - `:interrupt` -> `control.interrupt.request`
  - `:approve [text]` / `:reject [text]` -> stdin approval response payload
  - `:help` -> command contract help
  - `:detach` -> detach/exit

This removes ambiguity between "text for model" and "local command".

## 2. Attach/Detach State Model

Attached state machine:
1. `Detached` -> `Attaching` on `attach <agent-id>`.
2. `Attaching` replays bounded stream history from watch feed.
3. `Attached` tails watch feed and dispatches control requests.
4. `Attached` -> `Detached` on `:detach`, EOF, or process exit.

Watch lifecycle alignment:
- Read-path reuses existing watch-stream files and replay semantics.
- No mutation of watch subscription protocol needed for O.1 CLI attach baseline.

## 3. Bidirectional Integration Test Strategy

Goal: test attach behavior without requiring a real Codex binary.

Strategy:
1. Use watch-feed fixture files (JSONL) to test replay + live tail parsing.
2. Unit-test input routing (`plain` vs `:` commands) deterministically.
3. Unit-test typed attached-event envelope mapping from watch frames.
4. Integration test control dispatch by mocking daemon control ACK path where possible; keep real-Codex attach smoke tests optional/gated (`CODEX_BIN` style).

Rationale:
- Keeps CI deterministic and cross-platform.
- Exercises O.1 routing and stream-binding logic while deferring full renderer parity to O.2.

## 4. Typed Attached Event Envelope

O.1 introduces an attached-mode event envelope separate from watch-only string rendering:
- `mode = "attached"`
- classified event class (e.g., `assistant.output`, `approval`, `tool.exec`, `turn.lifecycle`, `input.atm_mail`)
- source attribution fields
- raw frame passthrough for forward compatibility

This creates a stable typed contract for O.2 renderer work.

## 5. Render Fallback Notes

- `print_frame` intentionally preserves a generic fallback path: when a class-specific renderer is unavailable, attached mode prints class/source with either extracted text or event type.
- Unknown/unhandled classes are surfaced explicitly (not dropped), and parity/deviation decisions are tracked in `phase-o-deviation-log.md`.
