# External Consumer Contract

**Status**: Active for AW.6
**Applies to**: `scmux`, `schook`, and any new external consumer repo
**See also**:
- `docs/observability/architecture.md`
- `docs/phase-aw-traces-metrics-planning.md`
- `docs/observability/external-consumer-checklist.md`
- `scripts/validate-external-consumer.sh`

## Purpose

This contract defines how an external repo adopts ATM observability without
reintroducing transport-specific code outside the approved entry-point setup.

The canonical source files in this repository are:

- `crates/sc-observability/src/lib.rs`
- `crates/sc-observability-otlp/src/lib.rs`
- `scripts/ci/observability_boundary_check.sh`
- `scripts/otel-dev-install-smoke.py`

## Required Boundary Rules

### 1. Feature code uses `sc-observability` only

Feature modules may:

- initialize logging through `sc-observability`
- emit structured logs, traces, and metrics through `sc-observability`
- read neutral health/fail-open state surfaced by `sc-observability`

Feature modules must not:

- import `sc_observability_otlp`
- import `opentelemetry` crates directly
- construct OTLP HTTP payloads or clients directly

### 2. OTLP transport stays at the approved entry point

If an external repo needs explicit collector/adapter wiring, it must be
confined to an approved entry point:

- `src/main.rs`
- or a deliberately equivalent process bootstrap module

That entry point may:

- read the canonical env/config surface
- initialize the collector adapter
- wire health reporting and fail-open behavior

It must not:

- spread OTLP client usage into feature modules
- invent repo-specific collector env vars

### 3. Local fail-open logging is mandatory

Collector export must never be the only sink.

Every external consumer must preserve:

- canonical local JSONL logging
- `.otel.jsonl` local mirror when OTel export is enabled
- command/process success when the collector is unavailable, misconfigured, or
  unauthorized

### 4. Cargo dependency rule

External consumer feature crates must depend on:

- `sc-observability`

They must not add:

- `sc-observability-otlp` in feature crates
- raw `opentelemetry*` dependencies in feature crates

## Canonical Environment Surface

External consumers must honor the same configuration surface as ATM:

| Variable | Description | Default | Required |
| --- | --- | --- | --- |
| `ATM_OTEL_ENABLED` | Master switch for OTel export behavior. Values `false`, `0`, `off`, `no`, and `disabled` disable remote export. | `true` | Optional |
| `ATM_OTEL_ENDPOINT` | Collector OTLP HTTP endpoint base URL. | Unset | Required for collector export |
| `ATM_OTEL_PROTOCOL` | Transport protocol selector. Current contract is OTLP HTTP. | `otlp_http` | Optional |
| `ATM_OTEL_AUTH_HEADER` | Prebuilt authorization header sent to the collector. | Unset | Optional |
| `ATM_OTEL_CA_FILE` | Custom CA bundle path for collector TLS verification. | Unset | Optional |
| `ATM_OTEL_INSECURE_SKIP_VERIFY` | Disable TLS certificate verification for the collector connection. | `false` | Optional |
| `ATM_OTEL_TIMEOUT_MS` | Per-export request timeout in milliseconds. | `1500` | Optional |
| `ATM_OTEL_RETRY_MAX_ATTEMPTS` | Additional export retry attempts after the initial send. | `2` | Optional |
| `ATM_OTEL_RETRY_BACKOFF_MS` | Initial retry backoff in milliseconds. | `25` | Optional |
| `ATM_OTEL_RETRY_MAX_BACKOFF_MS` | Maximum retry backoff in milliseconds. | `250` | Optional |
| `ATM_OTEL_DEBUG_LOCAL_EXPORT` | Emit extra local debug export records for troubleshooting. | `false` | Optional |

No repo-local replacement names should be introduced for those settings.

### Disabled mode

When `ATM_OTEL_ENABLED` is set to `false`, `0`, `off`, `no`, or `disabled`,
the facade performs no collector export attempt and exits successfully. This is
distinct from collector-failure fail-open behavior:

- disabled mode: no remote export is attempted and process exit remains `0`
- collector outage mode: remote export is attempted, failures are tolerated,
  and process exit remains `0`

## Facade API Reference

External consumers should use the neutral facade exported by
`crates/sc-observability/src/lib.rs`:

- `pub fn export_trace_records_best_effort(
    records: &[TraceRecord],
    config: &OtelConfig,
  )`
  Source: `crates/sc-observability/src/trace.rs`
- `pub fn export_metric_records_best_effort(
    records: &[MetricRecord],
    config: &OtelConfig,
  )`
  Source: `crates/sc-observability/src/metrics.rs`
- `pub fn current_otel_health(log_path: &Path) -> OtelHealthSnapshot`
  Source: `crates/sc-observability/src/health.rs`

These are the approved external-consumer facade entry points for best-effort
trace/metric export plus health inspection without importing transport-specific
code.

The following public helpers are internal/in-repo only and must not be treated
as external-consumer APIs:

- `pub fn export_otel_best_effort(
    event: &LogEventV1,
    config: &OtelConfig,
    exporter: &dyn OtelExporter,
  )`
- `pub fn export_otel_best_effort_from_path(
    log_path: &Path,
    event: &LogEventV1,
  )`

Those functions exist for internal sc-observability facade helpers and
entry-point wiring. External consumers must not call them directly.

## Versioning and Compatibility

The current contract is defined against:

- `sc-observability` version `0.46.0`
- Rust edition `2024`
- minimum supported Rust version `1.85`

Compatibility expectations:

- public facade APIs follow the repository semver policy
- additive configuration and attribute fields may be introduced in minor
  releases
- breaking facade or contract changes require a semver-major bump or an
  explicitly documented migration plan
- external consumers should pin `sc-observability` to a compatible semver range
  rather than copying internal ATM modules directly

## External Consumer Responsibilities

Every adopter repo must deliver:

1. Collector-backed smoke against its installed binary path.
2. Explicit outage/fail-open regression coverage.
3. A boundary check that blocks direct OTLP usage outside approved setup files.
4. Operator-visible OTel health for:
   - collector endpoint
   - collector state
   - local mirror state/path
   - last export error

## Validation Rule

The validator in `scripts/validate-external-consumer.sh` enforces three checks
for a target repo:

1. `sc-observability` dependency is present in at least one `Cargo.toml`.
2. `sc_observability_otlp` is not imported outside approved entry-point files.
3. `opentelemetry*` crates are not imported outside the dedicated transport
   layer.

Use that validator as a preflight gate before claiming external rollout
readiness.
