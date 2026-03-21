# Phase BC: SC-Observability Extraction

**Version**: 0.1
**Date**: 2026-03-21
**Status**: PLANNED
**Integration branch**: `integrate/phase-BC`
**Prerequisites**: `develop` at `103bd127` or later, with Phase BB merged to
`develop`
**Dependency graph**: `BC.1 -> BC.2 -> BC.3` (serial)
**Planning worktree**: `planning/sc-observability-extraction`

## Goal

Extract the three `sc-observability-*` crates from the agent-team-mail
workspace into a standalone repository published independently on crates.io.
The atm workspace then depends on the extracted crates via version pins.

## Why This Phase Exists

The `sc-observability` crates (`sc-observability-types`,
`sc-observability-otlp`, `sc-observability`) are general-purpose structured
logging and OTel pipeline utilities with no inherent dependency on ATM
messaging concepts. They happen to live in this workspace for historical
reasons. Extracting them:

- Makes them reusable by non-ATM projects without taking an atm-core dep
- Decouples their release cadence from atm releases
- Reduces the atm workspace size and build graph
- Reverses the current inverted dependency (atm-core owns types that
  sc-observability needs, which is backwards)

The composer crates (`sc-composer`, `sc-compose`) are out of scope for this
phase — they have much lighter coupling and can follow in a later phase.

## Canonical Roots for This Phase

- **Config root**: `~/.claude` (unchanged — not affected by this phase)
- **Extracted repo**: `github.com/randlee/sc-observability` (new)
- **Extracted repo target version**: `0.1.0` (independent semver from atm)

## Dependency Analysis Summary

### Current coupling (blockers)

| Coupling | Severity | Location |
|---|---|---|
| `sc-observability` → `agent-team-mail-core` (LogEventV1, SpanRefV1, ValidationError, new_log_event) | **HIGH** | `crates/sc-observability/src/lib.rs:9` |
| `sc-observability` → `agent-team-mail-core` (OtelHealthSnapshot, OtelLastError) | **MEDIUM** | `crates/sc-observability/src/lib.rs:23`, `health.rs:2` |
| `LogEventV1::default_spool_dir()` calls `atm-core::home::get_home_dir` | **MEDIUM** | `crates/atm-core/src/logging_event.rs:696` |
| `ATM_OTEL_*` env var prefix hardcoded in sc-observability-types | **LOW** | `crates/sc-observability-types/src/lib.rs:46-95` |
| `sc-compose` → `agent-team-mail-core` (get_home_dir only) | **LOW** | `crates/sc-compose/src/main.rs:243` |
| `sc-composer` → `agent-team-mail-core` (phantom dep — zero actual imports) | **NEGLIGIBLE** | `crates/sc-composer/Cargo.toml:13` |

### Dependency direction after extraction

```
sc-observability-types         ← leaf, no atm-* deps
       ^          ^
       |          |
       |     agent-team-mail-core (re-exports for compat)
       |          ^
sc-observability-otlp          |
       ^          |
       |     agent-team-mail-*  (CLI, daemon, mcp, tui)
sc-observability               |
       ^          |
       └──────────┘
```

### Who consumes sc-* crates from atm-* side

| sc-* crate | Consumed by |
|---|---|
| `sc-observability-types` | `agent-team-mail`, `agent-team-mail-daemon` |
| `sc-observability-otlp` | `agent-team-mail`, `agent-team-mail-daemon` |
| `sc-observability` | `agent-team-mail`, `agent-team-mail-daemon`, `agent-team-mail-mcp`, `agent-team-mail-tui`, `sc-compose` |
| `sc-composer` | `agent-team-mail`, `sc-compose` |

---

## Sprint BC.1 — Type Ownership Migration

**Goal**: Move the ATM-specific types that currently live in `atm-core` but
logically belong in `sc-observability-types` into the correct home. After this
sprint, `sc-observability` has no dependency on `agent-team-mail-core`.

### Deliverables

1. **Move `LogEventV1` module** from `crates/atm-core/src/logging_event.rs`
   into `crates/sc-observability-types/src/logging_event.rs`.
   - All types: `LogEventV1`, `LogEventV1Builder`, `SpanRefV1`,
     `ValidationError`, `new_log_event`, spool path helpers
   - Add `chrono` dep to `sc-observability-types/Cargo.toml`
   - `LogEventV1::default_spool_dir()` must not call `atm-core::home::get_home_dir`;
     make it accept `home_dir: &Path` as an explicit argument (or accept an
     env var name string). Only `default_spool_dir()` at line 696 calls it
     internally — all other spool path functions already accept explicit paths.

2. **Move `OtelHealthSnapshot` and `OtelLastError`** from
   `crates/atm-core/src/observability.rs` into
   `crates/sc-observability-types/src/health_types.rs`.
   - These are 44 lines of pure data structs with serde derives.
   - Add `pub mod health_types;` to `sc-observability-types/src/lib.rs`.

3. **Leave backward-compat re-export shims in atm-core**:
   - `crates/atm-core/src/logging_event.rs` → thin `pub use sc_observability_types::logging_event::*;`
   - `crates/atm-core/src/observability.rs` → thin `pub use sc_observability_types::health_types::*;`
   - Add `sc-observability-types` as a dep of `atm-core` in workspace `Cargo.toml`
   - Mark re-exports `#[deprecated = "import from sc_observability_types directly"]`
     to signal migration intent without breaking consumers

4. **Update `sc-observability` imports**:
   - Change `use agent_team_mail_core::logging_event::*` →
     `use sc_observability_types::logging_event::*` throughout
     `crates/sc-observability/src/`
   - Change `use agent_team_mail_core::observability::*` →
     `use sc_observability_types::health_types::*`
   - Remove `agent-team-mail-core` from `crates/sc-observability/Cargo.toml`

5. **Update publish ordering** in `release/publish-artifacts.toml`:
   - `sc-observability-types` must publish **before** `agent-team-mail-core`
   - Current: sc-observability-types=13, atm-core=10 (wrong after Phase BC.1)
   - New ordering: sc-observability-types=8, atm-core=10 (or equivalent)

### Acceptance Criteria

- `cargo test --workspace` passes
- `cargo clippy --all-targets -- -D warnings` passes
- Zero imports of `agent_team_mail_core` anywhere in `crates/sc-observability/`
- `crates/sc-observability/Cargo.toml` has no `agent-team-mail-core` dep
- `sc-observability-types` publish_order is lower than `agent-team-mail-core` in publish manifest
- All existing atm-* crate imports from `agent_team_mail_core::logging_event::*` continue to compile via re-export shims

### Files Changed

| File | Action |
|---|---|
| `crates/atm-core/src/logging_event.rs` | Replace with thin `pub use sc_observability_types::logging_event::*;` shim |
| `crates/atm-core/src/observability.rs` | Replace with thin `pub use sc_observability_types::health_types::*;` shim |
| `crates/atm-core/Cargo.toml` | Add `sc-observability-types` dep |
| `crates/sc-observability-types/src/lib.rs` | Add `pub mod logging_event; pub mod health_types;` |
| `crates/sc-observability-types/src/logging_event.rs` | New file (moved from atm-core) |
| `crates/sc-observability-types/src/health_types.rs` | New file (moved from atm-core) |
| `crates/sc-observability-types/Cargo.toml` | Add `chrono` dep |
| `crates/sc-observability/src/lib.rs` | Update imports × 2 |
| `crates/sc-observability/src/health.rs` | Update imports |
| `crates/sc-observability/Cargo.toml` | Remove `agent-team-mail-core` dep |
| `release/publish-artifacts.toml` | Fix publish_order for sc-observability-types |

---

## Sprint BC.2 — Coupling Cleanup

**Goal**: Remove the remaining minor couplings between sc-composer/sc-compose
and atm-core, and make `OtelConfig` env prefix configurable. After this
sprint, the three sc-observability crates are fully independent of any atm-*
crate at the source level.

### Deliverables

1. **Remove phantom dep from `sc-composer`**:
   - Delete `agent-team-mail-core` from `crates/sc-composer/Cargo.toml`
   - No code changes required (zero actual imports)

2. **Decouple `sc-compose` from `atm-core::home::get_home_dir`**:
   - Replace the single `get_home_dir()` call at `crates/sc-compose/src/main.rs:243`
     with a local implementation that reads `SC_HOME` (primary) or `HOME`/`USERPROFILE`
     (fallback) — keeping `ATM_HOME` as an additional fallback for backward
     compatibility during the transition period
   - Remove `agent-team-mail-core` from `crates/sc-compose/Cargo.toml`
   - Add `dirs = "5"` if the platform home-dir resolution needs it

3. **Add `OtelConfig::from_env_with_prefix(prefix: &str)`** to
   `sc-observability-types`:
   - Current `from_env()` reads 11 `ATM_OTEL_*` vars
   - New: `from_env_with_prefix(prefix)` accepts any prefix (e.g. `"SC_OTEL_"`)
   - Keep `from_env()` as `from_env_with_prefix("ATM_OTEL_")` for backward compat
   - Document the new method for non-ATM consumers

4. **Verify full independence**: Run `cargo tree -p sc-observability` and
   `cargo tree -p sc-observability-types` and confirm no `agent-team-mail-*`
   entries appear in the dependency tree.

### Acceptance Criteria

- `cargo test --workspace` passes
- `cargo clippy --all-targets -- -D warnings` passes
- `cargo tree -p sc-observability` shows no `agent-team-mail-*` deps
- `cargo tree -p sc-observability-types` shows no `agent-team-mail-*` deps
- `cargo tree -p sc-observability-otlp` shows no `agent-team-mail-*` deps
- `sc-compose` resolves home dir correctly with no atm-core dep

### Files Changed

| File | Action |
|---|---|
| `crates/sc-composer/Cargo.toml` | Remove `agent-team-mail-core` dep |
| `crates/sc-compose/src/main.rs` | Replace `get_home_dir()` with local impl |
| `crates/sc-compose/Cargo.toml` | Remove `agent-team-mail-core`; add `dirs` if needed |
| `crates/sc-observability-types/src/lib.rs` | Add `from_env_with_prefix()` |

---

## Sprint BC.3 — Repo Split and Publish

**Goal**: Create the standalone `sc-observability` repository, publish the
three crates to crates.io under independent versioning, and update the atm
workspace to consume them via version pins.

### Deliverables

1. **Create `sc-observability` repo** at `github.com/randlee/sc-observability`:
   ```
   sc-observability/
     Cargo.toml            (workspace: members = ["crates/*"])
     crates/
       sc-observability-types/    (publish_order: 1)
       sc-observability-otlp/     (publish_order: 2)
       sc-observability/          (publish_order: 3)
     .github/workflows/
       ci.yml              (cargo test + clippy)
       release.yml         (workflow_dispatch publish to crates.io)
     README.md
   ```
   - Workspace version: `0.1.0` (independent from atm versioning)
   - `sc-observability` crate-level deps reference `sc-observability-types`
     and `sc-observability-otlp` via path deps within the new workspace
   - All crates get their own independent `CHANGELOG.md`

2. **Publish to crates.io** in order:
   - `sc-observability-types@0.1.0`
   - `sc-observability-otlp@0.1.0`
   - `sc-observability@0.1.0`

3. **Update atm workspace**:
   - Remove `crates/sc-observability`, `crates/sc-observability-types`,
     `crates/sc-observability-otlp` from workspace `members` in root `Cargo.toml`
   - Update workspace dep entries from path deps to crates.io version pins:
     ```toml
     sc-observability = "0.1"
     sc-observability-types = "0.1"
     sc-observability-otlp = "0.1"
     ```
   - Remove `path = "crates/sc-observability*"` entries
   - Run `cargo update` to regenerate `Cargo.lock`
   - Remove the three crate directories from the repo

4. **Update `release/publish-artifacts.toml`**: Remove the three sc-observability
   entries (they now publish from their own repo).

5. **CI validation**: Confirm atm workspace CI passes with crates.io pinned deps.

### Acceptance Criteria

- `cargo build --workspace` in atm workspace passes with crates.io pins
- `cargo test --workspace` passes
- Three crates visible on crates.io at `0.1.0`
- No `crates/sc-observability*` directories remain in atm workspace
- GitHub Actions CI passes on new sc-observability repo

### Files Changed (atm workspace)

| File | Action |
|---|---|
| `Cargo.toml` (workspace root) | Remove 3 path members; update workspace deps to version pins |
| `Cargo.lock` | Regenerated via `cargo update` |
| `release/publish-artifacts.toml` | Remove 3 sc-observability entries |
| `crates/sc-observability/` | Deleted |
| `crates/sc-observability-types/` | Deleted |
| `crates/sc-observability-otlp/` | Deleted |

---

## Risk Assessment

| Risk | Severity | Mitigation |
|---|---|---|
| Re-export shims break a downstream atm-* crate | LOW | Shims use `pub use *` — all existing imports continue to compile; deprecation warnings only |
| Publish ordering regression (sc-observability-types must precede atm-core) | MEDIUM | Update publish_order in BC.1; verify with dry-run before BC.3 |
| `LogEventV1::default_spool_dir()` home injection breaks tests | MEDIUM | Change signature to accept `home_dir: &Path` — tests already pass explicit paths; only default resolution changes |
| crates.io 403 on publish (issue #399 — GH Actions IP block) | KNOWN | Verify manually post-publish; create GitHub Release manually if needed |
| Stale `Cargo.lock` after workspace dep change | LOW | Run `cargo generate-lockfile` after removing path deps |

---

## Out of Scope

- `sc-composer` / `sc-compose` extraction — covered in Phase BD below
- Changes to `OtelConfig` env var names (breaking change for users) — only
  `from_env_with_prefix()` is added; existing `ATM_OTEL_*` names are preserved
- atm-core re-export shim removal — can be done in a follow-up cleanup sprint
  once all consumers have migrated their imports

---

## Phase BD: SC-Compose Extraction

Phase BD plan is in its own standalone document:
[`docs/phase-bd-sc-compose-extraction.md`](./phase-bd-sc-compose-extraction.md)
