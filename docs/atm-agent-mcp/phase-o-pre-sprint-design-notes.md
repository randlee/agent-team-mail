# Phase O Pre-Sprint Design Notes (O.1 Readiness)

Date: 2026-02-24  
Owner: `arch-ctm`

Purpose:
- resolve known O.1/O.2/O.3 design risks up front so sprint execution stays implementation-focused.

## 1. Attach vs Watch Interaction Model

Decision:
- `watch`: read-only stream pane.
- `attach`: interactive mode with explicit input-router states.

Input routing contract for `attach`:
1. Default state: **agent-input mode** (typed text routes to agent turn input).
2. Command state: **control mode** (entered via explicit prefix or key chord) for local controls.

Recommended controls:
- `Enter`: submit agent input.
- `Ctrl-C`: interrupt/cancel in-flight turn.
- `Esc`: clear current draft.
- `Ctrl-G`: toggle control command palette.
- `:` prefix in empty composer: parse as local attach command (for example `:detach`, `:help`, `:watch`).

Rule:
- No ambiguous dual interpretation. If in control mode, text never reaches model input until mode exits.

## 2. O.2 Sizing and Split Trigger

Baseline plan keeps O.2 as one sprint, with an explicit split trigger:

- If by midpoint O.2 both conditions are true:
  1. diff/patch rendering parity is not landed, and
  2. tool lifecycle rendering is not fixture-backed,
  then split to:
  - `O.2a` renderer core/layout parity,
  - `O.2b` diff + tool lifecycle parity.

This preserves O.3 for control-path closure rather than renderer spillover.

## 3. O.3 Load Control

O.3 scope guard:
- Mandatory: approval/reject, request-user-input/elicitation, interrupt/cancel, explicit fault states.
- Mandatory fixtures: deferred M.7 set (`multi-item`, `fatal-error`, `unknown-event`, `atm-mail`, `user-steer`, `session-attach`, `detach-reattach`, `cross-transport`).
- Any extra polish must not displace these fixtures.

Completion gate:
- O.3 is complete only when required scenarios are fixture-backed and passing in CI.

## 4. Attach Integration Test Strategy (Without Real Codex Binary)

Approach:
1. Keep existing mock child for protocol-level tests.
2. Add an **interactive attach harness fixture binary** that simulates:
  - streaming deltas,
  - approval request/decision loops,
  - interrupt acknowledgement,
  - request-user-input/elicitation round-trips.
3. Run deterministic transcript tests against attach renderer and input router.
4. Optional real-codex smoke tests gated behind env (`CODEX_BIN`) and non-blocking in default CI.

Outcome:
- Core attach behavior is testable in CI without requiring real Codex installation.

## 5. Elicitation Complexity Handling

`RequestUserInput` and `ElicitationRequest` are `Required` by matrix and must be first-class:

- Render as dedicated prompt blocks, not generic message lines.
- Preserve request correlation IDs and timeout outcomes in UI event stream.
- Support structured choices and response confirmation in attached mode.

Scope treatment:
- O.2: render surface + layout contracts.
- O.3: interaction behavior, timeout/reject paths, and fixtures.

## 6. Linked Planning Artifacts

- `docs/atm-agent-mcp/phase-o-event-applicability-matrix.md`
- `docs/atm-agent-mcp/codex-cli-atm-tui-render-gap-analysis.md`
- `docs/atm-agent-mcp/codex-parity-test-plan.md`
- `docs/atm-agent-mcp/requirements.md` (FR-23.*)
