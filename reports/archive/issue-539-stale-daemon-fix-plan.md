# Issue #539 Fix Plan: Stale Daemon Process Accumulation

Date: 2026-03-10
Status: planned
Issue: https://github.com/randlee/agent-team-mail/issues/539

## Root Causes

1. **Test ATM_HOME isolation** — each test uses `ATM_HOME=/tmp/<random>`, so its `daemon.lock`
   is invisible to the production daemon. Crashed tests leave orphan processes with no discoverer.
2. **Drop-only cleanup** — `SocketServerHandle::drop()` and `FileLock::drop()` are the only
   cleanup paths. SIGKILL / OOM bypass them entirely.
3. **No global PID registry** — daemon only knows its own `ATM_HOME`-scoped PID file. No
   system-wide registry for `atm daemon status` or `atm doctor` to scan.
4. **Stale version survives config changes** — `ensure_daemon_running_unix()` checks mismatch
   only when a CLI command fires. Silent between sessions.

## Relevant Code

- `crates/atm-daemon/src/main.rs:70` — lock acquisition via `fs2::FileExt::try_lock_exclusive()`
- `crates/atm-core/src/io/lock.rs:10-20` — `FileLock` struct (Drop-based release)
- `crates/atm-daemon/src/daemon/socket.rs:315-316` — PID file write
- `crates/atm-daemon/src/daemon/socket.rs:148-159` — `SocketServerHandle` (Drop-based cleanup)
- `crates/atm-daemon/src/main.rs:312-339` — SIGTERM/SIGINT signal handler
- `crates/atm-core/src/daemon_client.rs:402-430` — `write_daemon_lock_metadata()`
- `crates/atm-core/src/daemon_client.rs:1664-1685` — stale cleanup at autostart time
- `crates/atm-core/src/daemon_client.rs:1718-1820` — `detect_daemon_identity_mismatch()`

## Solution Architecture: 4-Layer Defense

### Layer 1 — Startup Orphan Sweep

**Files**: `crates/atm-core/src/daemon_client.rs`, `crates/atm-daemon/src/main.rs`

New functions in `daemon_client.rs`:
- `scan_atm_daemon_processes() -> Vec<(u32, String)>` — platform-conditional:
  - Unix: `pgrep -u $(whoami) -f atm-daemon`
  - Windows: `tasklist /FI "IMAGENAME eq atm-daemon.exe" /FO CSV /NH`
- `is_pid_atm_daemon(pid) -> bool` — verify process name before kill (prevents PID reuse kills)
- `kill_orphan_daemons_for_home(home: &Path)` — kill scanned PIDs sharing same ATM_HOME (not self)

Call in `main.rs` after `write_daemon_lock_metadata()`, before logging init:
```rust
agent_team_mail_core::daemon_client::kill_orphan_daemons_for_home(&home_dir);
```

Best-effort: errors logged at `warn`, never propagated as startup failures.

### Layer 2 — SIGTERM Handling Hardening

**File**: `crates/atm-daemon/src/main.rs` (replace lines 312-339)

Changes:
1. **Watchdog thread**: after `cancel_token.cancel()`, spawn a `std::thread` that sleeps 30s
   then calls `std::process::exit(1)`. Ensures hard exit if graceful shutdown deadlocks.
2. **Double-signal exit**: second SIGTERM or SIGINT within 5s triggers `std::process::exit(1)`
   immediately.

### Layer 3 — CLI + Doctor Enrichment

**Files**: `crates/atm/src/commands/daemon.rs`, `crates/atm/src/commands/doctor.rs`

- `atm daemon status --sweep`: scan all user-owned atm-daemon PIDs, show table
  (PID / ATM_HOME / VERSION / STATUS), prompt to kill orphans (`--force` skips prompt)
- `atm doctor` new finding: `STALE_DAEMON_PROCESSES` (WARN severity) when orphans detected
- New public function: `scan_all_user_daemon_pids()` in `daemon_client.rs`

### Layer 4 — Test Daemon Isolation Hardening

**File**: `crates/atm-core/src/daemon_client.rs`

Global TMPDIR registry: `$TMPDIR/atm-daemon-test-pids.jsonl`
- Each line: `{"pid": N, "atm_home": "/tmp/xxx", "started_at": "..."}`
- `register_test_daemon_pid(pid: u32, atm_home: &Path)` — append to registry (no lock needed for JSONL append)
- `cleanup_stale_test_daemons()` — read registry, check liveness (`kill -0`), kill orphans, rewrite with alive-only entries
- Called at daemon startup (best-effort) and via `atm daemon status --sweep`

## Data Flow

### Production Startup
```
atm-daemon main()
  |-> acquire_lock(daemon.lock)
  |-> write_daemon_lock_metadata()        # PID, exe, home_scope, version
  |-> kill_orphan_daemons_for_home()      # Scan + kill orphans with same ATM_HOME
  |-> cleanup_stale_test_daemons()        # Scan TMPDIR registry
  |-> init logging, config, plugins
  |-> start_socket_server()               # Write PID file, bind socket
  |-> run event loop
  |-> [SIGTERM] -> cancel() + 30s watchdog + double-signal exit
  |-> graceful_shutdown()
  |-> SocketServerHandle::drop()          # Remove socket + PID files
  |-> FileLock::drop()                    # Release flock
```

### Crash Recovery
```
[daemon crashes / SIGKILL / OOM]
  |-> flock released by OS
  |-> PID file + socket + lock metadata remain on disk

[next CLI or daemon startup]
  |-> acquire_lock() succeeds
  |-> kill_orphan_daemons_for_home() — old PID in PID file: check liveness, SIGTERM + SIGKILL if alive
  |-> normal startup continues
```

## Sprint Breakdown

- **fix-539a**: Core liveness validation + startup sweep (Layer 1) — highest value
- **fix-539b**: SIGTERM hardening (Layer 2)
- **fix-539c**: CLI + Doctor enrichment (Layer 3)
- **fix-539d**: Test daemon isolation (Layer 4)

## Cross-Platform Notes

| Concern | Unix (macOS/Linux) | Windows |
|---------|-------------------|---------|
| Process scan | `pgrep -u $(whoami) -f atm-daemon` | `tasklist /FI "IMAGENAME eq atm-daemon.exe"` |
| Kill | `kill(pid, SIGTERM)` + `kill(pid, SIGKILL)` | `taskkill /PID {pid} /F` |
| PID liveness | `kill(pid, 0)` | `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` |
| SIGTERM handler | `tokio::signal::unix::signal(Terminate)` | N/A (Ctrl+C only) |
| Process env read | `/proc/{pid}/environ` (Linux), `ps eww` (macOS) | `wmic process get processid,commandline` |
| Test registry | `$TMPDIR/atm-daemon-test-pids.jsonl` | `%TEMP%\atm-daemon-test-pids.jsonl` |

## Risk Assessment

| Risk | Impact | Likelihood | Mitigation |
|------|--------|-----------|------------|
| PID reuse kills wrong process | High | Low | Verify process name contains "atm-daemon" before any kill |
| `pgrep` unavailable in container | Low | Medium | Graceful fallback, log warning, skip sweep |
| TMPDIR registry grows unbounded | Low | Low | Cleanup on every daemon start |
| Watchdog exits before graceful shutdown | Medium | Very Low | 30s timeout is generous |
| Breaking ATM_HOME test isolation | High | Low | Test daemon registration is additive only |
| Windows CI failures | Medium | Medium | All new code gated behind `#[cfg(unix)]`/`#[cfg(windows)]` |

## Constraints

- ATM_HOME isolation for tests MUST be preserved — no change to that pattern
- All sweep operations are best-effort — errors logged at `warn`, never block startup
- PID validation required before any kill signal (prevent PID reuse false positives)
- `fs2` flock-based singleton lock remains the primary guard — orphan sweep is secondary defense
