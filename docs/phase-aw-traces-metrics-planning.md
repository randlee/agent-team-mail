# Phase AW Planning: OTel Traces + Metrics Expansion

**Status**: Complete
**Prerequisite**: Phase AV merged and dogfooded against a Grafana-compatible
OTLP HTTP logs receiver.

## Goal

Expand the AV logs-only rollout into full OTel traces and metrics while keeping
the architecture partition clean:

- `sc-observability`: neutral event, span, and metric abstractions
- `sc-observability-otlp`: AW-created OTLP transport only
- entry-point binaries: process-level wiring only

## Problem Statement

Phase AV gets ATM to a useful first operational milestone:

- remote OTLP HTTP logs
- local fail-open logging preserved
- collector endpoint, auth, TLS, timeout, and retry config centralized

What AV does not yet deliver:

- native OTLP traces export
- native OTLP metrics export
- Grafana dashboards for spans/metrics
- external consumer rollout guidance beyond logs-first handoff

## AW Scope

1. Add trace export as a first-class signal, not just trace IDs embedded in logs.
2. Add metric instruments and export.
3. Define Grafana dashboard and query expectations for ATM observability.
4. Keep transport/SDK dependencies isolated in `sc-observability-otlp`.
5. Define the integration contract external repos (`scmux`, `schook`) must follow.

## Architecture Guardrails

- `sc-observability` may define neutral `TraceRecord` / `MetricRecord` shapes,
  correlation rules, buffering hooks, and fail-open semantics.
- `sc-observability-otlp` owns OTLP payload shaping, batching, retry, auth/TLS,
  and client dependencies for logs, traces, and metrics.
- Non-entry-point modules must not import OTLP SDK/client crates directly.
- Grafana- or backend-specific assumptions must live in docs/contracts, not in
  generic feature code.

## Sprint Map

| Sprint | Focus | Deliverables |
|---|---|---|
| AW.1 | Signal contracts + transport crate creation | Create `crates/sc-observability-otlp` as a new workspace member crate; define neutral `TraceRecord` / `MetricRecord` types, required correlation fields, schema rules, and import-boundary updates |
| AW.2 | OTLP transport expansion | Expand the new `sc-observability-otlp` crate with `/v1/traces` and `/v1/metrics` support plus shared batching/timeout/retry policy |
| AW.3 | Producer trace rollout | Instrument ATM/daemon/selected producers with real spans and span lifecycle coverage |
| AW.4 | Metrics rollout | Counters/histograms/gauges for ATM health and activity, export wiring, and diagnostics coverage |
| AW.5 | Grafana dashboards + smoke | Dashboards/query recipes for logs/traces/metrics plus end-to-end smoke verification |
| AW.6 | External consumer rollout | `scmux` / `schook` adoption contract, checklist, and handoff validation |

## AW.5 Status

AW.5 deliverables are implemented as:

- `docs/observability/grafana-dashboards.md`
  - Grafana LogQL, TraceQL, and PromQL recipes for ATM observability signals
- `scripts/grafana-verify-smoke.py`
  - live Loki-backed smoke with `--dry-run` support and env-driven auth
- `docs/observability/grafana-rollout-smoke.md`
  - rollout/operator smoke contract for Grafana-compatible collector setups

The automated AW.5 smoke intentionally validates the log ingestion path first:

- ATM commands run with `ATM_OTEL_ENABLED=true`
- remote verification happens through the Loki read endpoint
- PASS requires a matching stream keyed by `service_name` or `source_binary`
  plus the canonical correlation fields `team`, `agent`, `runtime`, and
  `session_id`

Trace and metric query recipes are documented for dashboard rollout, but the
smoke script remains logs-focused because that is the least brittle remote
verification surface across Grafana-compatible OTLP deployments.

AW.5 smoke therefore covers the logs endpoint only, in both dry-run and live
connectivity modes. Full traces-and-metrics smoke against Tempo/Mimir-class
backends requires live endpoints and is deferred to AW.7 and later.

## AW.6 Status

AW.6 rollout artifacts are:

- `docs/observability/external-consumer-contract.md`
- `docs/observability/external-consumer-checklist.md`
- `scripts/validate-external-consumer.sh`

Those artifacts define and validate the external-repo contract:

- feature code uses `sc-observability` only
- collector transport stays behind approved entry-point setup
- `sc_observability_otlp` and raw `opentelemetry*` imports are forbidden
  outside that setup boundary
- local fail-open logging remains mandatory

The validator provides a `--dry-run` mode so a rollout owner can confirm the
enforcement surface before running the real repo scan against `scmux`,
`schook`, or a new consumer repo.

## Dependencies

- AV must be merged and stable as a logs rollout.
- The AV Grafana smoke in
  `docs/observability/grafana-rollout-smoke.md` must pass first.
- External repos must accept the shared adapter boundary rather than reintroduce
  transport-specific code.

## External Repo Integration Contract

`scmux` and `schook` need:

- `sc-observability` facade usage only in feature code
- no direct OTLP client wiring outside approved entry-point setup
- the same env/config surface as ATM:
  - `ATM_OTEL_ENABLED`
  - `ATM_OTEL_ENDPOINT`
  - `ATM_OTEL_PROTOCOL`
  - `ATM_OTEL_AUTH_HEADER`
  - `ATM_OTEL_CA_FILE`
  - `ATM_OTEL_INSECURE_SKIP_VERIFY`
  - timeout/retry controls
- local fail-open logging preserved when collector export fails

## Grafana Requirements

AW should deliver:

- log queries by `team`, `agent`, `runtime`, `session_id`
- trace views for key ATM flows
- metric panels for:
  - event volume
  - export failures
  - dropped events / spool growth
  - daemon request volume / latency
  - subagent activity counts and duration

## Exit Criteria

1. ATM emits native OTLP traces and metrics in addition to logs.
2. Logs/traces/metrics all remain fail-open with local logging preserved.
3. `sc-observability-otlp` remains the only transport-owning crate.
4. Grafana smoke covers the logs endpoint end-to-end, with trace and metric
   dashboard recipes published and full live traces-and-metrics smoke deferred
   to AW.7+.
5. External consumer repos have a concrete adoption contract and checklist.

## Follow-On

Live Grafana smoke after AW identified deployment/dogfood gaps that are outside
the original AW implementation scope:

- live Loki verification of `service_name="atm"`
- fresh-daemon Tempo verification for `atm-daemon` traces
- canonical Mimir metric-name/query alignment in smoke/docs
- shared dev-daemon/startup dogfood readiness

Those follow-up fixes are planned in
`docs/phase-ay-grafana-dogfood-readiness.md`.
