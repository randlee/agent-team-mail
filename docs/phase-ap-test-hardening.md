# Phase AP Test Hardening Sprint Notes

Goal: eliminate hang-prone, flaky, and operationally unsafe test patterns
without changing product behavior.

Prerequisites:
- Phase AN merged to `develop`.
- Phase AO may proceed in parallel, but AP.1 should start before new
  daemon-heavy test coverage expands.

Status review:
- The original report was generated against the AN.4 worktree, but a current
  spot-check on `develop`-equivalent code shows the core findings still exist.
- Most line numbers have drifted, but the substantive risks remain:
  process-wide `ATM_HOME` mutation, raw wall-clock sleeps, leaked subprocesses,
  hardcoded `/tmp` fixtures, and missing upper-bound assertions.
- One finding needs re-audit rather than blind carry-forward:
  `QA-021` (non-serial monitor tests) because at least one affected monitor test
  is already marked `#[serial]` on current code. Treat this as an audit item,
  not a guaranteed bug.

Deletion policy for this phase:
- AP should delete low-value duplicate tests and ad hoc test helpers rather than
  stabilizing every historical path.
- A flaky or hang-prone test must justify unique coverage; otherwise the default
  action is consolidation or deletion.
- When AP introduces a canonical timeout/cleanup helper, older parallel helpers
  should be removed.
- A test that is the sole or primary coverage for a normative requirement in
  `docs/requirements.md` or `docs/ci-monitoring/requirements.md` must be
  stabilized rather than deleted. Deletion is permitted only when an equivalent
  canonical test retains equivalent coverage.

## AP.1 Environment and Process Safety

Goal: remove the highest-risk sources of process leaks and shared-state
corruption.

Integration branch: `integrate/phase-AP`

Scope:
- Replace process-wide `ATM_HOME` mutation with scoped guards or explicit
  context injection.
- Add RAII teardown for all subprocess/daemon-launching tests before the first
  assertion can panic.
- Improve autostart daemon readiness diagnostics so failures name the failing
  resource instead of timing out opaquely.

Findings covered:
- `QA-001`, `QA-002`
- `QA-006`
- `QA-013`, `QA-014`, `QA-015`, `QA-016`
- `QA-024`

Acceptance:
- No test helper mutates `ATM_HOME` process-wide without scoped restoration in
  `Drop`
- Daemon/subprocess tests have teardown guards that kill and `wait()` even on
  panic
- Autostart readiness failures report actionable diagnostics instead of a bare
  timeout
- Targeted suites complete without leaving child daemons behind
- duplicate daemon/process test helpers that become unnecessary after the new
  guards land are deleted

Estimate:
- about 1 sprint

## AP.2 Deterministic Timing and Bounded Waits

Goal: remove raw sleeps and long polling loops as the primary synchronization
mechanism in async and daemon tests.

Integration branch: `integrate/phase-AP`

Scope:
- Replace wall-clock sleeps in CI-monitor, daemon lifecycle, and MCP transport
  tests with paused time, explicit notifications, or deterministic direct
  reconciliation calls.
- Add upper bounds and clearer failure output for polling-loop tests so CI
  cannot hang silently.
- Reduce watcher tests that stack multiple long `wait_until` loops.

Findings covered:
- `QA-003`, `QA-004`, `QA-005`
- `QA-007`
- `QA-010`, `QA-011`, `QA-012`
- `QA-017`, `QA-018`, `QA-019`, `QA-020`, `QA-022`

Acceptance:
- No blocking/high-priority test depends solely on a raw sleep for correctness
- Long-running loop tests have explicit upper bounds and timeout-aware failure
  messages
- Upper bounds are enforced with an affirmative assertion, e.g.
  `assert!(elapsed < Duration::from_secs(N), "test exceeded {}s: elapsed={:?}",
  N, elapsed)`, not only with a timeout wrapper
- Watcher/config reconciliation tests prefer deterministic direct calls over
  repeated polling where possible
- CI output makes the currently running risky integration test identifiable
- duplicate timing tests that do not provide unique coverage are consolidated or
  removed

Estimate:
- about 1 to 1.5 sprints

## AP.3 Pathing, Serialization, and Final Audit

Goal: finish the smaller cross-platform and harness hygiene items and re-audit
for remaining silent-hang patterns.

Integration branch: `integrate/phase-AP`

Scope:
- Replace hardcoded `/tmp` fixture paths with `std::env::temp_dir()`
- audit monitor/integration tests for missing `#[serial]` where shared runtime
  state still leaks through test helpers
- `#[serial]` is reserved for tests that still touch shared process-wide
  resources after RAII cleanup is applied; RAII isolates per-test lifecycle via
  `Drop`, but it cannot prevent concurrent access to the same Tokio runtime,
  daemon socket, or shared runtime directory
- Use RAII where the resource can be scoped per-test. Use `#[serial]` where the
  resource is inherently process-global and concurrent test threads would still
  race even after Drop-based cleanup.
- perform a final identify-only sweep for unbounded waits, leaked subprocesses,
  and poor attribution in the touched suites

Findings covered:
- `QA-008`, `QA-009`
- `QA-021`
- any residual medium findings left after AP.1/AP.2

Acceptance:
- No cross-platform test fixture hardcodes `/tmp`
- Shared-runtime-sensitive integration tests are either serialized or proven not
  to share mutable global state
- A final audit confirms no remaining blocking/high hang-prone patterns in the
  touched suites
- redundant tests and helper paths identified during AP.1/AP.2 are removed so
  the final harness is smaller than the pre-AP baseline

Estimate:
- about 0.5 sprint

## AP.4 Daemon-Spawn RAII Hardening

Goal: eliminate daemon process leaks by adopting all spawned or autostarted
test daemons into RAII guards and tightening post-signal cleanup semantics.

Integration branch: `integrate/phase-AP`

Branch: `feature/pAP-s4-daemon-spawn-hardening`

Scope:
- Adopt every spawned or autostarted test daemon in
  `integration_daemon_autostart.rs` into a guard that guarantees cleanup on
  panic or early assertion failure.
- Ensure daemon guards distinguish between owned direct children and adopted
  non-child PIDs.
- Keep the daemon test registry authoritative for owned spawns so stale sweeps
  can reap leaked children.
- Tighten runtime cleanup after signal-based shutdown to attempt a best-effort
  reap for direct children.

Findings covered:
- `DS-AP1-001` through `DS-AP1-005`
- `DS-AP2-004`
- `DS-AP2-006`

Acceptance:
- `integration_daemon_autostart.rs` adopts every spawned or autostarted daemon
  into a RAII guard
- `daemon_tests.rs` registers owned child daemon PIDs in
  `daemon_test_registry`
- `RuntimeDaemonCleanupGuard` performs best-effort `waitpid(WNOHANG)` cleanup
  after signal delivery for direct child processes
- Spec wording distinguishes owned direct children from adopted non-child PIDs:
  direct children should use `waitpid` after signal-based shutdown, while
  adopted non-child PIDs must use `kill(pid, 0)` polling because Unix
  `waitpid` on a non-child returns `ECHILD`
- zero rogue daemons remain after the test run

Estimate:
- about 0.5 sprint
