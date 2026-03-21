# ATM Daemon Reset Architecture

**Version**: 0.1
**Date**: 2026-03-20
**Status**: PLANNED

## 1. Target Architecture

The reset architecture is intentionally smaller than the current daemon model.

### 1.1 Roots

- **Config root**: stable team/config location, independent of `ATM_HOME`
- **Runtime root**: daemon socket, lock, status, logs, and other runtime files

`ATM_HOME` controls the runtime root only.

### 1.2 Ownership Model

- One system daemon per user account
- One lock-metadata file as the ownership source of truth
- One status snapshot as the health source of truth

No per-home shared-daemon arbitration. No runtime-kind-specific daemon
ownership model.

### 1.3 Startup Contract

The daemon startup sequence must be:

1. resolve config root and runtime root,
2. validate admission,
3. acquire canonical lock,
4. initialize the minimal required runtime endpoints,
5. publish lock/status only after readiness is established.

If startup fails before readiness, the daemon must leave no state that implies a
live daemon is available.

### 1.4 Shutdown Contract

Shutdown and restart must clean up the same runtime artifacts that startup owns.
The design must not require callers to infer daemon health from partial,
leftover files.

## 2. Explicit Deletion Targets

The reset architecture assumes removal or collapse of these current concepts:

- multi-daemon runtime kinds (`release`, `dev`, `isolated`) as daemon ownership
  modes
- config lookup under `ATM_HOME/.claude`
- `daemon-touch.json` as a competing ownership sidecar
- separate PID-file ownership as a required source of truth
- multi-home daemon identity replacement heuristics

## 3. Transitional Strategy

The reset should be implemented by introducing a smaller canonical path and then
deleting legacy behavior quickly:

1. add explicit config-root/runtime-root APIs,
2. move callers to the new APIs,
3. collapse runtime artifact set,
4. remove legacy multi-daemon logic,
5. rewrite tests to the single-daemon model,
6. delete the obsolete code and documentation.

## 4. Architectural Rule

Any daemon change during the reset must make the subsystem smaller, simpler, or
more deterministic. Changes that add branches, modes, runtime artifacts, or
compatibility layers are out of scope for the reset.
