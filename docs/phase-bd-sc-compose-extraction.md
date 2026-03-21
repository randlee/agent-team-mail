# Phase BD: SC-Compose Extraction

**Version**: 0.1
**Date**: 2026-03-21
**Status**: PLANNED
**Integration branch**: `integrate/phase-BD`
**Prerequisites**: Phase BC merged to `develop` (sc-observability on crates.io at `0.1.x`)
**Dependency graph**: `BD.1` (single sprint)
**Planning worktree**: `planning/sc-observability-extraction`

## Goal

Extract `sc-composer` and `sc-compose` from the agent-team-mail workspace into
a standalone repository. After Phase BC, both crates will have zero atm-*
dependencies — extraction is purely a repo logistics sprint.

## Why This Phase Exists

After Phase BC:
- `sc-composer` has no atm-* deps (phantom dep removed in BC.2)
- `sc-compose` has no atm-* deps (get_home_dir coupling removed in BC.2)
- `sc-compose` depends on `sc-observability` via crates.io pin (after BC.3)

The two crates are general-purpose Jinja2 template tooling with no inherent
connection to ATM messaging. Extracting them:

- Allows independent release cadence
- Makes the template tooling reusable by other projects
- Further reduces atm workspace build graph

## Crate Summary

| Crate | Role | External deps |
|---|---|---|
| `sc-composer` | Jinja2 rendering library | minijinja, serde, serde_json, serde_yaml, thiserror |
| `sc-compose` | CLI binary wrapping sc-composer | anyhow, clap, chrono, serde, serde_json, serde_yaml, ulid |

`sc-compose` depends on `sc-composer` and `sc-observability` (for OTel
tracing of template renders). After BC.3, `sc-observability` is a crates.io dep.

## Dependency State After Phase BC

```
sc-composer            (leaf — no atm-* deps, no sc-observability-* deps)
       ^
       |
sc-compose             (depends on: sc-composer, sc-observability@crates.io)
```

No blockers. Zero coupling to atm-* crates at this point.

---

## Sprint BD.1 — Repo Split and Publish

**Goal**: Create `github.com/randlee/sc-compose` repo, publish both crates to
crates.io, and update atm workspace to consume them via version pins.

### Deliverables

1. **Create `sc-compose` repo** at `github.com/randlee/sc-compose`:
   ```
   sc-compose/
     Cargo.toml            (workspace: members = ["crates/*"])
     crates/
       sc-composer/        (publish_order: 1)
       sc-compose/         (publish_order: 2)
     .github/workflows/
       ci.yml              (cargo test + clippy)
       release.yml         (workflow_dispatch publish to crates.io)
     README.md
   ```
   - Workspace version: `0.1.0` (independent from atm versioning)
   - `sc-compose` dep on `sc-observability` uses crates.io pin: `sc-observability = "0.1"`
   - `sc-compose` dep on `sc-composer` uses path dep within the new workspace

2. **Publish to crates.io** in order:
   - `sc-composer@0.1.0`
   - `sc-compose@0.1.0`

3. **Update atm workspace**:
   - Remove `crates/sc-composer` and `crates/sc-compose` from workspace `members`
   - Update workspace dep entries from path deps to crates.io version pins:
     ```toml
     sc-composer = "0.1"
     sc-compose = "0.1"
     ```
   - Run `cargo update` to regenerate `Cargo.lock`
   - Remove the two crate directories from the repo

4. **Update `release/publish-artifacts.toml`**: Remove `sc-composer` and
   `sc-compose` entries (they now publish from their own repo).

5. **CI validation**: Confirm atm workspace CI passes with crates.io pinned deps.

### Acceptance Criteria

- `cargo build --workspace` in atm workspace passes with crates.io pins
- `cargo test --workspace` passes
- `sc-composer` and `sc-compose` visible on crates.io at `0.1.0`
- No `crates/sc-composer` or `crates/sc-compose` directories remain in atm workspace
- GitHub Actions CI passes on new `sc-compose` repo

### Files Changed (atm workspace)

| File | Action |
|---|---|
| `Cargo.toml` (workspace root) | Remove 2 path members; update workspace deps to version pins |
| `Cargo.lock` | Regenerated via `cargo update` |
| `release/publish-artifacts.toml` | Remove `sc-composer` and `sc-compose` entries |
| `crates/sc-composer/` | Deleted |
| `crates/sc-compose/` | Deleted |

## Risk Assessment

| Risk | Severity | Mitigation |
|---|---|---|
| `sc-compose` at `0.1.0` takes `sc-observability = "0.1"` — must exist on crates.io first | LOW | Hard dependency on Phase BC.3 completing successfully |
| crates.io 403 on publish (issue #399) | KNOWN | Manual GitHub Release fallback as per release procedure |
| atm CLI uses `sc-compose` binary in CI/tests | LOW | Verify CI workflows don't shell out to `sc-compose` binary; it is a dev tool, not a runtime dep |

## Out of Scope

- atm-core re-export shim removal (deferred cleanup)
- `sc-observability` minor version bumps after extraction (independent repo cadence)
- Any new features for sc-composer/sc-compose (extraction only, no changes)
