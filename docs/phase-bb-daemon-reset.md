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

Primary deletion candidates:

- `atm-daemon.pid`
- `daemon-touch.json`

Acceptance:

- failed startup leaves no misleading live-daemon state
- stop/restart leaves no ambiguous stale-artifact combinations

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
3. daemon startup/shutdown behavior is deterministic,
4. stale-artifact ambiguity is removed,
5. the daemon code and documentation are materially smaller than before the
   phase began.
