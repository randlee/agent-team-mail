# Phase AG Planning: sc-composer Full Implementation + sc-compose CLI

**Status:** Draft
**Target version:** v0.42.0
**Integration branch:** `integrate/phase-AG`
**Based on:** Phase AF (`integrate/phase-AF` → develop)

---

## Goal

Deliver the `sc-composer` library and `sc-compose` CLI as fully working products. The
library crate was committed in Phase AF (v0.41.0) as a skeleton — types are defined
but `compose()` and `validate()` return `NotImplemented`. This phase implements all
functional requirements from `docs/sc-composer/requirements.md`.

---

## Reference Documents

- `docs/sc-composer/requirements.md` — canonical FRs and NFRs
- `docs/sc-composer/architecture.md` — module breakdown and data model
- `docs/cross-platform-guidelines.md` — mandatory cross-platform patterns
- `docs/requirements.md` — core ATM requirements (for any integration points)

---

## Scope

Two parallel tracks:

| Track | Deliverable | Crate |
|-------|-------------|-------|
| A | `sc-composer` library — full implementation | `crates/sc-composer` |
| B | `sc-compose` binary — CLI for render/resolve/validate | `crates/sc-compose` |

Track B depends on Track A reaching a stable API surface, so sprints are sequenced:
AG.1–AG.4 (library) then AG.5 (binary).

---

## Sprint Plan

### AG.1 — Frontmatter + Context

**Goal:** YAML frontmatter parsing and variable context merging.

**Deliverables:**
- `src/frontmatter.rs`: parse optional YAML front matter from file text
  - `required_variables: Vec<String>`
  - `defaults: BTreeMap<String, String>`
  - `metadata: BTreeMap<String, serde_json::Value>`
  - Must be gracefully absent (no frontmatter → defaults all empty)
- `src/context.rs`: variable merge with precedence `input > env > defaults`
  - Track `VariableSource` per variable
  - Emit `Diagnostic` for unknown variables per `UnknownVariablePolicy`
- Unit tests: parse/no-parse, precedence, policy enforcement

**Dependencies:** none

---

### AG.2 — Resolver + Include Expansion

**Goal:** Profile file resolution and `@<path>` include expansion.

**Deliverables:**
- `src/resolver.rs`: resolve profile file by `RuntimeKind` + `kind` (agent/command/skill)
  - Probe order per architecture doc §2.1
  - Supported root directories: `.claude`, `.codex`, `.gemini`, `.opencode`, `.agents`
  - Explicit path override bypasses probe (subject to root safety check)
- `src/include.rs`: expand `@<path>` directives recursively
  - Confined to declared root (`policy.allowed_roots`)
  - Cycle detection (track include stack)
  - Max depth guard (`policy.max_include_depth`, default 8)
  - Each included file can also have frontmatter (merge into parent context)
- Unit tests: resolver probe order, explicit path, include expansion, cycle detection, depth limit

**Dependencies:** AG.1 (for frontmatter parsing in included files)

---

### AG.3 — Render + Validate

**Goal:** Jinja2 rendering and variable validation.

**Deliverables:**
- `src/render.rs`: Jinja2 rendering facade
  - Use `minijinja` crate (pure Rust, no C bindings)
  - Strict undefined mode (undefined variable → error)
  - Context injection from `context.rs` output
- `src/validate.rs`: required variable check + unknown variable policy
  - Cross-reference template variables vs provided context
  - Emit typed `Diagnostic` list (code, message, path)
- `src/diagnostics.rs`: `Diagnostic` type + JSON serialization
  - Stable diagnostic codes: `MISSING_VAR`, `UNKNOWN_VAR`, `INCLUDE_CYCLE`,
    `INCLUDE_DEPTH`, `INCLUDE_NOT_FOUND`, `RENDER_ERROR`, `ROOT_ESCAPE`
- Update `compose()` and `validate()` in `lib.rs` to call real implementations
- Unit tests: render with vars, strict mode, missing var, unknown var policy, JSON diagnostics

**New workspace dependency:** `minijinja = "2"` in `[workspace.dependencies]`

**Dependencies:** AG.1, AG.2

---

### AG.4 — Pipeline Integration + Tests

**Goal:** Wire all modules into `compose()` end-to-end; comprehensive integration tests.

**Deliverables:**
- `compose()` pipeline (per architecture §4):
  1. resolve → 2. read + parse frontmatter → 3. expand includes → 4. merge context
  5. validate → 6. render → 7. return `ComposeResult`
- `validate()` pipeline: same through step 5, no render
- `src/pipeline.rs`: block composition — `agent` + `guidance` + `user` blocks with
  fixed ordering and separator handling
- Integration tests in `tests/integration.rs`:
  - End-to-end: file-mode compose with vars
  - End-to-end: profile-mode compose (mock runtime dir)
  - Error paths: missing required var, cycle, out-of-root escape
  - Cross-platform: use `TMPDIR` / `std::env::temp_dir()`, not hardcoded paths
- `cargo clippy -- -D warnings` clean

**Dependencies:** AG.1, AG.2, AG.3

---

### AG.5 — `sc-compose` Binary

**Goal:** Standalone CLI binary implementing render/resolve/validate/frontmatter-init.

**Deliverables:**
- New crate `crates/sc-compose/` with binary target `sc-compose`
  - `Cargo.toml`: workspace version, description, binary name `sc-compose`
  - Added to workspace `members`
  - Added to `release.yml` inventory and `dependency_order`
- Commands:
  - `sc-compose render [OPTIONS] <template>` — render to stdout
  - `sc-compose resolve [OPTIONS] <agent>` — print resolved file path
  - `sc-compose validate [OPTIONS] <template>` — validate only, exit 2 on error
  - `sc-compose frontmatter-init <file>` — write default frontmatter stub
- Global flags: `--runtime`, `--root`, `--var key=val`, `--env-prefix`, `--json`
- Exit codes: 0 success, 2 validation/render failure, 3 usage error
- `--json` mode emits structured diagnostics matching `Diagnostic` JSON schema
- Binary smoke tests: round-trip render, exit codes, JSON mode
- Added to build step in `release.yml`: `--bin sc-compose`

**Dependencies:** AG.1–AG.4

---

## Version and Release

- Version: `0.42.0` (bump from `0.41.0` on integration branch before final PR)
- Release: `publisher` named teammate handles crates.io + GitHub Release + Homebrew
- `sc-compose` binary added to Homebrew formula for direct installation

---

## Testing Strategy

- All tests use `std::env::temp_dir()` or `tempfile::TempDir` — no hardcoded paths
- No `HOME`, `USERPROFILE`, `~` in test paths — use `ATM_HOME` pattern where needed
- Cross-platform CI: ubuntu-latest, macos-latest, windows-latest for all test jobs
- `cargo clippy -- -D warnings` required to pass before any PR

---

## Dependency Graph

```
AG.1 (frontmatter + context)
  └── AG.2 (resolver + include)
        └── AG.3 (render + validate)
              └── AG.4 (pipeline + integration tests)
                    └── AG.5 (sc-compose binary)
```

Sequential — no parallel tracks within this phase. AG.1–AG.4 can be batched as
two dev passes if arch-ctm prefers (AG.1+AG.2 pass 1, AG.3+AG.4+AG.5 pass 2).

---

## Success Criteria

1. `sc-composer::compose()` works end-to-end for file-mode and profile-mode requests.
2. `sc-compose render` produces correct output from a `.j2` template with vars.
3. `sc-compose validate` exits 2 when a required variable is missing.
4. All CI checks green on all three platforms.
5. `sc-composer` and `sc-compose` publish successfully to crates.io as v0.42.0.
