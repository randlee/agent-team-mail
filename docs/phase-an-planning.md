# Phase AN Planning — CI Monitor Extraction Readiness

## Goal

Prepare the CI monitor subsystem for clean extraction without repeating the
coupling problems that accumulated in `socket.rs` and `plugin.rs`.

Phase AN is not a "split files for their own sake" phase. Its purpose is to:
- define a stable CI-monitor core boundary
- keep daemon/plugin transport adapters inside `atm-daemon`
- remove daemon-only error and wire-type leakage from reusable CI-monitor code
- narrow the production surface so a later crate extraction is mechanical, not architectural

## Delivery Target

- Target version: post-`v0.44.8`
- Integration branch: `integrate/phase-AN`

## Inputs

- `docs/adr/phase-am-ci-monitor-extraction-review.md`
- `docs/requirements.md` plugin and CI monitor requirements
- `docs/ci-monitoring/architecture.md`
- `docs/adr/runtime-path-consistency-audit.md`
- `ARCH-003`: `plugin.rs` init path still uses `.unwrap()` in an extraction-sensitive path
- `ARCH-004`: `mod.rs` still exposes mocks/builders in the production surface

## Prerequisites

- Phase AL complete
- Phase AM merged to `develop` before AN.1 kickoff

## Phase Fit Decision

- Phase AM thinned `socket.rs` and separated the GH monitor router from the
  main daemon socket file.
- That work made the remaining coupling visible, but it did not make the
  subsystem extractable yet.
- Phase AN therefore focuses on extraction readiness, not immediate crate
  extraction.

## Core Boundary Decision

Phase AN uses a strict split between **CI-monitor core** and **daemon adapter**:

- CI-monitor core:
  - domain types
  - provider traits
  - provider registry
  - orchestration/service logic
  - provider-agnostic health and result shaping
- Daemon adapter:
  - plugin lifecycle
  - ATM roster/config/inbox integration
  - socket command routing
  - daemon task spawning/timers
  - daemon-specific health/state persistence

The daemon adapter stays in `atm-daemon` even after the core is clean enough
to extract into its own crate.

## Sprint 0 Cleanup

These are mandatory cleanup items before the main extraction track:

### AN.0a — ARCH-003 Init Error Propagation

**Branch**: `feature/pAN-s0a-init-propagation`

Replace `plugin.rs` init-path `.unwrap()` calls with explicit error
propagation/state transition handling.

Deliverables:
- no `.unwrap()` in CI-monitor plugin init paths that can be reached from
  runtime configuration/bootstrap
- init failures continue to surface as plugin state transitions, not panics

Acceptance:
- daemon startup with bad CI-monitor config degrades/disables the plugin
  without panic
- plugin init errors remain operator-visible in status/doctor surfaces

### AN.0b — ARCH-004 Surface Narrowing

**Branch**: `feature/pAN-s0b-surface-narrowing`

Move `MockCiProvider`, `MockCall`, `create_test_*`, and similar helpers behind
`#[cfg(test)]` or into test-only modules.

Deliverables:
- `mod.rs` exports only production interfaces/types
- test helpers are no longer visible from production builds

Acceptance:
- production `mod.rs` no longer re-exports test-only types
- tests compile without relying on production-surface leakage

## Sprint Sizing

| Sprint | Scope | Rough Size |
|---|---|---|
| AN.0a | Init error propagation (`ARCH-003`) | S |
| AN.0b | Surface narrowing (`ARCH-004`) | S |
| AN.1 | Domain types extraction boundary | M |
| AN.2 | Service split from `plugin.rs` | M |
| AN.3 | Trait injection and daemon decoupling | M |
| AN.4 | Production-surface narrowing | S/M |
| AN.5 | Transport adapter split | M |
| AN.6 | Crate extraction | M/L |

## Issue Tracking Note

GitHub issues for AN.0a-AN.6 may be filed at kickoff rather than before plan
approval, but each sprint must have a tracked issue before implementation
begins.

## Sprint Plan

### AN.1 — Domain Types Extraction

Move CI-monitor domain types to a stable boundary that does not depend on
daemon-only error or transport types.

Scope:
- extract/normalize shared CI monitor domain types from `types.rs`
- remove daemon/plugin-specific error types from type definitions
- stop using ATM daemon-client request/response structs as CI domain types
- define CI-domain request/response/error types where currently needed

Deliverables:
- domain types usable without `plugin.rs`
- provider/registry/service signatures no longer depend on daemon wire types
- explicit CI-domain errors replace `PluginError` leakage in reusable modules

Acceptance:
- `types.rs` and adjacent public type surfaces compile without importing
  `crate::plugin::PluginError`
- provider/registry/service APIs use CI-domain types only
- no behavior changes in existing CI-monitor tests

### AN.2 — Service Split

Decouple `CiMonitorService` from `plugin.rs` and other daemon-only context.

Scope:
- move orchestration logic that belongs to the service layer out of `plugin.rs`
- keep daemon lifecycle/bootstrap/state wiring in `plugin.rs`
- define a service input/output shape that does not require daemon plugin
  context types

Deliverables:
- `CiMonitorService` no longer depends on daemon plugin bootstrap details
- `plugin.rs` becomes a daemon adapter that delegates to the service layer

Acceptance:
- `service.rs` does not consume daemon-client wire structs
- `plugin.rs` focuses on wiring, lifecycle, and translation
- service tests can run without socket/plugin bootstrap scaffolding

### AN.3 — Trait Injection

Inject provider and registry dependencies through stable traits rather than
direct daemon-coupled implementations.

Scope:
- formalize provider and registry traits
- make service/orchestrator depend on trait objects or generics instead of
  daemon-bound concrete types
- keep trait boundaries narrow and purpose-specific

Deliverables:
- injectable provider boundary
- injectable registry boundary
- reduced direct coupling to daemon-owned state/config loaders

Acceptance:
- service layer can be tested with injected provider/registry implementations
- daemon adapter provides concrete implementations without widening the core API
- no direct plugin-context dependency remains in provider-facing service code

### AN.4 — `mod.rs` Narrowing

Finish narrowing the production module surface and remove accidental exports.

Scope:
- remove unconditional mock/test helper re-exports
- keep production exports intentional and minimal
- make public surface match the planned extraction boundary

Deliverables:
- `mod.rs` exports only production-facing CI-monitor modules and types
- test support lives in test-only modules or dedicated test-support files

Acceptance:
- production builds do not expose `MockCiProvider`, `MockCall`, or
  `create_test_*`
- module surface is small enough to serve as a future crate `lib.rs`
- AN.4 begins only after AN.3 lands; these sprints are intentionally serial
  because both reshape the CI-monitor module surface

### AN.5 — Transport Adapter

Make daemon transport/socket handling an explicit adapter layer instead of a
mixed part of the core subsystem.

Scope:
- keep `gh_monitor_router.rs` daemon-side
- define the minimal service-facing interface needed by transport code
- remove any remaining transport-specific logic from the core service modules

Deliverables:
- transport adapter boundary documented in code
- core service callable without socket/router types
- daemon-side routing remains in `atm-daemon`

Acceptance:
- no socket/router type appears in core service/provider/registry APIs
- `gh_monitor_router.rs` composes the core via adapter calls only
- CI-monitor socket tests remain green

### AN.6 — Crate Extraction

Extract the stabilized CI-monitor core into its own publishable crate.

Scope:
- create new crate for CI-monitor core
- move domain types, provider traits, registry, and service/orchestrator core
- keep daemon plugin/transport adapters in `atm-daemon`

Deliverables:
- new crate with stable production surface
- `atm-daemon` depends on that crate through a narrow adapter layer

Acceptance:
- extracted crate does not depend on daemon plugin bootstrap/socket code
- daemon adapter builds cleanly against the extracted core
- tests are split cleanly between crate-core tests and daemon-adapter tests

## Dependency Ordering

- AN.0 is required before AN.1 because Phase AN needs a clean production surface
  and panic-free init behavior first.
- AN.1 must land before AN.2 because the service split needs stable CI-domain
  types.
- AN.2 must land before AN.3 because traits should be introduced on top of the
  service boundary, not before it exists.
- AN.3 must land before AN.5 because the transport adapter can only be thin if
  service dependencies are already injectable.
- AN.4 is serial after AN.3; they must not run in parallel.
- AN.6 is last; crate extraction is a packaging step after boundaries stabilize.

## Hidden Coupling Risks

### 1. `plugin.rs` is still the main coupling hotspot

Current `plugin.rs` still mixes:
- plugin lifecycle
- repo/config resolution
- provider loading
- polling/task wiring
- health/state persistence decisions
- alert routing helpers

Phase AN must keep narrowing this file instead of simply moving that coupling
to a different module.

### 2. Transport is not the core

`gh_monitor_router.rs` is a daemon transport adapter. It should be made thin,
but it is not itself the reusable CI-monitor core.

### 3. Tests can hide boundary problems

If production modules keep re-exporting mocks/builders, extraction readiness
will look better in tests than it is in production.

## Acceptance Targets

1. No reusable CI-monitor module exposes daemon-only error or wire types.
2. `plugin.rs` is daemon-adapter glue, not the owner of CI-monitor business logic.
3. `gh_monitor_router.rs` remains daemon-side and consumes the core through a
   narrow interface.
4. Production module exports are minimal and test helpers are behind `#[cfg(test)]`.
5. Crate extraction in AN.6 is mechanical because the architectural boundary
   was already enforced in AN.1–AN.5.

## Recommended Worktree Sequence

1. `feature/pAN-s0a-init-propagation`
2. `feature/pAN-s0b-surface-narrowing`
3. `feature/pAN-s1-domain-types`
4. `feature/pAN-s2-service-split`
5. `feature/pAN-s3-trait-injection`
6. `feature/pAN-s4-mod-narrowing`
7. `feature/pAN-s5-transport-adapter`
8. `feature/pAN-s6-crate-extraction`

## Exit Criteria

- all AN sprint acceptance targets pass
- CI-monitor production surface is intentionally documented
- daemon adapter responsibilities are explicit
- new crate extraction no longer requires design decisions, only code motion
