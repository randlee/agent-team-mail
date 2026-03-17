# Phase AV Planning — Live OTel Collector Integration + Partitioning

**Status**: PLANNED

## Goal

Get OpenTelemetry live for the binaries that ship from this repository while
using the work as an architecture cleanup:

- real collector export for in-repo ATM binaries
- fail-open local observability remains intact
- collector/export code is isolated behind one dedicated adapter layer
- generic crates do not absorb backend-specific OTel SDK dependencies

## Scope

In scope for this repository:

- `atm`
- `atm-daemon`
- `atm-core` request/lifecycle emission paths
- `atm-tui`
- `atm-agent-mcp`
- `sc-compose`
- `sc-composer`
- `sc-observability`

Explicitly out of scope for AV implementation:

- `scmux`
- `schook`

Those remain follow-on work in their own repositories and must be documented as
such instead of being implied complete.

## Inputs

- `docs/observability/requirements.md`
- `docs/observability/architecture.md`
- historical placeholder: `docs/archive/phases/phase-ak-planning.md`
- open issue references:
  - `#624` external follow-up for `scmux` / `schook`
  - `#640` OTel docs wiring / cross-reference closure

## Problem Statement

The repo currently emits OTel-shaped records locally:

- `sc-observability` builds neutral `OtelRecord` payloads
- `atm-daemon` writes mirrored `.otel.jsonl` sidecar files

What is still missing:

- no live OTLP collector export
- no canonical collector config surface
- no transport/auth/TLS adapter boundary
- no explicit rule preventing OTel SDK/client dependencies from leaking through
  CLI/daemon/application crates
- no current phase plan that distinguishes in-repo rollout from external-repo
  follow-on work

## Architecture Direction

### 1. Keep generic observability generic

`sc-observability` should continue to own:

- event schema and validation
- redaction and fail-open logging behavior
- local JSONL and `.otel.jsonl` mirror output
- neutral `OtelRecord` shaping
- exporter trait definitions

It should not become the home for:

- OTLP protocol/client SDKs
- collector auth/TLS logic
- endpoint-specific batching/retry policy
- backend-specific vendor logic

### 2. Introduce one dedicated OTel transport adapter

Create a dedicated adapter layer/crate, `sc-observability-otlp`, that owns:

- `opentelemetry*` / `opentelemetry-otlp` dependencies
- OTLP/HTTP collector transport
- stdout debug exporter
- auth/TLS/header config
- remote batching and retry/flush behavior
- translation from neutral `OtelRecord` to the chosen SDK/export API

That crate is the only collector-facing layer in the repo.

### 3. Keep application crates on the facade

`atm`, `atm-daemon`, `atm-tui`, `atm-agent-mcp`, `sc-compose`, and
`sc-composer` should request telemetry through the shared observability facade
only. They must not:

- import OTLP SDK crates directly
- construct exporters directly
- own per-binary collector behavior

## Export Targets

Priority order:

1. OTLP/HTTP collector export
2. stdout debug exporter for controlled operator diagnostics
3. local `.otel.jsonl` mirror retained for fail-open auditing/debug

The collector path must be optional at runtime but mandatory in capability:

- if collector config is present, export attempts happen by default
- if collector is unavailable, local logging remains authoritative and
  non-blocking

## Highest-Value Instrumentation

### Traces / spans

- `atm.send`
- `atm.read`
- daemon request handling / request dispatch
- daemon plugin dispatch and lifecycle transitions
- GitHub firewall decision path
- GitHub execution ledger calls
- MCP request/session lifecycle in `atm-agent-mcp`

### Metrics

- command/request counts and duration
- daemon request latency
- spool fallback count
- OTel exporter success/failure count
- collector retry/backoff count
- GH firewall blocked/allowed counts
- GH ledger call counts
- session/worker lifecycle counts already modeled in structured events

### Health/reporting

- collector reachability state
- last export error
- local mirror state
- queue/spool fallback state
- consistent doctor/status JSON keys

## Sprint Breakdown

### AV.0 — Boundary Remediation Prerequisite

Deliver:

- remove the pre-existing direct `sc-observability` imports that already bypass
  the intended facade/boundary:
  - `crates/atm-daemon/src/daemon/socket.rs`
  - `crates/atm-daemon/src/daemon/log_writer.rs`
  - `crates/sc-composer/src/lib.rs`
  - `crates/sc-compose/src/observability.rs`
- move those call sites onto the canonical facade/entry-point wiring before any
  new collector adapter is introduced

Acceptance:

- the file-level violations above are removed
- AV.2 does not begin until AV.0 is merged
- QA can enforce the AV transport boundary on a clean baseline instead of an
  already-bypassed codebase

### AV.1 — Boundary + Config Contract

Deliver:

- finalize the `sc-observability-otlp` transport adapter boundary
- define canonical config surface for endpoint/protocol/auth/TLS/debug export
- document in-repo vs out-of-repo scope explicitly
- add CI/review rule forbidding direct `opentelemetry*` and
  `sc-observability-otlp` imports outside the adapter/entry-point layer

Acceptance:

- requirements and architecture docs explicitly separate generic observability
  from collector transport
- crate/dependency ownership is unambiguous
- `sc-observability-otlp` is the committed adapter crate name
- a CI import lint rule exists before AV.2 begins and blocks
  `sc-observability-otlp` imports from non-entry-point modules

### AV.2 — Transport Adapter

Deliver:

- add the dedicated transport adapter crate/layer
- wire OTLP/HTTP export
- wire stdout debug export
- preserve fail-open local mirror behavior

Acceptance:

- collector export works through one adapter
- local logging still succeeds during collector outage

### AV.3 — Daemon/CLI/Core Instrumentation

Prerequisite: Phase AT must be merged to `develop` before AV.3 begins. AV.3
must instrument the post-AT GitHub ownership layout rather than targeting code
paths that AT is still relocating.

Deliver:

- high-value traces/metrics for `atm`, `atm-daemon`, and `atm-core` emission
  paths
- GitHub firewall / ledger spans and metrics
- daemon request and lifecycle instrumentation closure

Acceptance:

- collector traces/metrics are useful for request/lifecycle diagnosis, not just
  raw event mirroring

### AV.4 — In-Repo Producer Rollout + Health

Deliver:

- rollout to `atm-tui`, `atm-agent-mcp`, `sc-compose`, `sc-composer`
- doctor/status OTel health surface
- troubleshooting/docs cross-reference closure (`#640`)

Acceptance:

- all in-repo binaries use the shared facade and report canonical OTel health

### AV.5 — Dogfood + QA + External Handoff

Deliver:

- collector-backed smoke tests on real dev install
- outage/fail-open regression tests
- explicit handoff notes for `scmux` / `schook` follow-on (`#624`)

Acceptance:

- ATM can dogfood collector export without breaking local logging
- QA confirms partition boundary and fail-open behavior

## Dependencies

- likely new crates:
  - `opentelemetry`
  - `opentelemetry_sdk`
  - `opentelemetry-otlp`
- possible transport dependencies depending on chosen SDK path:
  - `reqwest` or tonic stack as required by the OTLP implementation

Configuration topics to settle in AV.1:

- endpoint env var / config key
- OTLP protocol choice (AV default: HTTP)
- auth header injection
- TLS/CA material
- timeout/retry policy
- local debug exporter enablement

## QA Guidance

QA should review each AV sprint against two axes:

1. functionality
   - does telemetry reach the collector and remain useful?
2. architecture
   - did the sprint keep collector-specific code inside the dedicated adapter
     boundary?

Blocking QA failure examples:

- direct `opentelemetry*` import added to application/daemon/CLI crates
- direct `sc-observability-otlp` import added outside approved entry-point
  modules
- collector outage blocks command success
- per-binary exporter config drift
- AV claims external repo coverage (`scmux`/`schook`) without actual delivery

## Exit Criteria

1. Real collector export works for in-repo ATM binaries.
2. Local JSONL and `.otel.jsonl` fallback remain non-blocking and available.
3. OTel transport code is isolated to one dedicated adapter layer.
4. Doctor/status expose canonical collector and local-mirror health.
5. QA confirms both the functionality and the partition boundary.
