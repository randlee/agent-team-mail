# ATM Daemon Reset Requirements

**Version**: 0.1
**Date**: 2026-03-20
**Status**: PLANNED

This document defines the simplified daemon requirements that supersede the
current multi-daemon/runtime-splitting model.

## 1. Design Goal

The daemon must be reduced to a manageable, reliable subsystem. The reset goal
is to eliminate multi-daemon support and remove any daemon behavior that does
not directly serve the remaining requirements.

## 2. Core Requirements

### 2.1 Single Daemon Only

- ATM supports at most one system daemon per user account.
- ATM must not support separate `release`, `dev`, and per-home shared daemons.
- ATM may still support serialized or shared-fixture daemon testing, but test
  support must not require multiple concurrent daemon instances.

### 2.2 Config Root and Runtime Root Are Separate

- Team configuration, inboxes, and team metadata must not be resolved from
  `ATM_HOME`.
- `ATM_HOME` is a runtime-state root only.
- Team configuration must resolve from one stable config root independent of
  daemon runtime state.

### 2.3 Minimal Daemon Responsibilities

The daemon may exist only for responsibilities that still require a background
process after the reset. At minimum, every daemon responsibility must answer:

1. why it cannot be synchronous,
2. why it cannot live in the CLI/process invoking it, and
3. why it justifies daemon complexity.

Any daemon behavior that cannot meet that bar is a deletion candidate.

### 2.4 Startup Must Be Transactional

- The daemon must not publish partial live-daemon state before it is ready to
  accept work.
- Startup either succeeds and publishes canonical runtime state, or fails and
  leaves no misleading live-daemon artifacts behind.

### 2.5 Shutdown and Restart Must Be Symmetric

- Stop/restart cleanup must remove all daemon-owned runtime artifacts together.
- Cleanup must not leave ambiguous combinations such as lock/status metadata
  without a live daemon.

### 2.6 Runtime Artifacts Must Be Minimal

The reset target is one ownership source of truth and one health source of
truth.

- Ownership artifact: lock metadata
- Health artifact: status snapshot

Any additional daemon runtime artifact must be justified as required. Existing
artifacts such as `atm-daemon.pid` and `daemon-touch.json` are deletion
candidates unless explicitly retained by the reset design.

### 2.7 No Multi-Daemon Arbitration

The daemon design must not require:

- runtime-kind arbitration,
- per-home daemon identity competition,
- mismatched-daemon replacement heuristics,
- broad stale-artifact inference to decide which daemon is authoritative.

The system must instead have one canonical daemon ownership model.

## 3. Test Model

- Daemon integration tests must work under a single-daemon design.
- Tests that require a real daemon must use serialized execution or a shared
  fixture model.
- The test strategy must not depend on isolated per-test daemon instances.

## 4. Deletion Requirements

The reset phase must explicitly classify daemon-related code into:

- keep and simplify,
- transitional,
- delete.

Refactoring that only redistributes code without reducing behaviors, modes,
artifacts, or branches does not satisfy the reset.

## 5. Exit Criteria

The daemon reset is complete only when all of the following are true:

1. Multi-daemon support is removed from the requirements and implementation.
2. Team config is no longer resolved from `ATM_HOME`.
3. Startup leaves no partial live-daemon state on failure.
4. Stop/restart cleanup removes all daemon-owned runtime artifacts
   deterministically.
5. Dogfood start/restart/stop passes repeatedly on a clean machine/home.
6. The remaining daemon code and docs are smaller, simpler, and easier to
   reason about than the current model.
