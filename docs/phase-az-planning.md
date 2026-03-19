# Phase AZ Planning: Smoke Test Fixes + Bug Hardening

**Status**: PLANNED
**Prerequisites**:
- Phase AY complete and merged to `develop` at `384bfd8c`
- latest AY smoke report artifact from the dogfood run is available for review
- `develop` baseline includes the merged AY integration head used for the smoke run

## Goal

Address the remaining Phase AY smoke failures, close the flaky-test backlog that
keeps generating CI noise, fill the missing OTel test coverage gaps, and add the
publish-order CI gate needed before broad OTel enablement:

- fix daemon trace attribution so `service.name="atm-daemon"` is discoverable in Tempo
- fix `otel-dev-install-smoke.py` so it validates the real shared-dev logging path
- eliminate the flaky test backlog that keeps destabilizing validation
- add missing OTel env-var, error-path, and serde-contract coverage
- land the easy production code cleanup and the CI publish-order gate

## Sprint Map

| Sprint | Focus | Issues | Gate |
|--------|-------|--------|------|
| AZ.1 | Daemon trace attribution (`service.name="atm-daemon"`) | `#918` | ✓ |
| AZ.2 | Smoke script + OTel test coverage | `#919`, `#904`, `#905`, `#911` | ✓ |
| AZ.3 | Flaky test hardening | `#899`, `#900`, `#901`, `#902`, `#914` | ✓ |
| AZ.4 | Easy prod fix + CI publish gate | `#917`, `#838` | ✓ |

All four sprints are gate sprints. `integrate/phase-AZ` does not close until
all four merge.

---

## AZ.1 — Daemon Trace Attribution

**Issue**: `#918`

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
- the smoke still sees CLI traces for `service.name="atm"` because those are
  emitted later through the CLI path, after the daemon is already running

### Minimal Code Change

Move lifecycle trace hook installation earlier so startup-auth traces have a
live export path before `validate_startup_token()` runs:

1. Install `daemon::observability::install_lifecycle_trace_hook(...)` in
   [`crates/atm-daemon/src/main.rs`](../crates/atm-daemon/src/main.rs) before
   line `116`, immediately after `home_dir` is resolved.
2. Keep the hook target as
   `export_lifecycle_trace_from_entrypoint(...)`; it already preserves the
   daemon boundary by exporting through the entrypoint instead of direct daemon
   imports.
3. Leave the later trace/metric/log hook installation in place for the
   non-lifecycle paths, or refactor the registration block so lifecycle is
   installed first and only once.

This is the minimal safe fix because `export_lifecycle_trace_from_entrypoint`
depends only on environment-derived `OtelConfig::from_env()` and does not need
the later runtime config resolution.

### File Targets

- [`crates/atm-daemon/src/main.rs:112-117`](../crates/atm-daemon/src/main.rs)
  — startup-auth runs before hook installation
- [`crates/atm-daemon/src/main.rs:449-451`](../crates/atm-daemon/src/main.rs)
  — existing late lifecycle hook installation site
- [`crates/atm-daemon/src/daemon/startup_auth.rs:184-255`](../crates/atm-daemon/src/daemon/startup_auth.rs)
  — lifecycle trace emission path that currently fires too early

### Smoke Assertion

After a controlled daemon stop/start cycle, the dogfood smoke must assert that:

- `resource.service.name="atm-daemon"` returns at least one trace for the smoke session
- and at least one returned trace corresponds to
  `atm-daemon.lifecycle.launch_accepted` or an equivalent startup lifecycle
  trace carrying the same `session_id`

### Local Regression Test

Add or extend a daemon integration test that:

- starts the daemon with OTLP enabled against a mock collector
- captures `/v1/traces`
- asserts the startup lifecycle export contains a span with:
  - `service.name = "atm-daemon"`
  - the inherited `session_id`

### Acceptance

- dogfood smoke returns `> 0` Tempo traces under
  `resource.service.name="atm-daemon"` for the smoke session
- the new local regression test passes
- `cargo test --workspace` passes
- `cargo clippy --all-targets -- -D warnings` passes

---

## AZ.2 — Smoke Script + OTel Test Coverage

**Issues**: `#919`, `#904`, `#905`, `#911`

### Problem

- `#919`: `otel-dev-install-smoke.py` sets `ATM_LOG_FILE=<temp>` and
  `SC_COMPOSE_LOG_FILE=<temp>`, but in the current shared-dev flow the daemon
  writes logs to the canonical resolved `ATM_HOME` tree instead
- `#904`: the daemon-launch OTel env-var test covers only two inherited
  variables even though the transport config surface is larger
- `#905`: CLI error-path OTel export has no integration coverage
- `#911`: `OtelHealthSnapshot` / `OtelLastError` have no serde round-trip tests

### Root Cause For `#919`

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

### Minimal Diff For `#919`

Teach `otel-dev-install-smoke.py` to distinguish shared-dev mode from
isolated/local collector smoke:

1. Detect shared-dev mode from the canonical installed-bin path and/or the
   resolved shared runtime home rather than hardcoding
   `~/.local/share/atm-dev/home`.
2. In shared-dev mode, derive the canonical log paths from the resolved
   `ATM_HOME`.
3. Keep temp-path override mode for isolated/local collector tests where
   command-local log ownership is still the intended behavior.

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

- run a CLI command that deterministically exits non-zero after startup
- collector captures `/v1/logs` and `/v1/traces`
- assert the failing invocation still emits:
  - a log event describing the error path
  - a trace/span with `service.name = "atm"`
  - `team`, `agent`, `runtime`, and `session_id`
  - error status rather than success-only spans

#### `#911`: serde round-trip coverage for health structs

The canonical types now live in
[`crates/atm-core/src/observability.rs`](../crates/atm-core/src/observability.rs):

- `OtelHealthSnapshot`
- `OtelLastError`

Add round-trip tests that:

- serialize then deserialize a fully populated `OtelHealthSnapshot`
- serialize then deserialize a default snapshot
- verify nested `OtelLastError` preserves `code`, `message`, and `at`

### Acceptance

- `python3 scripts/otel-dev-install-smoke.py` exits `0` in shared-dev mode
- all five inherited OTel variables listed above are covered by the
  daemon-launch test
- the new CLI error-path OTel export integration test passes
- the serde round-trip tests pass
- `cargo test --workspace` passes

---

## AZ.3 — Flaky Test Hardening

**Issues**: `#899`, `#900`, `#901`, `#902`, `#914`

### Problem

Multiple flaky tests are still creating CI noise:

- `#899`: `proxy_integration` has incomplete `#[serial]` coverage for shared
  socket/port state
- `#900`: `proxy_integration` uses an `elapsed < 1s` liveness assertion that is
  too tight for loaded CI
- `#901`: `startup_auth` tests depend on the real home-dir path and fail in
  sandboxed CI layouts
- `#902`: `daemon_tests` use a polling timeout that is too tight for loaded CI
- `#914`: `sc-observability` still has cross-binary env-var bleed risks because
  `#[serial]` does not isolate process-global state across test binaries

### Deliverables

- `#899`: add missing `#[serial]` to all `proxy_integration` tests that share
  socket or port state
- `#900`: raise the `elapsed < 1s` assertion to a CI-safe bound such as
  `Duration::from_secs(10)`
- `#901`: replace real home-dir references in `startup_auth` tests with temp dirs
- `#902`: raise daemon polling timeout to tolerate loaded CI
- `#914`: document and mitigate cross-binary env-var bleed; add `#[serial]`
  where needed and reset `OnceLock`-backed globals in test teardown if the API
  allows it

### Acceptance

- all five issue clusters pass reliably across three consecutive
  `cargo test --workspace` runs
- no new `#[serial]` regressions are introduced
- `cargo clippy --all-targets -- -D warnings` passes

---

## AZ.4 — Easy Prod Fix + CI Publish Gate

**Issues**: `#917`, `#838`

### Deliverables

`#917`: replace bare `.unwrap()` on `Mutex` operations in `startup_auth.rs`
production code with `.expect("startup_auth mutex poisoned")` so production
panic paths carry real context.

`#838`: add a CI gate to publish-manifest validation so `publish_order` must
match the crate dependency graph. A crate must have a lower `publish_order` than
all crates that depend on it. The gate must fail with a diff showing the
violation.

### Acceptance

- `startup_auth.rs` has no bare `.unwrap()` on `Mutex` operations in production code
- the CI gate exists, is wired into CI, and rejects a manifest with inverted
  dependency order
- `cargo test --workspace` passes
- `cargo clippy --all-targets -- -D warnings` passes

---

## Exit Criteria

1. Shared-daemon restart smoke returns at least one Tempo trace under
   `resource.service.name="atm-daemon"` for the smoke session.
2. `scripts/otel-dev-install-smoke.py` passes against the real shared-dev flow
   without relying on temp-path log overrides.
3. Shared daemon-launch tests explicitly cover
   `ATM_OTEL_PROTOCOL`, `ATM_OTEL_AUTH_HEADER`, `ATM_OTEL_CA_FILE`,
   `ATM_OTEL_INSECURE_SKIP_VERIFY`, and `ATM_OTEL_DEBUG_LOCAL_EXPORT`.
4. CLI error-path OTel export is covered by an integration test.
5. `OtelHealthSnapshot` / `OtelLastError` serde round-trip tests pin the JSON
   contract.
6. AZ.1 local regression test passes: daemon integration test with mock OTLP
   collector asserts `/v1/traces` contains a span with
   `service.name="atm-daemon"` and inherited `session_id` after daemon startup.

---

## Integration Branch

`integrate/phase-AZ` off the current `develop` baseline.

Sprint PRs target `integrate/phase-AZ`. Final PR: `integrate/phase-AZ → develop`.
