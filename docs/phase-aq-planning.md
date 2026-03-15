# Phase AQ — Codebase Cleanup

**Branch**: `integrate/phase-AQ` off `develop`
**Prerequisite**: integrate/phase-AN ✅ merged 2026-03-15, integrate/phase-AO ✅ merged 2026-03-15, integrate/phase-AP ✅ merged 2026-03-15 (PR #768)

## Overview

Phase AQ is a focused cleanup phase addressing findings from the AN/AO/AP phase reviews plus user-directed improvements. No new features. All sprints target `develop` directly via `integrate/phase-AQ`.

---

## Sprint Plan

### AQ.1 — Const Consolidation and Magic Number Elimination

**Scope**: Extract all significant magic numbers to named constants; create `consts.rs` modules in folders where constants are currently scattered across multiple files.

**New files**:
- `crates/atm-core/src/consts.rs` — shared cross-module constants
- `crates/atm-daemon/src/daemon/consts.rs` — daemon runtime constants
- `crates/atm-daemon/src/plugins/consts.rs` — plugin-level constants
- `crates/atm/src/consts.rs` — CLI constants

**Constants to consolidate**:

| Crate | Current location | Constant | Value | Suggested name |
|-------|-----------------|----------|-------|----------------|
| atm / atm-daemon | `commands/send.rs`, `commands/broadcast.rs` | `MAX_LEN` | `100` | `MESSAGE_MAX_LEN` → `atm/src/consts.rs` |
| atm / atm-daemon | `util/hook_identity.rs`, `daemon/socket.rs` | `SESSION_FILE_TTL_SECS` | `86400.0` | Consolidate to `atm-core/src/consts.rs` |
| atm-daemon | `daemon/event_loop.rs:232` | spool drain interval | `10` | `SPOOL_DRAIN_INTERVAL_SECS` |
| atm-daemon | `daemon/event_loop.rs:242` | event channel buffer | `100` | `EVENT_CHANNEL_CAPACITY` |
| atm-daemon | `daemon/event_loop.rs:483-502` (×8) | graceful shutdown timeout | `5` | `GRACEFUL_SHUTDOWN_TIMEOUT_SECS` |
| atm-daemon | `daemon/event_loop.rs:1334` | status write interval | `30` | `STATUS_WRITE_INTERVAL_SECS` |
| atm-daemon | `daemon/socket.rs:400` | socket retry delay | `100` ms | `SOCKET_RETRY_DELAY_MS` |
| atm-daemon | `daemon/socket.rs:2816,2872` (×2) | drain timeout default | `30` | `DEFAULT_DRAIN_TIMEOUT_SECS` |
| atm-daemon | `daemon/socket.rs:2837,2893` (×2) | sleep between drains | `250` ms | `DRAIN_SLEEP_MS` |
| atm-daemon | `daemon/socket.rs:3504` | poll intervals | `5`, `15` | `INITIAL_POLL_INTERVAL_SECS`, `SUBSEQUENT_POLL_INTERVAL_SECS` |
| atm-daemon | `daemon/socket.rs:4507` | default timeout | `300` | `DEFAULT_TIMEOUT_SECS` |
| atm-daemon | `daemon/socket.rs:6615,6676` (×2) | poll sleep | `5` ms | `POLL_SLEEP_MS` |
| atm-daemon | `daemon/socket.rs:6706,6735` (×2) | stream check sleep | `25` ms | `STREAM_CHECK_SLEEP_MS` |
| atm-daemon | `daemon/socket.rs:7820,7836,7840` | stale threshold | `15`,`59`,`60` | `GH_MONITOR_STALE_THRESHOLD_SECS` |
| atm-daemon | `daemon/socket.rs:11389,11410` (×2) | timestamp window | `300` | `TIMESTAMP_WINDOW_SECS` |
| atm-daemon | `daemon/log_writer.rs:69` | warning rate limit | `5` | `LOG_WARNING_RATE_LIMIT_SECS` |
| atm-daemon | `daemon/socket.rs:10121` | elapsed check threshold | `20` ms | `MIN_ELAPSED_CHECK_MS` |
| atm-daemon | `plugins/worker_adapter/plugin.rs:1217` | log rotation interval | `300` | `LOG_ROTATION_INTERVAL_SECS` |
| atm-daemon | `plugins/worker_adapter/plugin.rs:1223` | nudge scan interval | `5` | `NUDGE_SCAN_INTERVAL_SECS` |
| atm-core | `daemon_client.rs:1234` | timeout bounds | `30`, `600` | `DAEMON_TIMEOUT_MIN_SECS`, `DAEMON_TIMEOUT_MAX_SECS` |
| atm-core | `daemon_client.rs:1413,1635` (×2) | startup deadline | `5` | `STARTUP_DEADLINE_SECS` |
| atm-core | `daemon_client.rs:1419,1442,1566,1682,1982,1990` (×6) | retry sleep | `100` ms | `RETRY_SLEEP_MS` |
| atm-core | `daemon_client.rs:1460,2091` (×2) | socket I/O timeout | `500` ms | `SOCKET_IO_TIMEOUT_MS` |
| atm-core | `daemon_client.rs:2152,2165` (×2) | short deadline | `2` | `SHORT_DEADLINE_SECS` |
| atm-core | `daemon_client.rs:2158,2186` (×2) | poll check sleep | `25` ms | `POLL_CHECK_SLEEP_MS` |
| atm-core | `daemon_client.rs:2848,3015` | short sleep | `50` ms | `SHORT_SLEEP_MS` |
| atm-core | `daemon_client.rs:543` | query timeout | `500` ms | `DAEMON_QUERY_TIMEOUT_MS` |
| atm-core | `logging.rs:236` | log channel capacity | `512` | `LOG_EVENT_CHANNEL_CAPACITY` |

**Deliverables**:
- 4 new `consts.rs` files with full documentation comments
- All callsites updated to reference named constants
- No behavioral changes; CI must pass green

---

### AQ.2 — Dead Code Removal and Duplicate Elimination

**Scope**: Remove dead code identified in phase reviews; eliminate duplicate logic patterns flagged by arch-qa.

**Known items from reviews**:
- ARCH-001 class: any remaining shadow type definitions or re-exports that duplicate canonical types (AN review found shadow `GhMonitorStateRecord`/`GhMonitorStateFile` in plugin.rs — fixed, but audit for similar patterns)
- Duplicate `notify_target` resolution logic (AN-004 class)
- Any dead `#[allow(dead_code)]` attributes added as workarounds rather than fixes
- **QA-002** (rust-qa minor, AP review): `integration_conflict_tests.rs` has its own local `daemon_binary_path()` helper that duplicates the canonical implementation in `daemon_process_guard.rs`. Remove the local copy and use the shared support module.
- **ATM-QA-004** (AP review, minor): `RuntimeDaemonCleanupGuard` calls `reap_child_pid_best_effort` on non-child PIDs — harmless (ECHILD) but undocumented. Add doc comment explaining the intent.
- **ATM-QA-003** (AP review, minor): 1 bare sleep used as retry backoff (not synchronization) — add explanatory comment.
- **ATM-QA-005** (AP review, minor): `#[serial]` comment on `test_spool_drain_delivery_cycle` is stale — update or remove.

**Deliverables**:
- `cargo check --all-features` + `cargo clippy -- -D warnings` clean
- No `#[allow(dead_code)]` annotations except where genuinely needed for public API
- All removed items verified unused via `find_referencing_symbols`

---

### AQ.3 — Deferred Non-Blocking Findings

**Scope**: Fix the two GH issues filed as non-blocking during AO integration review.

**Items**:
- **GH #761** — `write_runtime_metadata` in `gh_monitor_observability.rs:737`: `let _current = read_runtime_metadata(home)` re-reads but discards result. Caller responsibility or genuine bug — investigate and either fix or document with comment explaining intent.
- **GH #763** — `gh_monitor_observability.rs:140-169,182-209,635-641`: 5-minute cache TTL purges entire `GhRepoStateRecord` including hourly budget state. TTL should only evict stale cache entries, not reset accumulated budget. Fix TTL to preserve budget across cache refresh cycles.

**Deliverables**:
- Both GH issues closed
- Tests covering both fixed behaviors

---

### AQ.4 — AP.5 Deferred Scope: PID-File Race and Observability Gap

**Scope**: Items deferred from AP.4 due to scope — these are the remaining daemon lifecycle hardening items.

**Items**:
- `integration_conflict_tests.rs:415`, `:922`, `:963` — `RuntimeDaemonCleanupGuard` PID-file race: daemon writes PID file after guard is created, so guard may attempt cleanup before PID is written. Add synchronization or retry logic.
- `daemon_autostart_observability.rs` — missing RAII guard: autostart path spawns daemon but doesn't adopt it into a cleanup guard before any await point, leaving a leak window.

**Deliverables**:
- Both race conditions covered by tests demonstrating the fix
- No process leaks in test runs (validated by `daemon_test_registry` sweep)

---

## Findings from Phase Reviews (consolidated input)

### Phase AN (fixed before merge)
| ID | Finding | Status |
|----|---------|--------|
| ARCH-001 | Shadow `GhMonitorStateRecord`/`GhMonitorStateFile` structs in plugin.rs duplicating types.rs | Fixed (842600b9) |
| AN-004 | `_ => Vec::new()` defaulted notify_target to empty on unknown provider | Fixed (842600b9) |
| AN-009 | `resolve_ci_alert_routing` had no fallback when routing list empty | Fixed (842600b9) |

### Phase AO (non-blocking, deferred to AQ.3)
| ID | Finding | Status |
|----|---------|--------|
| ATM-QA-003 | `write_runtime_metadata:737` re-read result discarded | Filed GH #761 → AQ.3 |
| AO-CTM-001 | 5-min TTL purges hourly budget state | Filed GH #763 → AQ.3 |

### Phase AP (review complete — fixes in progress)

**arch-qa: FAIL → fix-r1 implemented by team-lead (arch-ctm stalled)**

| ID | Finding | Severity | Status |
|----|---------|----------|--------|
| ARCH-002 | `TestDaemonChildGuard` in `daemon_tests.rs:275` duplicates `DaemonProcessGuard` (same RAII kill+wait; `DaemonProcessGuard` already imported in same file) | **Blocking** | **FIXED** — feature/pAP-fix-r1-duplicate-guards, PR #767 |
| ARCH-003 | `FakeDaemonChildGuard` in `integration_daemon_autostart.rs:195` duplicates `DaemonProcessGuard` | **Blocking** | **FIXED** — same branch |
| ARCH-001 | `daemon_client.rs` ~2703 non-test lines (pre-existing RULE-003; grew ~558 lines this sprint) | Important | Pre-existing; track for future refactor sprint |

**atm-qa: PASS — 6 non-blocking**

| ID | Finding | Severity | Disposition |
|----|---------|----------|-------------|
| ATM-QA-002 | `daemon_tests.rs:162/196` bare `set_var("ATM_HOME")` without RAII EnvGuard (serial prevents races but doesn't restore) | Important | Fix in same fix-r1 pass |
| ATM-QA-004 | `RuntimeDaemonCleanupGuard` uses `reap_child_pid_best_effort` on non-child PIDs — harmless (ECHILD) but undocumented | Minor | Add doc comment → AQ.2 |
| ATM-QA-003 | 1 bare sleep is retry backoff not sync | Minor | AQ.2 |
| ATM-QA-005 | `#[serial]` comment on `test_spool_drain_delivery_cycle` stale | Minor | AQ.2 |
| ATM-QA-001 | Docs named `phase-ap-test-hardening.md` not `phase-ap-planning.md` | Minor | Note only |
| ATM-QA-006 | `proxy_integration.rs` timeout wrappers — confirmed compliant | ✓ Pass | — |

**Confirmed resolved by AP.4**: `kill_pid_from_file` — 0 occurrences. DS-AP1-001–005, DS-AP2-004, DS-AP2-006 all verified compliant.

**Rogue daemon investigation complete** — 7 leaked daemons (PIDs 91450–91521) killed. Root causes identified by daemon-spawn-qa, all incorporated into fix-r1:

| ID | File | Finding | Severity | Fix |
|----|------|---------|----------|-----|
| F-1 | `atm/tests/support/daemon_process_guard.rs` `spawn()` | `ATM_DAEMON_BIN` env var inherited → installed binary spawns instead of test target; never tracked → leaks | **Blocking** | `.env_remove("ATM_DAEMON_BIN")` before `.spawn()` |
| F-2 | `daemon_process_guard.rs` `adopt_registered_pid()` | Uses ambient `ATM_HOME` not caller's `TempDir` → lock file path mismatch → PID never released | **Blocking** | Add explicit `atm_home: &Path` parameter; update callsites |
| F-3 | `daemon_process_guard.rs` `register_test_daemon()` | No dedup check; called from both `spawn()` and `adopt_registered_pid()` → conflicting entries | **Blocking** | Check for existing registration before writing |
| F-4 | `daemon_tests.rs:275` | `TestDaemonChildGuard` duplicates `DaemonProcessGuard` (ARCH-002) | **Blocking** | Delete; use `DaemonProcessGuard` |
| F-5 | `integration_daemon_autostart.rs:195` | `FakeDaemonChildGuard` duplicates `DaemonProcessGuard` (ARCH-003) | **Blocking** | Delete; use `DaemonProcessGuard` |
| F-6 | `daemon_tests.rs:162,196` | Bare `set_var("ATM_HOME")` without `EnvGuard` RAII (ATM-QA-002) | **Blocking** | Wrap in `EnvGuard` |

Attribution: F-1/F-2/F-3 are pre-existing in `daemon_process_guard.rs`; F-4/F-5 introduced by pAP-s3; F-6 is pAP-s1 non-compliance.
All 6 fixed by team-lead on `feature/pAP-fix-r1-duplicate-guards` (PR #767). Re-QA PASS (arch-qa + rust-qa). CI green 16/16. Merged to integrate/phase-AP.

---

## Sprint Dependencies

```
AQ.1 (consts)      ─┐
AQ.2 (dead code)   ─┤─→ integrate/phase-AQ → develop → release
AQ.3 (GH#761/763)  ─┤
AQ.4 (AP.5 scope)  ─┘
```

AQ.1 and AQ.2 can run in parallel (different files, no overlap).
AQ.3 and AQ.4 can run in parallel.
All four can merge to `integrate/phase-AQ` independently once CI green.

---

## Release Gate

After all AQ sprints merge to `develop`:
1. Dogfood on develop branch (dev-install)
2. Publish as next version bump
