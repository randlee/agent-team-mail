# Phase AK Planning — Mandatory OpenTelemetry Rollout

## Goal

Deliver non-optional OpenTelemetry across the observability scope while keeping
local structured logging always-on, canonical, and fail-open.

## Delivery Target

- Target version: `v0.44.x` (planning-level target)
- Integration branch: `integrate/phase-AK`

## Inputs

- Observability requirements and architecture:
  - `docs/observability/requirements.md`
  - `docs/observability/architecture.md`
- QA findings from first observability planning review:
  - ATM-QA-002 (path contract mismatch)
  - ATM-QA-003 (OTel correlation fields missing in schema)
  - ATM-QA-008 (`spans` semantics undefined)
  - ATM-QA-010 (doctor/status JSON keys not defined)

## Locked Contract

1. OTel is mandatory for in-scope tools in this phase (not optional).
2. Canonical session correlation key is `session_id` across all runtimes.
3. Runtime-specific names (`thread-id`, `session-id`) are adapter internals.
4. OTel exporter failures must never block command execution.
5. Local structured logging remains available regardless of OTel backend state.

## Phase Scope

1. Contract reconciliation and schema hardening
- Align requirements/architecture/path contracts to one canonical model.
- Add required OTel correlation fields and define `spans` shape semantics.
- Lock doctor/status JSON key names for logging health.

2. Shared crate mandatory OTel core
- Implement default-on OTel exporter wiring in `sc-observability`.
- Add retry/backoff and fail-open behavior.
- Ensure required correlation attributes are present for runtime/agent events.

3. Producer integration rollout
- Integrate mandatory OTel for:
  - `atm`
  - `atm-daemon`
  - `atm-tui`
  - `atm-agent-mcp`
  - `scmux`
  - `schook`
  - `sc-compose`
  - `sc-composer`

4. Diagnostics and runbook closure
- Ensure `atm doctor --json` and `atm status --json` report canonical logging
  health fields and degraded/unavailable conditions.
- Update troubleshooting runbook for mandatory OTel behavior and fallback paths.

5. QA and release confidence
- Full ATM-QA review of AK scope.
- Cross-platform CI and targeted reliability checks before release cut.

## Proposed Sprint Map

| Sprint | Focus | Size |
|---|---|---|
| AK.1 | Contract reconciliation + schema hardening | S |
| AK.2 | `sc-observability` mandatory OTel core | M |
| AK.3 | Producer integration rollout | L |
| AK.4 | Diagnostics/reporting + runbook closure | M |
| AK.5 | QA + release confidence | M |

## Dependency Graph

- AK.1 is required before all implementation work.
- AK.2 depends on AK.1.
- AK.3 depends on AK.2.
- AK.4 depends on AK.3.
- AK.5 depends on AK.3 and AK.4.

## Acceptance Criteria

1. OTel exporter is enabled by default for all in-scope tools.
2. Required correlation fields (`team`, `agent`, `runtime`, `session_id`,
   `trace_id`, `span_id`) are present where applicable.
3. Sub-agent telemetry includes `subagent_id` on traces/log events.
4. OTel backend outages degrade gracefully without blocking primary workflows.
5. `atm doctor --json` and `atm status --json` expose the same canonical logging
   health key set defined in requirements.
