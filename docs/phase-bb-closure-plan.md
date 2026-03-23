# Phase BB Closure Plan

**Date**: 2026-03-22
**Status**: PLANNING
**Worktree**: `planning/phase-bb-closure`
**Base branch**: `develop` at `903367d2`

## Purpose

Close Phase BB with the minimum set of changes required to make ATM reliable
enough to release.

This plan is intentionally narrower than the original Phase BB reset plan. It
does **not** attempt to redesign the daemon in-place. It only closes the
highest-risk release issues and leaves daemon replacement for the next phase.

## Release Objective

Phase BB closure succeeds only if:

1. multiple daemon instances cannot start in normal product operation,
2. daemon-backed state is either correct or explicitly unavailable,
3. the known BB end-of-phase blocking findings are resolved or deliberately
   deferred with explicit release sign-off,
4. the existing CLI usability fixes remain intact.

## Non-Goals

The following are out of scope for BB closure:

- redesigning the daemon architecture,
- extracting `ci-monitor` into a separate daemon or repo,
- adding new daemon features,
- broad plugin cleanup beyond blocking release issues,
- introducing new fallback paths.

## Confirmed End-of-Phase Findings

Confirmed blocking findings from the BB end-of-phase review and QA consolidation:

1. `ATM-BB4-QA-003`: daemon plugin path migration incomplete
2. `ATM-BB4-QA-004`: `atm-tui` path-root migration incomplete
3. `RUST-001`: `test_concurrent_sends_no_data_loss` tokio panic
4. `RUST-002`: `test_identity_mismatch_socket_is_detected_and_restarted` tokio panic
5. `DSQ-001`: test daemons launched with `LaunchClass::Shared` instead of `LaunchClass::IsolatedTest`

Important but not blocking:

1. `ATM-BB4-QA-002`: `mail_inject.rs` still uses `get_home_dir()` for inbox/state
   paths
2. daemon-backed state surfaces (`status`, `members`, `doctor`) currently
   degrade too easily into `unknown` / empty-state presentation

## Closure Strategy

The closure strategy is:

1. enforce one shared daemon path,
2. make daemon-backed state truthful instead of silently degraded,
3. close the remaining blocking path/test regressions,
4. avoid any work that deepens daemon scope.

## Sprint Plan

### CLEAN-1 Hard Singleton and Test Harness Safety

Goal: guarantee that multiple daemon instances do not start and stabilize the
test harness around that model.

Scope:

- remove or disable remaining alternate daemon launch/runtime paths that permit
  multiple daemon instances in product behavior
- make second daemon startup fail immediately and predictably
- fix `DSQ-001` so test daemons do not use shared runtime ownership
- fix the readiness/runtime panic path behind:
  - `RUST-001`
  - `RUST-002`

Acceptance:

- product startup has one canonical daemon runtime path
- second daemon start fails with a clear single-instance error
- test daemon helpers do not compete for shared runtime ownership
- `RUST-001` and `RUST-002` pass

### CLEAN-2 Truthful State Surfaces

Goal: make daemon-backed state surfaces reliable enough to trust.

Scope:

- stop `atm status`, `atm members`, and `atm doctor` from silently flattening
  daemon query failure into misleading empty/unknown output
- ensure hook/session-derived master state is what these surfaces render
- if authoritative daemon state is unavailable, show an explicit
  daemon-unavailable condition rather than synthetic ambiguity
- keep core file-based mail commands unchanged

Acceptance:

- `status`, `members`, and `doctor` no longer silently present missing daemon
  state as normal output
- when hook/session state exists in the master record, it is visible in the
  rendered state surfaces
- when daemon-backed authority is unavailable, the user sees an explicit error
  or degraded-state marker with provenance

### CLEAN-3 Release Closure and Path Migration

Goal: close the remaining BB blockers that still affect release reliability.

Scope:

- fix `ATM-BB4-QA-003` daemon plugin path-root migration sites
- fix `ATM-BB4-QA-004` `atm-tui` path-root migration sites
- preserve the CLI usability fixes already landed in BB
- update docs/release notes to describe actual shipped behavior:
  - single shared daemon only
  - no support for ambiguous daemon-backed fallback behavior

Acceptance:

- QA blocking path-root findings are closed
- `atm-tui` uses the intended root split for config/team-state paths
- release docs match shipped daemon behavior

## Worktree Split

If approved, create one feature worktree per closure sprint:

1. `feature/pBB-clean-1-singleton`
2. `feature/pBB-clean-2-state-truth`
3. `feature/pBB-clean-3-release-closure`

## Release Gate

BB closure is release-ready only if all of the following are true:

1. first daemon start succeeds,
2. second daemon start fails every time,
3. stop/restart does not leave stale ownership that permits duplicate startup,
4. `status`, `members`, and `doctor` do not silently hide daemon failure behind
   normal-looking `unknown` output,
5. `RUST-001`, `RUST-002`, and targeted daemon-start tests pass,
6. BB path-root blockers are resolved or explicitly signed off for deferral.

## Deferred to Next Phase

The following are explicitly deferred to the daemon-replacement phase:

- daemon redesign,
- plugin extraction,
- `ci-monitor` standalone daemonization,
- broader dependency-direction cleanup,
- full deletion of legacy socket/control surfaces.
