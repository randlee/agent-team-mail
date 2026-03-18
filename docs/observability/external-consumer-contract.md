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

- `ATM_OTEL_ENABLED`
- `ATM_OTEL_ENDPOINT`
- `ATM_OTEL_PROTOCOL`
- `ATM_OTEL_AUTH_HEADER`
- `ATM_OTEL_CA_FILE`
- `ATM_OTEL_INSECURE_SKIP_VERIFY`
- `ATM_OTEL_TIMEOUT_MS`
- `ATM_OTEL_RETRY_MAX_ATTEMPTS`
- `ATM_OTEL_RETRY_BACKOFF_MS`
- `ATM_OTEL_RETRY_MAX_BACKOFF_MS`
- `ATM_OTEL_DEBUG_LOCAL_EXPORT`

No repo-local replacement names should be introduced for those settings.

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
