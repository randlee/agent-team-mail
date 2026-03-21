# Phase BB: Daemon Reset

**Status**: PLANNED
**Target branch**: `integrate/phase-BB`

## Goal

Remove multi-daemon support and replace the current daemon/runtime model with a
smaller, deterministic single-daemon design.

This phase is a deletion-heavy reset, not a feature phase.

## Why This Phase Exists

The current daemon model has accumulated:

- multi-daemon/runtime-mode branching,
- `ATM_HOME` coupling for both runtime and config,
- partial startup artifacts,
- asymmetric cleanup,
- hard-to-reason-about identity replacement logic,
- repeated reliability regressions across many follow-up phases.

Phase BB changes the strategy from patching symptoms to reducing the design.

## Sprint Plan

### BB.0 Dead Code Cleanup (Pre-Reset Trim)

Trim obviously dead code before the structural daemon reset starts.

Deliverables:

- remove unused `atm-core` schema/version scaffolding that never became part of
  the active runtime model
- remove dead/deprecated constructors and helpers that are provably uncalled
- reduce visibility on helpers that should be test-only or crate-private

Concrete candidates already identified:

- `crates/atm-core/src/schema/version.rs`
- `SystemContext.schema_version` and `with_schema_version()`
- deprecated `logging::init`
- `RetentionResult::new` / `CleanReportResult::new` visibility tightening

Acceptance:

- dead-code cleanup lands with no behavior change
- workspace validation remains green
- BB.1 and later sprints do not have to preserve these obsolete surfaces

Decision:

- `BB.0` is intentionally a pre-phase cleanup sprint because it removes code
  that no longer participates in any daemon-reset design choice.

### BB.1 Path Separation

Define and adopt separate APIs for:

- config root
- runtime root

Deliverables:

- team config/inbox paths stop resolving from `ATM_HOME`
- `ATM_HOME` becomes runtime-state-only
- daemon and CLI call sites migrate to the split path model

Acceptance:

- dev/shared runtime homes no longer break team config lookup
- `~/.claude/teams` (or the chosen stable config root) is independent of
  runtime-home overrides

### BB.2 Single-Daemon Model Collapse

Collapse daemon ownership to one system-daemon model.

Deliverables:

- remove multi-daemon runtime ownership support
- remove daemon runtime-kind arbitration for `release` vs `dev` shared daemons
- simplify admission/ownership checks around one daemon model

Acceptance:

- daemon ownership no longer depends on per-home competition
- runtime-kind branching is reduced to what is still required for tests only, or
  removed entirely

### BB.3 Artifact Collapse and Transactional Startup

Reduce daemon runtime state to the minimum required artifact set.

Deliverables:

- startup publishes canonical state only after readiness
- stop/restart cleanup removes all daemon-owned runtime artifacts symmetrically
- remove obsolete sidecars and redundant ownership files where possible
- remove daemon/plugin surfaces that are dead in production and only add
  maintenance load to the daemon reset

Primary deletion candidates:

- `atm-daemon.pid`
- `daemon-touch.json`
- unregistered bridge plugin module under `crates/atm-daemon/src/plugins/bridge/`
- dead plugin capabilities and registry helpers with no production callers
- superseded issue-provider legacy entrypoints

Acceptance:

- failed startup leaves no misleading live-daemon state
- stop/restart leaves no ambiguous stale-artifact combinations
- production-dead daemon plugin code is removed rather than carried through the
  reset

### BB.4 Test Model Rewrite and Final Deletion

Move daemon tests to the single-daemon model and delete the old complexity.

Deliverables:

- serialized/shared-fixture daemon test model
- obsolete multi-daemon tests removed
- dead code, env vars, docs, and fallback paths deleted

Acceptance:

- daemon tests no longer depend on isolated per-test daemon instances
- dogfood start/restart/stop passes repeatedly under the new model
- deleted code outweighs newly added code for the phase

## Exit Criteria

Phase BB is complete only when:

1. multi-daemon support is removed from the active requirements and
   implementation,
2. config root and runtime root are separate,
3. dead-code cleanup identified in `BB.0` is complete,
4. daemon startup/shutdown behavior is deterministic,
5. stale-artifact ambiguity is removed,
6. the daemon code and documentation are materially smaller than before the
   phase began.
