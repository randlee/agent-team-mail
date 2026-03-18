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

## Scope

1. Prove the Loki `service_name` fix end-to-end on live Grafana data.
2. Make daemon-trace verification reliable by ensuring shared daemon startup
   inherits or resolves canonical OTel config.
3. Lock Grafana query recipes to the actual exported metric names and signal
   owners.
4. Get the dev-install/shared-daemon path into a state where live Grafana
   dogfooding is practical and repeatable.

## Sprint Map

| Sprint | Focus | Deliverables |
|---|---|---|
| AY.0 | Flaky test hardening | Parallel with AY.1. Small reliability fixes for already-known flaky tests uncovered during AW smoke/CI so AY implementation work stops paying incidental test fallout |
| AY.1 | Live signal correctness | Runs in parallel with AY.0. Verify Loki `service_name=\"atm\"` on live data, verify daemon traces after a fresh OTel-configured start, align smoke/docs/scripts to backend-specific read auth and canonical metric names, and close any remaining signal-shaping gaps discovered during that verification |
| AY.2 | Shared dev-daemon dogfood readiness | Sequential after AY.1. Make canonical shared daemon/dev-install startup preserve OTel config, add an operator-safe dogfood smoke for live Grafana data, and document the exact install/start/query flow needed for ongoing dogfooding |
| AY.3a | OTel struct and operator-smoke cleanup | Small follow-up boundary cleanup: move mirror structs into `atm-core` and add the operator `otel-dev-install-smoke.py` script |
| AY.3b | OTel type/boundary extraction | Medium follow-up boundary work: create `sc-observability-types` and relocate `otlp_adapter` wiring to entry-point crates |
| AY.4 | Spool/inbox reliability | Verify and fix spool filename collision risk, merged-write durability, and spool cleanup diagnostics |

## AY.1 Acceptance

- Loki returns at least one recent ATM log stream under `service_name="atm"`
  for an AY session tag in a `10m` window using the documented LogQL/curl flow.
- Tempo returns at least one recent `atm-daemon` trace after the smoke-controlled
  daemon stop/start sequence.
- Mimir returns at least one canonical ATM metric series using the documented
  PromQL query pattern.
- All read-path smoke commands use backend-specific instance IDs and the shared
  read token correctly.

## AY.2 Acceptance

- A fresh shared dev daemon started through the canonical dev-install flow emits
  live Grafana telemetry without manual post-start patching.
- The live Grafana dogfood smoke passes with:
  - one CLI log visible in Loki
  - one daemon trace visible in Tempo
  - one canonical metric visible in Mimir
- The dogfood smoke is documented and repeatable on `develop`.
