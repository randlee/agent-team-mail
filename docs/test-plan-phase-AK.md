# Test Plan — Phase AK (Mandatory OpenTelemetry Rollout)

## Scope

Validate mandatory OTel behavior across in-scope tools while preserving
non-blocking local structured logging and canonical observability schema rules.

## Requirement Mapping

| Requirement Area | Sprint | Planned Coverage |
|---|---|---|
| Schema/path/health contract reconciliation | AK.1 | unit tests for schema validation, path resolution, and health JSON key presence |
| Mandatory OTel in shared crate | AK.2 | crate-level tests for default-on exporter, retry/backoff, fail-open fallback |
| Producer integration rollout | AK.3 | integration tests per producer confirming trace/metric/log emission and required attrs |
| Diagnostics/runbook closure | AK.4 | doctor/status JSON contract tests + human-output degradation messaging tests |
| Release confidence | AK.5 | end-to-end smoke tests and cross-platform CI matrix checks |

## Planned Test Suites

## AK.1 — Contract and Schema

- `LogEventV1` schema tests:
  - Scope-based mandatory correlation fields:
    - agent/runtime-scoped: `team`, `agent`, `runtime`, `session_id`
    - trace events: `trace_id`, `span_id`
    - sub-agent events: `subagent_id` plus all required runtime/trace keys
  - `spans` semantic shape validation:
    - root→leaf ordering
    - same-trace invariant
    - parent-chain invariant (`parent_span_id == previous span_id`)
    - leaf matches top-level `trace_id`/`span_id` when provided
- Pathing tests:
  - ATM-managed profile:
    - sink `${home_dir}/.config/atm/logs/<tool>/<tool>.log.jsonl`
    - spool `${home_dir}/.config/atm/logs/<tool>/spool`
  - Standalone profile:
    - sink `${home_dir}/.config/<tool>/logs/<tool>.log.jsonl`
    - spool `${home_dir}/.config/<tool>/logs/spool`
- Health JSON contract tests:
  - `logging_health` key set present for `doctor --json` and `status --json`.
  - Shared keys are byte-for-byte identical across doctor/status outputs
    (name, type, nullability expectations).

## AK.2 — Shared Crate OTel Core

- Default-on exporter tests:
  - exporter initialized unless explicit controlled-disable path.
- Fail-open tests:
  - exporter transport failure does not fail command path.
  - local log sink continues writing.
- Retry/backoff tests:
  - retry scheduling and bounded backoff behavior.

## AK.3 — Producer Integration

- Producer coverage tests:
  - `atm`, `atm-daemon`, `atm-tui`, `atm-agent-mcp`, `scmux`, `schook`,
    `sc-compose`, `sc-composer`.
- Required attribute checks:
  - `team`, `agent`, `runtime`, `session_id` for runtime/agent-scoped events.
  - `trace_id`, `span_id` for traces.
  - `subagent_id` for sub-agent traces/events.

## AK.4 — Diagnostics and Runbook

- `atm doctor --json` and `atm status --json` parity tests:
  - identical `logging_health` key names and semantics.
- Human-output tests:
  - degraded/unavailable state includes actionable remediation text.

## AK.5 — End-to-End and CI

- Cross-platform matrix:
  - Linux, macOS, Windows.
- Release confidence smoke tests:
  - OTel backend reachable path.
  - OTel backend unreachable path (degrade without blocking).
  - log rotation/spool merge behavior under exporter outages.

## Exit Criteria

1. All AK test suites are implemented and passing in CI.
2. OTel is default-on and non-optional for AK scope.
3. Local logging continuity is proven under exporter failure conditions.
4. Observability JSON contracts are stable and validated in tests.
