# Phase AC Planning

**Status**: Planning (2026-03-06)
**Integration branch**: `integrate/phase-AC` (off `develop`)
**Target version**: v0.38.0
**Goal**: Codex agent reliability, daemon state hardening, `atm spawn` interactive UX, gh-monitor config validation fix

---

## Background

Phase AB delivered spawn gate (leaders-only), session scoping, CI hardening, and `atm spawn --help` / `cleanup --dry-run`. Three issues surfaced during AB that need systematic fixes in Phase AC:

1. **Flaky test in event_loop.rs** — process-global statics mutated concurrently by non-serial tests. **FIXED** by arch-ctm (41053cf, planning/phase-AC).
2. **Codex agent state persistence** — arch-ctm showed `[-]` in all ATM logs; root cause was `backend_expected_rule` not recognising `agentType=codex`. **FIXED** by arch-ctm (da6cae5, planning/phase-AC). Cleanup guard still needed (AC.2b). Full analysis in `docs/codex-agent-registration.md`.
3. **gh-monitor config validation** — `validate_gh_monitor_config` does not check `repo` is set; a config with `enabled=true` but no `repo` passes validation and returns "ok" instead of `CONFIG_ERROR`. Filed as issue #471. **Fix in AC.2b**.

Additionally, the spawn UX prototype (`scripts/spawn-demo.sh`) was built and validated during Phase AB planning. Phase AC implements it in Rust.

---

## Sprint Overview

| Sprint | Track | Description | Status | Depends On |
|--------|-------|-------------|--------|------------|
| AC.1 | Test | Fix flaky ReconcileCycleState test (event_loop.rs) | **DONE** (41053cf) | — |
| AC.1b | Daemon | Fix Codex PPID detection in send.rs (arch-ctm `[-]` bug) | **DONE** (da6cae5) | — |
| AC.2 | Daemon | Cleanup guard tests (guard already in Phase AB) + fix `validate_gh_monitor_config` repo check (#471) | Planned | AC.1 |
| AC.3 | CLI | `atm spawn` interactive review-panel UX | Planned | — |
| AC.4 | Daemon | Daemon logging + startup observability + plugin init isolation (#472, #473, #474) | Planned | — |
| AC.5 | QA/Compliance | Spawn command alignment + compliance/test-plan updates | Complete | AC.2, AC.3, AC.4 |
| AC.6 | QA/Hardening | Hook install confidence + parity coverage + init matrix validation | In progress | AC.5 |
| AC.7 | Daemon/QA | Hook lifecycle coverage + restart recovery convergence hardening | In progress | AC.6 |

AC.2 and AC.3 are independent and can run in parallel.

---

## Sprint AC.1 — Flaky Test Fix

**Goal**: Eliminate race condition in `test_reconcile_prunes_stale_absent_dead_members_only_after_two_full_extra_cycles`.

**Root cause**: `ABSENT_REGISTRY_CYCLES` and `DEAD_MEMBER_CYCLES` are process-global `static LazyLock<Mutex<HashMap>>`. Four non-`#[serial]` tests mutate the same keys concurrently with the flaky test on macOS CI.

**Fix**: Inject `ReconcileCycleState` struct per test. Full design in `docs/flaky-test-analysis.md`.

**Key change**: `reconcile_team_member_activity[_with_mode]` gains a `cycle_state: &SharedCycleState` parameter. Production code creates one instance per daemon lifetime. Tests create a fresh instance per test.

**File**: `crates/atm-daemon/src/daemon/event_loop.rs`
**Scope**: ~80 lines changed, all within `event_loop.rs`.

**Acceptance criteria**:
- `cargo test -p agent-team-mail-daemon` passes 3 consecutive runs on macOS
- No `#[serial]` needed for cycle isolation
- `cargo clippy -p agent-team-mail-daemon` clean

**Status**: **COMPLETE** — arch-ctm commit 41053cf on planning/phase-AC. All 14 reconcile tests use per-test `super::new_reconcile_cycle_state()`. `cargo clippy` clean. Targeted reconcile tests: 10/10 pass.

---

## Sprint AC.1b — Codex PPID Detection (COMPLETE)

**Status**: **COMPLETE** — arch-ctm commit da6cae5 on planning/phase-AC.

**Changes delivered**:
- `backend_expected_rule` now honours `agentType=codex/gemini` (legacy fallback)
- `process_matches_rule` normalises to basename (`rsplit('/').next()`)
- PPID traversal depth 8→16
- Stable session ID `local:{sender}:pid:{process_id}` for non-hook processes
- Log format: emitter `pid/ppid` prefix removed from send lines; only sender/recipient PID slots shown

**Remaining gap**: `atm doctor` still shows `ACTIVE_WITHOUT_SESSION` because the send path does not write to `session_registry`. Tracked as part of AC.4 daemon state work.

---

## Sprint AC.2 — Cleanup Guard Tests + gh-monitor Config Fix

**Goal**: Add test coverage for the external agent cleanup guard (already implemented in Phase AB); fix `validate_gh_monitor_config` to require `repo` (issue #471).

**Root cause references**: `docs/codex-agent-registration.md`, issue #471.

### AC.2a — `atm cleanup` external agent guard (guard already implemented)

**Status**: The guard is already in `crates/atm/src/commands/teams.rs` (Phase AB, commit 837e421). Implementation:
- `is_external = member.external_backend_type.is_some()` — detects codex/gemini/external agents
- No session_id → agent skipped with "unknown liveness" warning (kept, not removed)
- session_id present → daemon queried; only removed if daemon explicitly reports session dead
- `--dry-run` lists skipped external agents in a `Skipped N member(s)` footer line

**Remaining work**: Add the two tests specified in `docs/codex-agent-registration.md`:
- `test_cleanup_does_not_remove_external_agent_without_state` — verifies no-session_id external agent is kept
- `test_cleanup_removes_external_agent_after_long_absence` (can be renamed to reflect daemon-dead path)

**File**: `crates/atm/src/commands/teams.rs` (tests only — production code already correct)

### AC.2b — Fix `validate_gh_monitor_config` repo check (issue #471)

In `crates/atm-daemon/src/daemon/socket.rs`, locate `validate_gh_monitor_config` (line ~2097).
After parsing `CiMonitorConfig`, add a `repo` presence check:
```rust
if parsed.repo.as_deref().map(str::trim).unwrap_or("").is_empty() {
    return Err("gh_monitor configuration missing required field: repo".to_string());
}
```
This fixes `test_gh_monitor_invalid_config_transitions_to_disabled_config_error`.

**File**: `crates/atm-daemon/src/daemon/socket.rs`

**Acceptance criteria**:
- `atm teams cleanup atm-dev` does not remove arch-ctm when it has no daemon state record (test coverage added)
- `test_gh_monitor_invalid_config_transitions_to_disabled_config_error` passes
- `cargo test -p agent-team-mail-daemon` fully green
- `cargo test -p agent-team-mail` fully green
- `cargo clippy` clean

---

## Sprint AC.3 — `atm spawn` Interactive UX

**Goal**: `atm spawn` in a terminal launches a review-panel UX. Non-interactive path (no tty, or `--yes`) is unchanged.

**Prototype**: `scripts/spawn-demo.sh` on `develop` (commit e8f8cf0) — Rust implementation must match this UX.

**Design**: See `docs/spawn-ux.md`.

**Key behaviors**:
- Numbered field list with current values
- `n=value` or `n=value,m=value2` inline editing syntax
- Per-field validation; errors shown inline; confirmation blocked until clean
- `new-pane` / `existing-pane` / `current-pane` modes
- `--dry-run` shows tmux + launch commands without executing
- stdin-is-not-tty guard with helpful error message

**Files**:
- `crates/atm/src/commands/spawn.rs` (new)
- `crates/atm-core/src/spawn.rs` (new, shared logic)
- `crates/atm/src/main.rs`
- `crates/atm/Cargo.toml` (add `crossterm` dep if not already present)

**Acceptance criteria**:
- `atm spawn codex` in a terminal shows review panel
- `atm spawn codex --dry-run` prints commands without executing
- `echo "" | atm spawn codex` prints tty-guard error and exits 1
- `atm spawn codex --yes` executes without prompting
- All pane modes produce correct tmux commands in dry-run output

---

## Integration Strategy

```
develop
  └── integrate/phase-AC          (created from develop)
        ├── feature/AC-1-flaky-test-fix    -> PR -> integrate/phase-AC
        ├── feature/AC-2-cleanup-guard     -> PR -> integrate/phase-AC
        └── feature/AC-3-spawn-interactive -> PR -> integrate/phase-AC
```

After all sprints merge to `integrate/phase-AC`: one final PR to `develop`.

---

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| AC.1 fix introduces performance regression (Arc<Mutex> per reconcile call) | Low | Low | Profile reconcile loop; Arc<Mutex> overhead is negligible at this frequency |
| AC.2 `atm register` called before daemon starts | Medium | Low | Daemon auto-starts on first CLI call; register retries once |
| AC.3 `crossterm` raw mode breaks on some terminals | Low | Medium | Test on iTerm2 + Terminal.app; add `--no-interactive` fallback |
| arch-ctm does not call `atm register` automatically | Medium | High | AC.2b cleanup guard provides safety net until AC.2a is confirmed working |

---

---

## Sprint AC.4 — Daemon Logging & Startup Observability

**Goal**: Fix three root causes identified during gh-monitor testing that make daemon failures silent and non-recoverable.

**Source**: arch-ctm investigation, 2026-03-06. Issues #472, #473, #474.

### AC.4a — DaemonWriter PRODUCER_TX fix (issue #472)

`setup_daemon_writer` in `crates/atm-core/src/logging.rs` never sets `PRODUCER_TX`. All `emit_event_best_effort` calls on the daemon side are silently dropped — no structured events are persisted to `atm logs`.

**Fix**: Initialize `PRODUCER_TX` in `setup_daemon_writer` to route daemon-side events to the unified fan-in sink.

### AC.4b — Autostart startup observability (issue #473)

`ensure_daemon_running_unix` spawns daemon with `stdout/stderr=null`. Startup failures are silent — no context surfaced to caller or structured logs.

**Fix**: Capture daemon stderr tail on startup failure; include in returned error and `daemon_autostart_failure` structured event.

### AC.4c — Plugin init failure isolation (issue #474)

`init_all` in `crates/atm-daemon/src/plugin/registry.rs` is fail-fast (`?`). One bad plugin init kills entire daemon startup.

**Fix**: Change `init_all` to mark failed plugins as `disabled_init_error` and continue. Surface via `PLUGIN_INIT_FAILED` finding in `atm doctor`. This implements the plugin failure isolation contract in §5.9 of `requirements.md`.

**Files**:
- `crates/atm-core/src/logging.rs` — PRODUCER_TX init in setup_daemon_writer
- `crates/atm-core/src/daemon_client.rs` — stderr capture on autostart failure
- `crates/atm-daemon/src/plugin/registry.rs` — per-plugin error isolation in init_all

**Acceptance criteria**:
- `atm logs` surfaces daemon-side `emit_event_best_effort` events
- `atm doctor` shows `PLUGIN_INIT_FAILED` when a plugin fails init, daemon otherwise healthy
- Autostart failure returns actionable error including stderr tail
- `cargo test -p agent-team-mail-daemon` green
- `cargo clippy` clean

---

## Open Questions

1. Should `atm register` be called automatically by the daemon when it detects a new send event from an unregistered agent? (Option B from codex-agent-registration.md)
2. Should `atm register` accept an explicit `--pid` flag, or always auto-detect via `getppid()`?
3. Should the 7-day cleanup grace period for external agents be configurable in `.atm.toml`?
