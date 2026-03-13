# Flaky Test Root Cause Analysis

**Test**: `test_reconcile_prunes_stale_absent_dead_members_only_after_two_full_extra_cycles`
**File**: `crates/atm-daemon/src/daemon/event_loop.rs` (line 1998)
**Analyst**: rust-architect (Opus), 2026-03-06

---

## Root Cause

The test fails intermittently on macOS CI due to **shared mutable global state accessed concurrently by serial and non-serial tests**.

Two process-global statics control reconcile pruning behavior:

- **`ABSENT_REGISTRY_CYCLES`** (line 28): `static LazyLock<Mutex<HashMap<String, u8>>>`
- **`DEAD_MEMBER_CYCLES`** (line 35): `static LazyLock<Mutex<HashMap<String, u8>>>`

The flaky test (marked `#[serial]`) clears both maps at entry, then calls `reconcile_team_member_activity` three times expecting a precise cycle progression: cycle 1 inserts counter=1, cycle 2 increments to counter=2, cycle 3 reaches threshold (>=3) and prunes.

**The race**: Four other tests call `reconcile_team_member_activity` **without `#[serial]`**:

| Line | Test Name | Has `#[serial]`? | Team name |
|------|-----------|-------------------|-----------|
| 1568 | `test_reconcile_seeds_state_store_from_config` | No | `"atm-dev"` |
| 1608 | `test_reconcile_removes_deleted_member_from_state_store` | No | `"atm-dev"` |
| 1665 | `test_reconcile_marks_missing_session_member_inactive_after_restore` | No | `"atm-dev"` |
| 1714 | `test_reconcile_keeps_pid_backend_mismatch_offline_within_same_pass` | No | `"atm-dev"` |

The `#[serial]` attribute only serializes tests that are themselves marked `#[serial]`. Non-serial tests run freely in parallel, including alongside serial tests. When a non-serial test runs concurrently with the flaky test, it can:

1. Insert or remove entries in `ABSENT_REGISTRY_CYCLES` with key `"atm-dev:arch-ctm"` between the flaky test's reconcile calls.
2. Cause the cycle counter to be off-by-one (prune fires on cycle 2, or never fires).
3. Insert a `DEAD_MEMBER_CYCLES` entry that changes which branch of the pruning conditional executes.

macOS CI surfaces this more often because Darwin's thread scheduler provides less deterministic ordering than Linux, widening the interleave window.

**Additional factor**: All affected tests use the hardcoded team name `"atm-dev"`, so their HashMap keys collide (e.g., `"atm-dev:arch-ctm"`).

---

## All At-Risk Tests

| Line | Test Name | Risk |
|------|-----------|------|
| **1998** | `test_reconcile_prunes_stale_absent_dead_members_only_after_two_full_extra_cycles` | **Flaky (confirmed)** |
| 1918 | `test_session_end_converges_to_remove_dead_member_from_roster_and_mailbox` | At risk |
| 1944 | `test_sigterm_escalation_converges_to_remove_dead_member_from_roster_and_mailbox` | At risk |
| 1970 | `test_kill_timeout_fallback_converges_to_remove_dead_member_from_roster_and_mailbox` | At risk |
| 2059 | `test_reconcile_does_not_prune_absent_active_sessions` | At risk |
| 2105 | `test_reconcile_dispatch_mode_remove_then_readd_preserves_dead_session_record` | At risk |
| 2286 | `test_reconcile_config_dispatch_mode_does_not_advance_absent_prune_cycles` | At risk |

---

## Recommended Fixes

### Option A (Preferred): Inject Cycle Counters as Parameters

Replace the two process-global statics with an injectable `ReconcileCycleState` struct passed as a parameter.

```rust
/// Tracks cycle counters for reconcile pruning decisions.
/// Production code creates one instance per daemon lifetime.
/// Tests create one instance per test — no global state.
#[derive(Default, Clone)]
pub(crate) struct ReconcileCycleState {
    pub absent_cycles: HashMap<String, u8>,
    pub dead_member_cycles: HashMap<String, u8>,
}

pub(crate) type SharedCycleState = Arc<Mutex<ReconcileCycleState>>;
```

**Changes**:

1. `reconcile_team_member_activity` and `reconcile_team_member_activity_with_mode`: add `cycle_state: &SharedCycleState` parameter; replace all `ABSENT_REGISTRY_CYCLES.lock()` with `cycle_state.lock().unwrap().absent_cycles`, and similarly for `DEAD_MEMBER_CYCLES`.
2. `reconcile_loop` (line 470): create one `SharedCycleState` at loop entry, pass to every reconcile call.
3. `run()` (line 86): create one `SharedCycleState` for the startup reconcile pass, share it with dispatch task and reconcile loop.
4. All tests: create a fresh `ReconcileCycleState::default()` per test. Remove `.clear()` calls. Remove `#[serial]` where it was only needed for global-state isolation.
5. Remove the two `static LazyLock` declarations at lines 28-37.

**Scope**: ~80 lines changed, all within `event_loop.rs`. No API changes to other crates.

**Benefits**: Fully deterministic. No global state. Tests run in parallel. No `#[serial]` needed for cycle isolation. Supports arbitrary pre-seeded cycle states in tests.

### Option B (Band-aid): Make All Reconcile Tests Serial

Add `#[serial]` to the four non-serial tests (lines 1568, 1608, 1665, 1714) and add global-clear setup to each.

**Scope**: ~12 lines added.

**Drawbacks**: Tests slower (serial execution). Global statics remain — any future test that forgets `#[serial]` reintroduces the bug. Does not address the architectural smell.

### Option C: Use Unique Team Names in All Tests

Change every test using `"atm-dev"` to `unique_test_team_name()`. Since cycle-counter keys are `"{team}:{agent}"`, unique names prevent key collisions.

**Scope**: ~30 lines changed.

**Drawbacks**: Non-serial tests still mutate the global maps (latent risk). Fragile — adding any test with `"atm-dev"` re-breaks everything.

---

## Recommendation

**Option A** is the correct fix. It eliminates the root cause (shared mutable global state), makes all reconcile tests fully parallel-safe, and follows Rust best practices (dependency injection over globals). The two-static pattern was a convenience shortcut; replacing with a parameter improves both testability and production code clarity.

**Option B** is a 12-line band-aid if time is constrained — stops the flaky failure but leaves the technical debt.

---

## Files to Modify

| File | Changes |
|------|---------|
| `crates/atm-daemon/src/daemon/event_loop.rs` | Remove statics (lines 28-37), add `ReconcileCycleState` struct, update `reconcile_team_member_activity[_with_mode]` signatures, update `reconcile_loop`, update `run()`, update all ~11 test functions |

No other files reference `ABSENT_REGISTRY_CYCLES` or `DEAD_MEMBER_CYCLES`.

---

## Build Sequence (Option A)

1. Define `ReconcileCycleState` struct and `SharedCycleState` type alias
2. Add `cycle_state: &SharedCycleState` param to `reconcile_team_member_activity_with_mode`
3. Replace all 6 `ABSENT_REGISTRY_CYCLES.lock()` and 4 `DEAD_MEMBER_CYCLES.lock()` call sites with `cycle_state.lock().unwrap()` field access
4. Update `reconcile_team_member_activity` wrapper to forward the param
5. Update `reconcile_loop` to create and hold one `SharedCycleState`
6. Update `run()` startup reconcile + dispatch task to share a `SharedCycleState`
7. Remove the two `static LazyLock` declarations
8. Update all test functions: create local `SharedCycleState`, remove `.clear()` calls, remove `#[serial]` where only needed for global-state isolation
9. `cargo test -p agent-team-mail-daemon`
10. `cargo clippy -p agent-team-mail-daemon`
