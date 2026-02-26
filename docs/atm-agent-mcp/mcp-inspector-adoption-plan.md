# MCP Inspector Adoption Plan for ATM

## Objective
Adopt the official MCP Inspector (`modelcontextprotocol/inspector`) as a fast, repeatable harness for validating `atm-agent-mcp` server behavior before full end-to-end Codex client runs.

## Why This Tool
- First-party MCP ecosystem tool with active maintenance.
- Works as both web UI and CLI tester.
- Supports stdio/SSE/streamable HTTP target transports.
- Can run locally via `npx` (no long setup) and in Docker.

## Scope
In scope:
- Test `atm-agent-mcp serve` tool contracts (`tools/list`, `tools/call`) in a controlled harness.
- Capture reproducible test recipes and expected outcomes.
- Add a thin ATM-focused preset/config workflow.

Out of scope:
- Replacing full Codex client parity testing.
- Replacing daemon/TUI integration tests.

## Proposed Rollout

### Phase 1: Local Harness Baseline
- Run Inspector in UI mode.
- Connect to `atm-agent-mcp` (stdio) and validate:
  - connection/auth handshake,
  - `tools/list` shape,
  - core tool calls (`atm_send`, `atm_read`, `atm_pending_count` where available).
- Record known-good flows and failure signatures.

Exit criteria:
- A documented, repeatable local flow can be executed in <10 minutes.

### Phase 2: Scripted CLI Smoke Suite
- Use Inspector CLI mode (`--cli`) to run deterministic calls:
  - `tools/list`
  - selected `tools/call` cases (happy path + bad args)
- Save command snippets and expected response patterns.
- Integrate into manual release checklist and CI.
- CI profile constraints:
  - Do not run Codex child processes in this gate.
  - Test only MCP contract behavior exposed by `atm-agent-mcp serve`.
  - Focus on low-risk calls (`tools/list` + basic `tools/call` with bounded inputs).

Exit criteria:
- One-command smoke sequence that catches obvious MCP contract regressions.

### Phase 3: ATM Extensions (Minimal)
- Add ATM-specific sample config(s) and helper wrappers:
  - predefined server entry for `atm-agent-mcp serve`,
  - env var template for identity/team.
- Add fixtures for regression repro (payloads for known bug classes).

Exit criteria:
- New contributor can run the same test matrix with minimal setup.

## Risks and Mitigations
- Node version mismatch (`inspector` requires modern Node):
  - Mitigation: pin tested Node version in runbook.
- False confidence from isolated MCP tests:
  - Mitigation: keep Inspector as preflight; retain full E2E parity tests.
- Auth/proxy misconfiguration in shared environments:
  - Mitigation: keep localhost binding and auth token enabled by default.

## Deliverables
- `docs/atm-agent-mcp/mcp-inspector-adoption-plan.md` (this file)
- `docs/atm-agent-mcp/mcp-inspector-runbook.md` (setup + commands)
- CI-safe Inspector CLI smoke profile (documented in runbook)

## Recommendation
Adopt Inspector as the standard preflight MCP contract harness, then run Codex/TUI full-flow tests as a second gate.
