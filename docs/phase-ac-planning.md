# Phase AC Planning

**Status**: Planning (2026-03-06)
**Integration branch**: `integrate/phase-AC` (off `develop`)
**Target version**: v0.38.0
**Goal**: Codex agent reliability, daemon state hardening, `atm spawn` interactive UX

---

## Background

Phase AB delivered spawn gate (leaders-only), session scoping, CI hardening, and `atm spawn --help` / `cleanup --dry-run`. Two classes of bugs surfaced during AB that need systematic fixes in Phase AC:

1. **Flaky test in event_loop.rs** — process-global statics mutated concurrently by non-serial tests. Root cause and fix designed; arch-ctm implementing.
2. **Codex agent state persistence** — arch-ctm never registers with the daemon (Codex does not fire Claude Code hooks), causing `atm cleanup` to remove active Codex team members and `atm doctor` to show them as Offline/Unknown. Full root cause in `docs/codex-agent-registration.md`.

Additionally, the spawn UX prototype (`scripts/spawn-demo.sh`) was built and validated during Phase AB planning. Phase AC implements it in Rust.

---

## Sprint Overview

| Sprint | Track | Description | Depends On |
|--------|-------|-------------|------------|
| AC.1 | Test | Fix flaky ReconcileCycleState test (event_loop.rs) | — |
| AC.2 | Daemon | Codex self-registration (`atm register`) + cleanup guard | AC.1 |
| AC.3 | CLI | `atm spawn` interactive review-panel UX | — |

AC.1 and AC.3 are independent and can run in parallel.

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

**Status**: In progress (arch-ctm on planning/phase-AC worktree). BF-001 QA finding resolved, fix in progress.

---

## Sprint AC.2 — Codex Agent Self-Registration

**Goal**: arch-ctm shows correct PID/session in `atm logs`; `atm cleanup` does not remove active Codex agents.

**Root cause**: See `docs/codex-agent-registration.md`.

**Changes**:

### AC.2a — `atm register` command (daemon + CLI)

New socket command `register` in daemon `socket.rs`:
- Validates agent is a known team member
- Skips PID backend validation (external agents are not Claude processes)
- Calls `session_registry.upsert_for_team()` + `state_store.set_state(Active)`
- Emits session identity change events (same as `session_start`)

New `atm register` CLI subcommand:
- Reads identity from `ATM_IDENTITY` / `.atm.toml` pipeline
- Sends register event to daemon socket
- Called by `launch-worker.sh` after Codex startup

### AC.2b — `atm cleanup` external agent guard

In cleanup logic: skip removal of members with `agentType` in `{"codex", "gemini", "external"}` unless `last_seen` is older than 7 days.

This prevents cleanup from removing Codex agents that are active but simply have no daemon state record (because they have not yet called `atm register` in this daemon lifecycle).

**Files**:
- `crates/atm-daemon/src/daemon/socket.rs`
- `crates/atm-core/src/socket_protocol.rs`
- `crates/atm/src/commands/register.rs` (new)
- `crates/atm/src/main.rs`
- `crates/atm-daemon/src/daemon/cleanup.rs`
- `scripts/launch-worker.sh`

**Acceptance criteria**:
- After `atm register`, `atm logs` shows `arch-ctm@atm-dev [<pid>]` (not `[-]`)
- After `atm register`, `atm doctor` shows arch-ctm as Online
- `atm teams cleanup atm-dev` does not remove arch-ctm when it has no daemon state record
- All new tests pass; no regression on existing tests

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
        ├── feature/AC-2-codex-register    -> PR -> integrate/phase-AC
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

## Open Questions

1. Should `atm register` be called automatically by the daemon when it detects a new send event from an unregistered agent? (Option B from codex-agent-registration.md)
2. Should `atm register` accept an explicit `--pid` flag, or always auto-detect via `getppid()`?
3. Should the 7-day cleanup grace period for external agents be configurable in `.atm.toml`?
