# Phase O Pre-Sprint Design Notes

Date: 2026-02-24  
Owner: `arch-ctm`

## 1. Scope

Phase O targets attached CLI parity for `atm-agent-mcp attach <agent-id>`.
The phase is split into:
- O.1 stream/control wiring,
- O.2 renderer/runtime parity expansion,
- O.3 control-path parity and deferred fixture closure.

## 2. Risks

- Attached mode must not hide stream or control failures (FR-23.5).
- Unknown/degraded/out-of-scope events must be visible (FR-23.10/23.11).
- Replay continuity and attach/detach behavior must stay deterministic.

## 3. O.1 Design Guardrails (Required by QA)

- Tail/read failures are surfaced as visible `stream.error` lines, never silent.
- Control dispatch failures are classified by I/O kind and rendered explicitly.
- Unknown event classes are emitted as `unsupported.<event_type>` and counted.
- Replay behavior is fixture-tested from JSONL watch-stream samples.

## 4. O.2/O.3 Hand-off Contract

- O.1 exposes typed attached envelopes with stable class/source fields.
- O.2 extends render parity for required/degraded classes.
- O.3 closes approval/reject, interrupt/cancel, fault-state parity plus deferred fixture matrix.

## 5. Validation Plan

- Unit tests: parse routing contract, event-class mapping, unsupported counters.
- Fixture tests: watch replay parsing from `tests/fixtures/attach/*.jsonl`.
- Mock daemon integration test: control request/ack round-trip via Unix socket mock.
- Parity suite: `cargo test -p agent-team-mail-tui` on deferred scenarios.
