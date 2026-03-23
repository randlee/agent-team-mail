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

1. `ATM-BB4-QA-003`: daemon plugin path migration incomplete at:
   - `crates/atm-daemon/src/plugins/ci_monitor/plugin.rs:348`
   - `crates/atm-daemon/src/plugins/issues/plugin.rs:317`
   - `crates/atm-daemon/src/plugins/worker_adapter/plugin.rs:241`
2. `ATM-BB4-QA-004`: `atm-tui` path-root migration incomplete at:
   - `crates/atm-tui/src/config.rs:135`
   - `crates/atm-tui/src/dashboard.rs:201`
   - `crates/atm-tui/src/main.rs:108`
   - `crates/atm-tui/src/main.rs:135`
   - `crates/atm-tui/src/main.rs:197`
   - `crates/atm-tui/src/main.rs:544`
   - `crates/atm-tui/src/main.rs:695`
3. `RUST-001`: `test_concurrent_sends_no_data_loss` tokio panic
4. `RUST-002`: `test_identity_mismatch_socket_is_detected_and_restarted` tokio panic
5. `DSQ-001`: `DaemonProcessGuard::spawn` launches test daemons with
   `LaunchClass::Shared` instead of `LaunchClass::IsolatedTest`
6. `QA-004`: multiteam isolation regressions:
   - `crates/atm/tests/integration_multiteam_isolation.rs::test_cli_team_scoped_commands_do_not_bleed_members_across_teams`
   - `crates/atm/tests/integration_multiteam_isolation.rs::test_status_and_members_preserve_registered_member_state_after_daemon_restart`

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

Implementation order is serial:

1. `CLEAN-1`
2. `CLEAN-2`
3. `CLEAN-3`

`CLEAN-1` and `CLEAN-2` both touch daemon readiness / `status.json` behavior.
`CLEAN-2` must not start until `CLEAN-1` stabilizes async-drop safety and
daemon availability semantics.

## Sprint Plan

### CLEAN-1 Hard Singleton and Test Harness Safety

Goal: guarantee that multiple daemon instances do not start and stabilize the
test harness around that model.

Scope:

- retain valid shared launch classes for product runtimes and retain
  `LaunchClass::IsolatedTest` for test runtimes; do not delete test isolation
  support in this closure phase
- correct `crates/atm/tests/support/daemon_process_guard.rs` so
  `DaemonProcessGuard::spawn` passes `LaunchClass::IsolatedTest` instead of
  `LaunchClass::Shared`
- make second daemon startup fail immediately and predictably
- fix `DSQ-001` so test daemons do not use shared runtime ownership
- fix the root cause behind:
  - `RUST-001`
  - `RUST-002`
  Root cause: tokio async-drop panic in `SocketServerHandle` drop during daemon
  teardown/readiness timing, which leaves tests waiting on incomplete readiness
  publication
- fix or explicitly triage the multiteam isolation regressions:
  - `test_cli_team_scoped_commands_do_not_bleed_members_across_teams`
  - `test_status_and_members_preserve_registered_member_state_after_daemon_restart`

Acceptance:

- product startup has one canonical daemon runtime path for shared daemon
  classes while explicitly retaining `ProdShared`, `DevShared`, and
  `LaunchClass::IsolatedTest` as valid launch classes
- second daemon start fails with a clear single-instance error
- `DaemonProcessGuard::spawn` uses `LaunchClass::IsolatedTest`
- daemon-spawn-qa gate passes for the `DaemonProcessGuard::spawn` launch-class
  fix
- `RUST-001` and `RUST-002` pass with async-drop safety addressed, not masked
- the two `integration_multiteam_isolation` failures either pass or are
  explicitly deferred with release sign-off and rationale

### CLEAN-2 Truthful State Surfaces

Goal: make daemon-backed state surfaces reliable enough to trust.

Scope:

- route `atm status`, `atm members`, and `atm doctor` through the shared
  team-scoped query surface
  `agent_team_mail_core::daemon_client::query_team_member_states()`
- introduce one shared daemon-availability contract in
  `atm-core/daemon_client` so command handlers stop inventing per-command socket
  checks; use a shared `DaemonAvailability` result instead of ad hoc
  “query failed so render empty state” logic
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
- concrete runnable scenario:
  1. start daemon
  2. emit a registered-member hook/session event so the master record contains
     `session_id` and liveness for that member
  3. verify `atm status`, `atm members`, and `atm doctor` surface that state
     through `query_team_member_states()`
  4. stop the daemon and verify the same commands report explicit
     daemon-unavailable state rather than normal-looking `unknown`

### CLEAN-3 Release Closure and Path Migration

Goal: close the remaining BB blockers that still affect release reliability.

Scope:

- fix daemon plugin path-root migration at the explicit BB.1 inventory sites:
  - `crates/atm-daemon/src/plugins/ci_monitor/plugin.rs:348`
  - `crates/atm-daemon/src/plugins/issues/plugin.rs:317`
  - `crates/atm-daemon/src/plugins/worker_adapter/plugin.rs:241`
  The fix shape is not `get_home_dir() -> get_os_home_dir()`. These paths must
  thread the runtime home from `PluginContext` instead of using the global
  `get_home_dir()` resolver.
- fix `atm-tui` path-root handling at:
  - `crates/atm-tui/src/config.rs:135`
  - `crates/atm-tui/src/dashboard.rs:201`
  - `crates/atm-tui/src/main.rs:108`
  - `crates/atm-tui/src/main.rs:135`
  - `crates/atm-tui/src/main.rs:197`
  - `crates/atm-tui/src/main.rs:544`
  - `crates/atm-tui/src/main.rs:695`
  Use explicit root ownership rather than a blanket resolver swap:
  config-facing paths use the config-root contract, runtime/watch/spool paths
  use explicit runtime-home ownership
- preserve the CLI usability fixes already landed in BB
- update docs/release notes to describe actual shipped behavior:
  - single shared daemon only
  - no support for ambiguous daemon-backed fallback behavior

Acceptance:

- QA blocking plugin path findings are closed at the 3 named plugin sites
- QA blocking `atm-tui` path findings are closed at the 7 named call sites
- plugin inbox/runtime paths use explicit runtime-home threading from
  `PluginContext` rather than ambient global resolver behavior
- `atm-tui` uses the intended split between config-root-owned paths and
  runtime-home-owned paths
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
6. `DaemonProcessGuard::spawn` launch-class behavior passes daemon-spawn-qa,
7. the two `integration_multiteam_isolation` tests pass or are explicitly
   signed off for deferral,
8. BB path-root blockers are resolved or explicitly signed off for deferral.

## Deferred to Next Phase

The following are explicitly deferred to the daemon-replacement phase:

- daemon redesign,
- plugin extraction,
- `ci-monitor` standalone daemonization,
- broader dependency-direction cleanup,
- full deletion of legacy socket/control surfaces.
