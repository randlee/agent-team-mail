# Phase AZ Planning: AY Smoke Follow-Up

**Status**: PLANNED
**Prerequisites**:
- Phase AY complete and merged to `develop`
- AY smoke report available at `/tmp/ay_smoke_report.md`
- `develop` baseline includes the merged AY integration head used for the smoke run

## Goal

Close the two remaining AY smoke failures so OTel can be enabled for all teams
with a release-quality smoke path:

- daemon lifecycle traces must appear in Tempo under
  `resource.service.name="atm-daemon"` after a controlled restart
- `scripts/otel-dev-install-smoke.py` must validate the real shared-dev logging
  path instead of assuming `ATM_LOG_FILE` overrides still own the live install

## Sprint Map

| Sprint | Focus | Deliverable | Status |
|---|---|---|---|
| AZ.1 | Daemon trace attribution | restore fresh `atm-daemon` lifecycle traces in Tempo after restart and lock the smoke assertion to the real lifecycle signal | PLANNED |
| AZ.2 | Shared-dev smoke + OTel test coverage | fix `otel-dev-install-smoke.py` for shared-dev mode and close the missing OTel test-coverage gaps from `#904`, `#905`, and `#911` | PLANNED |

## AZ.1: Daemon Trace Attribution (`#918`)

### Smoke Failure

- AY smoke returned fresh traces for the smoke `session_id`, but only under
  `resource.service.name="atm"`
- the canonical dogfood query
  `resource.service.name="atm-daemon" && resource.session_id="<smoke-id>"`
  returned `0` traces after the daemon stop/start cycle

### Root Cause

The daemon lifecycle trace hook is installed too late in the daemon startup
sequence:

- [`crates/atm-daemon/src/main.rs:116`](../crates/atm-daemon/src/main.rs) calls
  `daemon::startup_auth::validate_startup_token(&home_dir)` before any lifecycle
  trace hook is installed
- [`crates/atm-daemon/src/daemon/startup_auth.rs:184`](../crates/atm-daemon/src/daemon/startup_auth.rs)
  emits lifecycle traces during startup via `export_lifecycle_trace(...)`
- [`crates/atm-daemon/src/daemon/startup_auth.rs:205`](../crates/atm-daemon/src/daemon/startup_auth.rs)
  builds a `LifecycleTraceRecord` with `source_binary = "atm-daemon"` and the
  current session id
- [`crates/atm-daemon/src/main.rs:449`](../crates/atm-daemon/src/main.rs)
  installs `install_lifecycle_trace_hook(...)` only after startup auth and
  runtime admission are already complete

Net effect:

- `launch_accepted` and other early startup lifecycle traces are constructed
  with the correct `atm-daemon` identity
- but they are emitted before the hook exists, so they never reach the OTLP
  exporter
- the AY smoke still sees CLI traces for `service.name="atm"` because those are
  emitted later through the CLI path, after the daemon is already running

### Minimal Fix

Move lifecycle trace hook installation earlier so startup-auth traces have a
live export path before `validate_startup_token()` runs:

1. Install `daemon::observability::install_lifecycle_trace_hook(...)` in
   [`crates/atm-daemon/src/main.rs`](../crates/atm-daemon/src/main.rs) before
   line `116`, immediately after `home_dir` is resolved.
2. Keep the hook target as
   `export_lifecycle_trace_from_entrypoint(...)`; it already preserves the
   daemon boundary by exporting through the entrypoint instead of direct daemon
   imports.
3. Leave the existing later trace/metric/log hook installation in place for the
   non-lifecycle paths, or refactor the hook registration block so lifecycle is
   installed first and only once.

This is the minimal safe change because `export_lifecycle_trace_from_entrypoint`
depends only on environment-derived `OtelConfig::from_env()` and does not need
the later runtime config resolution.

### File Targets

- [`crates/atm-daemon/src/main.rs:112-117`](../crates/atm-daemon/src/main.rs)
  — startup-auth runs before hook installation
- [`crates/atm-daemon/src/main.rs:449-451`](../crates/atm-daemon/src/main.rs)
  — existing late lifecycle hook installation site
- [`crates/atm-daemon/src/daemon/startup_auth.rs:184-255`](../crates/atm-daemon/src/daemon/startup_auth.rs)
  — lifecycle trace emission path that currently fires too early

### Verification

Add one targeted smoke assertion and one local regression test:

1. **Dogfood smoke assertion**
   - in [`scripts/dogfood-smoke.py`](../scripts/dogfood-smoke.py), keep the
     Tempo query keyed to
     `resource.service.name="atm-daemon" && resource.session_id="<session>"`
   - upgrade the success condition from “any trace exists” to “at least one
     trace exists after restart for `atm-daemon.lifecycle.launch_accepted` or a
     startup-lifecycle equivalent carrying the same `session_id`”
2. **Local regression**
   - add/extend a daemon integration test to start the daemon with OTLP enabled
     against a mock collector and assert that the first startup lifecycle export
     reaches `/v1/traces` with `service.name = "atm-daemon"` and the inherited
     session id

## AZ.2: Shared-Dev Smoke + OTel Test Coverage (`#919`, `#904`, `#905`, `#911`)

### Smoke Failure

`scripts/otel-dev-install-smoke.py` still assumes live shared-dev commands obey
command-local log overrides:

- [`scripts/otel-dev-install-smoke.py:160`](../scripts/otel-dev-install-smoke.py)
  sets `ATM_LOG_FILE = <temp>/atm.log.jsonl`
- [`scripts/otel-dev-install-smoke.py:161`](../scripts/otel-dev-install-smoke.py)
  sets `SC_COMPOSE_LOG_FILE = <temp>/sc-compose.log`
- [`scripts/otel-dev-install-smoke.py:178-184`](../scripts/otel-dev-install-smoke.py)
  fails if those temp files do not exist
- [`scripts/otel-dev-install-smoke.py:190-202`](../scripts/otel-dev-install-smoke.py)
  repeats the same assumption for the outage case

AY smoke proved the current shared-dev flow writes to the canonical shared
`ATM_HOME` log tree instead of honoring those temp-path overrides for the live
install.

### Minimal Fix

Teach `otel-dev-install-smoke.py` to distinguish shared-dev mode from isolated
collector-smoke mode:

1. Detect shared-dev mode from the canonical installed-bin path and/or the
   active `ATM_HOME` (`~/.local/share/atm-dev/home` in the current flow).
2. In shared-dev mode, derive canonical log paths from `ATM_HOME` instead of
   forcing `ATM_LOG_FILE` / `SC_COMPOSE_LOG_FILE`.
3. Continue to allow temp-path override mode for isolated/local collector tests
   where command-local log ownership is still the intended behavior.

### File Targets

- [`scripts/otel-dev-install-smoke.py:160-161`](../scripts/otel-dev-install-smoke.py)
  — live-mode temp-log override assumption
- [`scripts/otel-dev-install-smoke.py:178-184`](../scripts/otel-dev-install-smoke.py)
  — live-mode existence checks against temp paths
- [`scripts/otel-dev-install-smoke.py:190-202`](../scripts/otel-dev-install-smoke.py)
  — outage-mode temp-log override assumption and checks

### OTel Coverage Gaps

#### `#904`: shared daemon launch env-var coverage

The current daemon-launch inheritance test only asserts:

- `ATM_OTEL_ENABLED`
- `ATM_OTEL_ENDPOINT`

See [`crates/atm-daemon-launch/src/lib.rs:321-367`](../crates/atm-daemon-launch/src/lib.rs).

The missing `ATM_OTEL_*` coverage that should be added in the same test family:

- `ATM_OTEL_PROTOCOL`
- `ATM_OTEL_AUTH_HEADER`
- `ATM_OTEL_CA_FILE`
- `ATM_OTEL_INSECURE_SKIP_VERIFY`
- `ATM_OTEL_DEBUG_LOCAL_EXPORT`

`OtelConfig::from_env()` in
[`crates/sc-observability-types/src/lib.rs:42-99`](../crates/sc-observability-types/src/lib.rs)
already consumes a wider surface, so the shared-runtime launch test should stop
pretending only enable/endpoint matter.

#### `#905`: CLI error-path OTel export

Add an integration test alongside the existing CLI collector coverage in
[`crates/atm/tests/integration_otel_traces.rs`](../crates/atm/tests/integration_otel_traces.rs):

- run a CLI command that deterministically fails after startup
- collector captures `/v1/logs` and `/v1/traces`
- assert the command still emits:
  - a log event describing the error path
  - a trace/span with `service.name = "atm"`
  - `team`, `agent`, `runtime`, and `session_id`
  - error status rather than only success-path spans

This closes the current bias toward happy-path `status --json` coverage.

#### `#911`: serde round-trip coverage for health structs

The canonical types now live in
[`crates/atm-core/src/observability.rs`](../crates/atm-core/src/observability.rs):

- `OtelHealthSnapshot`
- `OtelLastError`

Add round-trip tests that:

- serialize then deserialize a fully populated `OtelHealthSnapshot`
- serialize then deserialize a default snapshot
- verify nested `OtelLastError` preserves `code`, `message`, and `at`

These can live in `atm-core` unit tests or a small dedicated observability test
module; the important part is that the schema used by `atm status --json` and
`atm doctor --json` is pinned by tests.

## Exit Criteria

1. Fresh daemon restart smoke returns at least one `atm-daemon` Tempo trace for
   the smoke session.
2. `scripts/otel-dev-install-smoke.py` passes against the real shared-dev flow
   without relying on temp-path log overrides.
3. Shared daemon launch tests cover the full connection/debug OTel env surface
   required for inherited shared-runtime export.
4. CLI error-path OTel export is covered by an integration test.
5. `OtelHealthSnapshot` / `OtelLastError` serde round-trip tests pin the JSON
   contract.
