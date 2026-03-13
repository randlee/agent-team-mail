# Phase AM Sprint 7: config.json Atomic Write Gate

**Sprint**: AM.7
**Branch**: `feature/pAM-s7-config-write-gate`
**PR target**: `integrate/phase-AM`
**Status**: PLANNED (design review FAIL — revision in progress)

---

## Goal

Guarantee that all writes to `config.json` (team configuration) are atomic,
lock-protected, and go through a single gate. **No caller outside the gate
may hold or derive the config.json path for writing.** The gate owns the
path, the lock, and the write primitive entirely.

---

## Requirement

Every mutation to `config.json` MUST satisfy all four properties:

1. **Lock** — acquire `config.json.lock` before reading or writing
2. **Re-read under lock** — read the current on-disk state *inside* the lock,
   never use a cached in-memory copy as the write base
3. **Atomic write** — write to a `.tmp` file, call `fsync`, then call
   `atomic_swap` (exchange semantics from `crates/atm-core/src/io/atomic.rs`);
   never write directly to `config.json`; **do NOT use `std::fs::rename`**
4. **Single gate** — all of the above is enforced by one type
   (`TeamConfigStore`) whose write methods are the *only* way to mutate the
   file; the path is private to that type

---

## Design: `TeamConfigStore`

### Location

`crates/atm-core/src/team_config_store.rs`

### API

```rust
/// Outcome returned by `update()` and `create_or_update()`.
pub enum UpdateOutcome {
    /// Config was mutated and written to disk.
    Updated(TeamConfig),
    /// Closure signalled no change; disk was not written.
    Unchanged(TeamConfig),
}

pub struct TeamConfigStore {
    config_path: PathBuf,   // private — callers never see this
    lock_path: PathBuf,     // private
}

impl TeamConfigStore {
    /// Open a store for a team directory. Does NOT read from disk yet.
    /// `team_dir` MUST be derived via ATM_HOME-aware helpers
    /// (e.g., `get_home_dir()` / `team_config_path_for()`) — never via
    /// `dirs::home_dir()` directly. Windows CI compliance requires this.
    pub fn open(team_dir: &Path) -> Self;

    /// Apply a mutation under lock+re-read+atomic-write.
    /// `f` returns `Ok(Some(new_config))` to write, or `Ok(None)` to signal
    /// no change (skips the atomic write, returns `Unchanged`).
    /// Returns Err if config.json does not exist (use `create_or_update` for
    /// creation paths).
    pub fn update<F>(&self, f: F) -> Result<UpdateOutcome>
    where
        F: FnOnce(TeamConfig) -> Result<Option<TeamConfig>>;

    /// Create-or-update: same protocol as `update` but uses `default_fn` to
    /// produce an initial config if config.json does not yet exist.
    /// Used by creation-only callers (e.g., `ensure_team_config`).
    pub fn create_or_update<F, D>(&self, default_fn: D, f: F) -> Result<UpdateOutcome>
    where
        D: FnOnce() -> TeamConfig,
        F: FnOnce(TeamConfig) -> Result<Option<TeamConfig>>;

    /// Read the current config (acquires lock for consistent snapshot).
    pub fn read(&self) -> Result<TeamConfig>;

    /// Async wrapper for bridge callers (tokio). Runs `update` inside
    /// `spawn_blocking` so callers do not hand-roll blocking task + error
    /// mapping at each site. No async mutex or registry lock may be held
    /// across this await boundary.
    pub async fn update_async<F>(&self, f: F) -> Result<UpdateOutcome>
    where
        F: FnOnce(TeamConfig) -> Result<Option<TeamConfig>> + Send + 'static;
}
```

`update()` implements the full four-property protocol internally:
1. Acquire lock
2. Read current file from disk (returns Err if missing)
3. Call `f(current_config)` — returns `Ok(Some(new))` to write or `Ok(None)` to skip
4. If `Some(new)`: write to `.tmp`, `fsync`, `atomic_swap` (exchange semantics); return `Updated`
5. If `None`: return `Unchanged` without touching disk
6. Release lock
6. Return new config

`create_or_update()` is identical but calls `default_fn()` if step 2 finds no file.

### Async/Sync Boundary

`TeamConfigStore` is **sync** — it uses `std::fs` and blocking I/O.

Callers in async contexts (e.g., `bridge/team_config_sync.rs` which uses
`tokio::fs`) MUST wrap `TeamConfigStore` calls in `tokio::task::spawn_blocking`:

```rust
let store = store.clone();  // Arc<TeamConfigStore>
tokio::task::spawn_blocking(move || store.update(|cfg| { ... })).await?
```

### Lock Re-entrancy Contract

`TeamConfigStore::update()` acquires the config.json lock internally. Callers
that currently hold an outer lock before calling a write helper (e.g., the
pattern `acquire_lock(); ...; write_team_config(...)`) **must drop the outer
lock acquisition** when migrating to `TeamConfigStore::update()`. The outer
lock is redundant — the store acquires it. Keeping both will deadlock.

All 8 `write_team_config` call sites in `teams.rs` (lines 955, 1584, 1624,
1667, 1852, 1967, 2527, 3110) must be audited for outer-lock acquisition
before migration.

All existing helpers (`write_team_config`, `atomic_config_update`,
`write_team_config_atomic`) are replaced by `TeamConfigStore::update`.

---

## Violations to Fix (from rust-explorer audit + design review)

### CRITICAL — must fix

| # | File | Lines | Violation |
|---|------|-------|-----------|
| 1 | `crates/atm-daemon/src/daemon/event_loop.rs` | 586-593, 856, 896-906 | `reconcile_team_member_activity_with_mode` reads config **outside** the lock at line 586, mutates in-memory, then calls `write_team_config_atomic` at line 856 with the stale snapshot. The lock is acquired inside that function but the value being written is already stale. Also: `write_team_config_atomic` uses `std::fs::write` (no fsync). |
| 2 | `crates/atm-daemon/src/plugins/bridge/team_config_sync.rs` | 61-70 | Init path writes directly to `config.json` with `fs::write` — no lock, no tmp, no fsync, no rename. |
| 3 | `crates/atm-daemon/src/plugins/bridge/team_config_sync.rs` | 62-64, 76-79 | Merge path reads without lock at line 62, writes with tmp+rename but still no lock and no fsync. |
| 4 | `crates/atm-daemon/src/daemon/hook_watcher.rs` | ~714, ~804 | `write_team_config_atomic` second copy (identical TOCTOU class as Finding 1): `auto_update_member_session_id` reads config.json outside the lock before passing the snapshot to `write_team_config_atomic`. Must be migrated to `TeamConfigStore::update`. |
| 5 | `crates/atm-daemon/src/daemon/activity.rs` | ~87 | `ActivityTracker::update_config` is a standalone reimplementation of the full gate (lock + re-read + fsync + atomic_swap) — functionally correct but violates Single Gate invariant. Must be replaced by `TeamConfigStore::update`. |

### MINOR — fix to enforce the gate (prevent future regressions)

| # | File | Lines | Violation |
|---|------|-------|-----------|
| 6 | `crates/atm/src/commands/register.rs` | 100 | Pre-lock unlocked read for liveness check — TOCTOU in check only, write path is safe. Migrate to `TeamConfigStore::read`. |
| 7 | `crates/atm/src/commands/teams.rs` | 3008, 3014-3019 | `restore` builds `restore_members` list from pre-lock read at 3008. Dry-run output can be stale. Migrate to `TeamConfigStore`. |
| 8 | `crates/atm/src/commands/init.rs` | 844, 855-858 | `ensure_team_config` (creation-only path) uses `write_json_atomic` without lock. No established data at risk but violates the gate invariant. Migrate to `TeamConfigStore::create_or_update`. |
| 9 | `crates/atm-daemon/src/roster/service.rs` | 314 | `atomic_config_update` uses `std::fs::write` (no fsync) before rename. Lock and re-read are correct. Migrate to `TeamConfigStore`. |

---

## Reference: Gold-Standard Implementations (to be replaced by gate)

- `crates/atm/src/commands/teams.rs:3177` — `write_team_config()`: uses `atomic_swap` + `sync_all`, called under lock by all callers. **This is the template for the new store.**
- `crates/atm/src/commands/register.rs:252` — identical pattern.

Both will be deleted and replaced by `TeamConfigStore::update`.

---

## Lock Path Consistency

All existing lock sites derive the lock path as:
```rust
config_path.with_extension("lock")
```
This is consistent. `TeamConfigStore` will use the same derivation internally.

---

## Deliverables

1. `crates/atm-core/src/team_config_store.rs` — `TeamConfigStore` with `open`, `read`, `update`, `create_or_update`
2. Migrate all CRITICAL sites (Findings 1–5) to use `TeamConfigStore::update`
3. Migrate all MINOR sites (Findings 6–9) to use `TeamConfigStore` / `create_or_update`
4. Delete: `write_team_config` (teams.rs:3177 **and** register.rs:252), `write_team_config_atomic` (event_loop.rs:896 and hook_watcher.rs:~714), `atomic_config_update` (roster/service.rs:~287), `ActivityTracker::update_config` (activity.rs:~87)
5. No caller outside `team_config_store.rs` holds or derives the config.json write path
6. Audit all 8 `write_team_config` call sites in teams.rs (lines 955, 1584, 1624, 1667, 1852, 1967, 2527, 3110) — remove outer lock acquisition at each migration site

## Acceptance Criteria

- `TeamConfigStore` is the sole writer for config.json across all crates
- Finding 1 fixed: `event_loop.rs` reconcile passes a closure to `update()`, not a pre-read snapshot
- Findings 2+3 fixed: `bridge/team_config_sync.rs` uses `update()` for both init and merge paths; async callers wrapped in `spawn_blocking`
- Finding 4 fixed: `hook_watcher.rs` `auto_update_member_session_id` migrated to `update()`, stale-read eliminated
- Finding 5 fixed: `activity.rs` `ActivityTracker::update_config` replaced by `TeamConfigStore::update`
- Finding 8 fixed: `init.rs` `ensure_team_config` uses `create_or_update` (not bare `write_json_atomic`)
- Finding 9 fixed: `roster/service.rs` uses `update()` (fsync guaranteed by store)
- Both gold-standard helpers deleted (`teams.rs:3177`, `register.rs:252`)
- All config.json writes have lock + re-read + fsync + `atomic_swap`
- No double-lock: outer lock acquisitions removed at all migrated call sites
- Change-signaling preserved: call sites that previously returned bool/changed-state use `UpdateOutcome` to distinguish `Updated` vs `Unchanged`; no unnecessary writes introduced
- `update_async` wrapper used by bridge callers — no hand-rolled `spawn_blocking` at call sites; no async lock held across await boundary
- `cargo clippy -- -D warnings` clean
- Existing tests pass
- Add tests:
  - Concurrent `update()` calls: no data loss (exactly one mutation wins)
  - `update()` with `Ok(None)` closure: file unchanged, `Unchanged` returned
  - `create_or_update` on missing file: creates correctly
  - Fault injection: process-kill simulation after `.tmp` write, before `atomic_swap` — on restart, config.json is intact (`.tmp` is leftover, not corruption)

## References

- Audit source: rust-explorer output (2026-03-13)
- Design review: quality-mgr AM.7 (2026-03-13) — rust-qa + atm-qa combined findings
- Related issue: #721 (config.json member additions dropped — symptom of Finding 1)
- `crates/atm-core/src/io/atomic.rs` — `atomic_swap` implementation (exchange semantics)
