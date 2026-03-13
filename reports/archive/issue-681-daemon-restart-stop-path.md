---
issue: 681
title: "Daemon SIGTERM / Restart / Autostart Stop-Path Hardening"
date: 2026-03-12
worktree: fix/issue-681-sigterm-dogfood-verification
status: ready-to-implement
---

# Issue #681: Daemon Restart Stop-Path — Root Cause & Fix Blueprint

## Summary

The daemon's SIGTERM handler and CancellationToken propagation are sound. The bugs are
entirely in the **CLI-side restart path** (`crates/atm/src/commands/daemon.rs`).

## Identified Bugs

### H1: `execute_stop` leaves stale socket on ESRCH (correctness)
**Location**: `daemon.rs:282-283`

When daemon PID is gone (ESRCH), `execute_stop` removes the PID file but NOT the socket
file. The stale socket blocks the next daemon from binding.

**Fix**: Also remove socket file on ESRCH, same as PID file removal.

### H2: `execute_restart` exits(1) on stop timeout instead of escalating (correctness)
**Location**: `daemon.rs:305`

If `execute_stop` times out, it calls `std::process::exit(1)` — so `execute_restart`
never reaches the re-launch phase. For a restart operation, the CLI should escalate to
SIGKILL rather than giving up.

**Fix**: On stop timeout within `execute_restart` context, send SIGKILL, clean up runtime
files, then proceed to spawn the new daemon.

### H3: Fixed 500ms sleep in `execute_restart` is fragile (robustness)
**Location**: `daemon.rs:319`

After `execute_stop` confirms the PID is dead, a fixed 500ms sleep is used to wait for
socket/PID file cleanup. RAII cleanup runs synchronously before process exit, so files
should already be gone, but this is fragile under load.

**Fix**: Replace with a poll loop (e.g., 50ms interval, 2s max) checking for absence of
PID file and socket before spawning the new daemon.

### H4: No watchdog thread for graceful shutdown deadline (robustness)
**Location**: `crates/atm-daemon/src/main.rs:289-316`

If a plugin's `shutdown()` deadlocks, the daemon hangs indefinitely. #539 Layer 2
proposed a 30s watchdog thread.

**Fix**: After `cancel_token.cancel()`, spawn a `std::thread` that sleeps 30s then calls
`std::process::exit(1)`.

### H5: No double-signal immediate exit (robustness)
**Location**: `crates/atm-daemon/src/main.rs:289-316`

A second SIGTERM/SIGINT within 5s should trigger `std::process::exit(1)` immediately.
Currently the signal handler task completes after the first signal.

**Fix**: Track signal count in an `AtomicU32`; second signal within 5s → immediate exit.

### H6: Sequential plugin shutdown (performance, nice-to-have)
**Location**: `crates/atm-daemon/src/daemon/shutdown.rs:19`

`graceful_shutdown` shuts plugins sequentially — worst case `N × 5s`. A TODO comment
exists. Parallel shutdown bounds worst case to one 5s window.

## Coverage Gaps

| Gap | Test to Add |
|-----|-------------|
| GAP-1: `atm daemon stop` untested | `test_daemon_stop_removes_runtime_files` |
| GAP-2: `atm daemon restart` untested | `test_daemon_restart_produces_new_pid` |
| GAP-3: Stop timeout behavior | `test_daemon_stop_timeout_exits_nonzero` |
| GAP-4: Restart after SIGKILL | `test_restart_after_sigkill_cleans_stale_files` |
| GAP-5: ESRCH stale socket | `test_daemon_stop_cleans_socket_when_esrch` |

## Implementation Sequence

**Phase 1 — Critical fixes (correctness):**
- [ ] H1: Clean up socket file on ESRCH in `execute_stop` (`daemon.rs:282`)
- [ ] H2: `execute_restart` escalates to SIGKILL on stop timeout (`daemon.rs:305`)
- [ ] H3: Replace fixed 500ms sleep with poll loop for runtime file absence (`daemon.rs:319`)
- [ ] Add tests: GAP-1, GAP-2, GAP-5

**Phase 2 — Robustness hardening:**
- [ ] H4: 30s watchdog thread (`main.rs:289`)
- [ ] H5: Double-signal handler (`main.rs:289`)
- [ ] Add tests: GAP-3, GAP-4

**Phase 3 — Performance (optional):**
- [ ] H6: Parallel plugin shutdown (`shutdown.rs:19`)

## Issue #539 Interaction

4 layers proposed in ADR `docs/adr/issue-539-stale-daemon-fix-plan.md`:
- Layer 1 (startup orphan sweep): Partial — `cleanup_stale_daemon_runtime_files()` handles stale files but no pgrep scan
- Layer 2 (SIGTERM hardening): Not implemented — H4+H5 above
- Layer 3 (CLI+Doctor enrichment): Not implemented
- Layer 4 (test daemon isolation): Not implemented

H4+H5 complete Layer 2. Layers 3 and 4 are separate issues.
