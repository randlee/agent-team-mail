# Phase AY Planning: OTel Grafana Dogfood Readiness

**Status**: Planned
**Prerequisites**:
- Phase AV complete and merged
- Phase AW complete and merged
- `develop` baseline includes `ec94e2e1` (AW smoke follow-up merges, including
  the B.4 state-path fix and Loki/metric shaping work)
- Phase AS is independent of AY. AY does not depend on the GH governance/firewall
  work from AS and may proceed once the required `develop` baseline is present.

## Goal

Take the working AW-era OTel stack and close the live Grafana gaps uncovered by
real smoke testing so ATM can:

- query Loki by `service_name="atm"` for live CLI logs
- query Tempo for fresh `atm-daemon` traces after a controlled daemon start
- query Mimir using the real exported ATM metric names
- install and restart the shared dev daemon in a way that preserves OTel config
- begin live Grafana dogfooding with confidence

## Current Issue Triage Snapshot

### Already fixed on `develop` (not AY scope)

The triage sweep closed these as already resolved on `develop` and they should
not be re-planned inside AY:

- `#883`
- `#862`
- `#863`
- `#793`
- `#835`
- `#724`
- `#725`
- `#772`
- `#783`
- `#774`
- `#798`
- `#757`

### Parallel work already in flight

- `#888` / PR `#889` (`feature/cross-team-source-envelope`) is already being
  fixed separately. Treat it as a parallel fix expected to land on `develop`
  before AY.1 begins; do not duplicate that work in AY.

### Still-open smoke follow-up note

- `#886` (Loki `service_name`) remains pending until the live smoke re-run
  confirms the post-AW shaping fix on current `develop`.

## Smoke Findings That Drive AY

Confirmed from AW smoke and follow-up investigation:

- `B.4` was fixed by the merged budget-state path work and is now baseline, not
  new AY scope.
- Loki reads authenticated successfully once backend-specific instance IDs were
  used, but the smoke initially returned no `service_name="atm"` streams.
  Investigation showed that live Loki data had been landing under
  `service_name="unknown_service"` before the AW shaping fix.
- Tempo reads worked for CLI traces, but daemon traces were absent when the
  smoke attached to an already-running daemon that had started without the
  active `ATM_OTEL_*` env.
- Mimir reads authenticated successfully; smoke failures were driven by metric
  name/query mismatch rather than transport/auth failure.

AY turns those findings into a short execution plan.

## Scope

1. Prove the Loki `service_name` fix end-to-end on live Grafana data.
2. Make daemon-trace verification reliable by ensuring shared daemon startup
   inherits or resolves canonical OTel config.
3. Lock Grafana query recipes to the actual exported metric names and signal
   owners.
4. Get the dev-install/shared-daemon path into a state where live Grafana
   dogfooding is practical and repeatable.

## Non-Goals

- New signal families beyond AW
- Broad Grafana dashboard redesign
- External repo rollout (`scmux`, `schook`) beyond keeping the handoff docs
  accurate

## Sprint Map

| Sprint | Focus | Deliverables |
|---|---|---|
| AY.0 | Flaky test hardening | Parallel with AY.1. Small reliability fixes for already-known flaky tests uncovered during AW smoke/CI so AY implementation work stops paying incidental test fallout |
| AY.1 | Live signal correctness | Runs in parallel with AY.0. Verify Loki `service_name="atm"` on live data, verify daemon traces after a fresh OTel-configured start, align smoke/docs/scripts to backend-specific read auth and canonical metric names, and close any remaining signal-shaping gaps discovered during that verification |
| AY.2 | Shared dev-daemon dogfood readiness | Sequential after AY.1. Make canonical shared daemon/dev-install startup preserve OTel config, add an operator-safe dogfood smoke for live Grafana data, and document the exact install/start/query flow needed for ongoing dogfooding |
| AY.3a | OTel struct and operator-smoke cleanup | Small follow-up boundary cleanup: move mirror structs into `atm-core` and add the operator `otel-dev-install-smoke.py` script |
| AY.3b | OTel type/boundary extraction | Medium follow-up boundary work: create `sc-observability-types` and relocate `otlp_adapter` wiring to entry-point crates |

## AY.0: Flaky Test Hardening

### Purpose

Close small, pre-existing flaky tests that surfaced during AW smoke/CI so AY
implementation work can proceed without avoidable validation churn.

### Deliverables

- `#887`
  - change `TraceCollector::Drop` to use `let _ = join.join()` in
    `crates/sc-observability/tests/trace_export_integration.rs`
  - verify the serial fix for
    `log_event_exports_to_otlp_http_collector_with_service_name` in
    `crates/sc-observability/tests/log_export_integration.rs` landed via the
    `develop` hotfix (`#880`)
- `#871` / `#870`
  - fix the Tokio runtime flavor and panic isolation in
    `crates/atm-daemon/src/plugins/registry.rs`
- `#873`
  - add the missing `#[serial]` to
    `crates/atm-agent-mcp/tests/proxy_integration.rs`
- `#860`
  - replace the timed polling loop with a direct assertion in the janitor-reap
    test

### Notes

- AY.0 is intentionally small and can run in parallel with AY.1.
- These fixes are reliability-only and carry no architecture decision load.

## AY.1: Live Signal Correctness

### Problem

The AW stack exists, but live Grafana verification still had three practical
gaps:

- Loki success depended on `service.name` being shaped correctly and querying
  the right live window
- daemon traces depended on a fresh daemon process inheriting the active OTel
  config
- Mimir verification depended on the true exported metric names rather than
  guessed dashboard aliases

### Deliverables

- live Loki verification using the real query contract from
  `docs/observability/smoke-test-plan-phase-aw.md` Area C with:
  - session tag pattern: `ay-smoke-log-<unix-seconds>`
  - LogQL selector: `{service_name="atm"}`
  - query window: `start=now-10m`
  - curl pattern:
    `curl -s -G "$ATM_LOKI_ENDPOINT" -H "$ATM_LOKI_AUTH_HEADER" --data-urlencode 'query={service_name="atm"} | json | session_id="<tag>"' --data-urlencode "start=<now-10m nanos>"`
- live Tempo verification for daemon-owned traces after a controlled fresh
  daemon start, using the Area D model with:
  - session tag pattern: `ay-smoke-daemon-<unix-seconds>`
  - controlled lifecycle:
    `atm daemon stop || true`, export `ATM_OTEL_*`, run `atm send`/`atm read`,
    wait `20s`, then query Tempo
  - TraceQL selector:
    `{ resource.service.name = "atm-daemon" && session_id = "<tag>" && name =~ "atm-daemon.(dispatch_message|plugin..*)" }`
- live Mimir verification using the canonical exported metric names:
  - `atm_commands_count_total`
  - `atm_messages_sent_count_total`
  - `atm_daemon_request_count_total`
  - PromQL/curl pattern from Area D.4 in
    `docs/observability/smoke-test-plan-phase-aw.md`
- smoke/docs/script updates so:
  - per-backend read auth is explicit
  - daemon-trace smoke stops/restarts the daemon when required
  - metric queries use actual exported names

### Acceptance

- Loki returns at least one recent ATM log stream under `service_name="atm"`
  for an AY session tag in a `10m` window using the concrete LogQL/curl flow
  above
- Tempo returns at least one recent `atm-daemon` trace for the smoke session
  using the concrete TraceQL query above, after the smoke-controlled daemon
  stop/start sequence
- Mimir returns at least one of the canonical ATM metric series listed above
  using the documented PromQL query pattern
- all read-path smoke commands use backend-specific instance IDs and the shared
  read token correctly

## AY.2: Shared Dev-Daemon Dogfood Readiness

### Dependency

AY.1 must be complete and live Grafana signal correctness confirmed before
AY.2 begins.

### Problem

Even with correct signals, dogfooding is fragile if the shared dev daemon keeps
running without the intended OTel config or if `scripts/dev-install` does not
reliably start the daemon in the desired observability mode.

### Deliverables

- canonical launcher/shared daemon startup consumes the same OTel config surface
  used by the CLI/dev-install flow, with concrete work expected in:
  - `scripts/dev-install`
  - `crates/atm-daemon-launch/src/lib.rs`
  - any daemon-starting caller that must preserve the launcher contract
- `scripts/dev-install` and the shared dev-daemon restart path are verified as
  preserving OTel config for dogfood use, where “preserving OTel config” means
  a freshly restarted shared daemon process shows `ATM_OTEL_ENABLED=true`
  (and the expected endpoint/auth env) in its effective environment after the
  dev-install restart flow
- one operator-safe live Grafana dogfood smoke that extends
  `scripts/grafana-verify-smoke.py` during AY.2; AY.3a may later formalize the
  operator workflow into `scripts/otel-dev-install-smoke.py`
- the AY.2 dogfood smoke proves:
  - CLI logs visible in Loki
  - CLI and daemon traces visible in Tempo
  - canonical metrics visible in Mimir
  - local fail-open behavior preserved if the collector path breaks

### Acceptance

- a fresh shared dev daemon started through the canonical dev-install flow emits
  live Grafana telemetry without manual post-start patching
- the AY.2 dogfood smoke passes with at minimum:
  - one CLI log visible in Loki
  - one daemon trace visible in Tempo
  - one canonical metric visible in Mimir
- the dogfood smoke is documented and repeatable on `develop`
- live Grafana data is sufficient to begin routine dev-daemon dogfooding

## AY.3a: OTel Struct and Operator-Smoke Cleanup

### Deliverables

- `#852`
  - move `OtelHealthSnapshot` / `OtelLastError` mirror structs into
    `atm-core`
- `#878`
  - add `scripts/otel-dev-install-smoke.py` as the operator-oriented dev-install
    smoke script

### Dependency

- can begin after AY.2 is stable
- must land before AY.3b

## AY.3b: OTel Type and Boundary Extraction

### Deliverables

- `#876`
  - create `sc-observability-types` to break the
    `sc-observability` <-> `sc-observability-otlp` dependency cycle
- `#867`
  - relocate `otlp_adapter` wiring to entry-point crates after the type split

### Dependency

- depends on AY.3a

### Purpose

This is not required to begin dogfooding, but it is the right follow-on cleanup
once the live Grafana and dev-daemon path are stable.

### Merge-Gate Note

AY.3b is explicitly excluded from the AY merge gate. If `#876` / `#867` grow in
scope or threaten to delay AY dogfood readiness, they may be deferred to a
follow-on phase without blocking AY completion.

## Risks

- Grafana ingestion lag can still cause false-negative smoke runs if the query
  window is too narrow
- existing long-lived daemon processes can mask startup-config mistakes unless
  smoke explicitly controls lifecycle
- metric alias drift can reappear if dashboards and smoke scripts diverge from
  the canonical exported metric names
- AY.0 flake fixes may continue landing independently on `develop`; the plan
  must treat those as “verify landed” when applicable rather than duplicate
  work

## Exit Criteria

1. Live Loki queries show ATM logs under `service_name="atm"`.
2. Live Tempo queries show fresh `atm-daemon` traces from a controlled daemon
   start with OTel config.
3. Live Mimir queries use canonical metric names and return ATM data.
4. Shared dev-daemon install/start flow preserves OTel config well enough to
   begin Grafana-backed dogfooding.
5. Smoke docs/scripts match the real read-auth, query, and daemon-lifecycle
   contract.
