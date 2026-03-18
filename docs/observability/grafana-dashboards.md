# Grafana Dashboards and Query Recipes

**Status**: Active for AW.5
**Phase**: AW.5 Grafana dashboards + smoke
**See also**:
- `docs/observability/architecture.md`
- `docs/observability/grafana-rollout-smoke.md`
- `docs/phase-aw-traces-metrics-planning.md`
- `scripts/grafana-verify-smoke.py`

## Purpose

This document defines the Grafana-side query contract for the AW-era ATM
observability rollout:

- logs arrive through OTLP HTTP and remain queryable by correlation fields
- traces are queryable by span name and shared correlation attributes
- metrics are queryable by service name, runtime, and team dimensions

The canonical source of those exported fields is:

- `crates/sc-observability/src/lib.rs`
- `crates/sc-observability/src/otlp_adapter.rs`
- `crates/sc-observability-otlp/src/lib.rs`

## Canonical Exported Dimensions

The AW.5 dashboards and smoke checks assume these exported dimensions:

- `service_name`
  - derived from `source_binary`
  - examples: `atm`, `atm-daemon`, `sc-compose`
- `team`
- `agent`
- `runtime`
- `session_id`

Trace and metric signals also preserve their signal-specific keys:

- traces:
  - `name`
  - `trace_id`
  - `span_id`
  - `parent_span_id`
  - `status`
- metrics:
  - `name`
  - `kind`
  - `unit`

## LogQL Recipes

Use these in Grafana Explore against Loki or a Grafana-compatible logs backend.

### 1. ATM command lifecycle for one session

```logql
{service_name="atm", session_id="$session_id"} |= "command_"
```

Purpose:
- verify ATM command events arrived remotely
- scope one smoke run by unique `session_id`

### 2. One team and agent across all ATM binaries

```logql
{team="$team", agent="$agent", runtime="$runtime"}
```

Purpose:
- inspect all correlated activity for one operator/runtime combination

### 3. Specific ATM binary if `service_name` mapping is unavailable

```logql
{source_binary="atm", session_id="$session_id"}
```

Purpose:
- fallback query if the collector preserves `source_binary` rather than
  promoting `service_name`

### 4. Error-path scan

```logql
{team="$team"} |= "error"
```

Purpose:
- fast operator check for exporter failures or command errors during smoke

### 5. Daemon OTel health or fail-open issues

```logql
{service_name="atm-daemon"} |= "otel"
```

Purpose:
- inspect daemon-side health/fail-open behavior while collector export is
  enabled or degraded

## TraceQL Recipes

Use these against Tempo or another Grafana-compatible trace backend after AW
trace rollout is enabled.

Span names in the recipes below match the instrumentation in
`crates/sc-observability/src/trace.rs`.

### 1. ATM command traces

```traceql
{ resource.service.name = "atm" && name =~ "atm.command.*" }
```

### 2. Daemon dispatch traces

```traceql
{ resource.service.name = "atm-daemon" && name = "atm-daemon.dispatch_message" }
```

### 3. One runtime/session slice

```traceql
{ session_id = "$session_id" && runtime = "$runtime" }
```

### 4. Error traces

```traceql
{ status = error }
```

## PromQL Recipes

Use these against Prometheus/Mimir-compatible metric storage after AW metric
rollout is enabled.

### 1. ATM message counter by team

```promql
sum by (team) (atm_messages_total)
```

### 2. Metric volume by binary

```promql
sum by (service_name) (atm_messages_total or log_events_total)
```

### 3. Runtime-scoped activity

```promql
sum by (runtime) (subagent_runs_total)
```

### 4. Histogram summary

```promql
histogram_quantile(0.95, sum by (le) (rate(subagent_run_duration_ms_bucket[5m])))
```

### 5. Error growth

```promql
sum(rate(errors_total[5m]))
```

### 6. Export failures

```promql
sum by (service_name) (rate(otel_export_errors_total[5m]))
```

Purpose:
- track OTLP exporter failures by emitting binary or service
- distinguish collector/backoff problems from healthy local fail-open logging

### 7. Spool growth

```promql
max by (runtime) (atm_spool_dir_bytes) or max by (runtime) (atm_spool_dir_entries)
```

Purpose:
- detect sustained local spool growth when remote export is degraded
- spot runtimes that are accumulating fail-open backlog instead of draining

### 8. Daemon request volume and latency

```promql
sum by (plugin, method) (rate(atm_daemon_requests_total[5m]))
or
histogram_quantile(
  0.95,
  sum by (le, plugin, method) (rate(atm_daemon_request_duration_ms_bucket[5m]))
)
```

Purpose:
- monitor daemon plugin request volume by route
- surface p95 latency regressions in daemon request handling

## Suggested Dashboard Panels

### Logs

- ATM command lifecycle stream by `session_id`
- daemon exporter/fail-open diagnostics
- team/agent filtered error log table

### Traces

- ATM command span list filtered by `team`, `agent`, `runtime`
- daemon dispatch span list
- error trace table grouped by `service_name`

### Metrics

- total ATM messages by `team`
- subagent activity by `runtime`
- error/warning rates
- duration percentiles for subagent runs
- OTLP export failures by `service_name`
- spool growth by `runtime`
- daemon request volume and p95 latency by `plugin` / `method`

## Smoke Expectations

The AW.5 smoke script validates only the log-side contract directly:

- it runs ATM commands with `ATM_OTEL_ENABLED=true`
- it queries Loki with a bounded time window
- it verifies a matching stream by `service_name` or `source_binary`
- it verifies `team`, `agent`, `runtime`, and `session_id` correlation fields

That smoke is implemented in `scripts/grafana-verify-smoke.py`.

Trace and metric recipes in this document are the operator contract for the
AW-era rollout and dashboard setup, but the automated Loki smoke intentionally
stays focused on the logs path because that is the lowest-friction remote
validation surface.
