# Phase AY Planning: OTel Grafana Dogfood Readiness

**Status**: Planned
**Prerequisites**:
- Phase AV complete and merged
- Phase AW complete and merged
- `develop` baseline includes `ec94e2e1` (AW smoke follow-up merges, including
  the B.4 state-path fix and Loki/metric shaping work)

## Goal

Take the working AW-era OTel stack and close the live Grafana gaps uncovered by
real smoke testing so ATM can:

- query Loki by `service_name="atm"` for live CLI logs
- query Tempo for fresh `atm-daemon` traces after a controlled daemon start
- query Mimir using the real exported ATM metric names
- install and restart the shared dev daemon in a way that preserves OTel config
- begin live Grafana dogfooding with confidence

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
| AY.1 | Live signal correctness | Verify Loki `service_name="atm"` on live data, verify daemon traces after a fresh OTel-configured start, align smoke/docs/scripts to backend-specific read auth and canonical metric names, and close any remaining signal-shaping gaps discovered during that verification |
| AY.2 | Shared dev-daemon dogfood readiness | Make canonical shared daemon/dev-install startup preserve OTel config, add an operator-safe dogfood smoke for live Grafana data, and document the exact install/start/query flow needed for ongoing dogfooding |

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

- live Loki verification using the real query contract:
  - `service_name="atm"`
  - live session correlation fields present
- live Tempo verification for daemon-owned traces after a controlled fresh
  daemon start
- live Mimir verification using the canonical exported metric names
- smoke/docs/script updates so:
  - per-backend read auth is explicit
  - daemon-trace smoke stops/restarts the daemon when required
  - metric queries use actual exported names

### Acceptance

- Loki returns at least one recent ATM log stream under `service_name="atm"`
- Tempo returns at least one recent `atm-daemon` trace for the smoke session
- Mimir returns the canonical ATM metric series used by the smoke
- all read-path smoke commands use backend-specific instance IDs and the shared
  read token correctly

## AY.2: Shared Dev-Daemon Dogfood Readiness

### Problem

Even with correct signals, dogfooding is fragile if the shared dev daemon keeps
running without the intended OTel config or if `scripts/dev-install` does not
reliably start the daemon in the desired observability mode.

### Deliverables

- canonical launcher/shared daemon startup consumes the same OTel config surface
  used by the CLI/dev-install flow
- `scripts/dev-install` and the shared dev-daemon restart path are verified as
  preserving OTel config for dogfood use
- one operator-safe live Grafana dogfood smoke that proves:
  - CLI logs visible in Loki
  - CLI and daemon traces visible in Tempo
  - canonical metrics visible in Mimir
  - local fail-open behavior preserved if the collector path breaks

### Acceptance

- a fresh shared dev daemon started through the canonical dev-install flow emits
  live Grafana telemetry without manual post-start patching
- the dogfood smoke is documented and repeatable on `develop`
- live Grafana data is sufficient to begin routine dev-daemon dogfooding

## Risks

- Grafana ingestion lag can still cause false-negative smoke runs if the query
  window is too narrow
- existing long-lived daemon processes can mask startup-config mistakes unless
  smoke explicitly controls lifecycle
- metric alias drift can reappear if dashboards and smoke scripts diverge from
  the canonical exported metric names

## Exit Criteria

1. Live Loki queries show ATM logs under `service_name="atm"`.
2. Live Tempo queries show fresh `atm-daemon` traces from a controlled daemon
   start with OTel config.
3. Live Mimir queries use canonical metric names and return ATM data.
4. Shared dev-daemon install/start flow preserves OTel config well enough to
   begin Grafana-backed dogfooding.
5. Smoke docs/scripts match the real read-auth, query, and daemon-lifecycle
   contract.
