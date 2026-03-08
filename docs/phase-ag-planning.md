# Phase AG Planning: sc-composer Full Implementation + sc-compose CLI

**Status:** Draft
**Target version:** v0.42.0
**Integration branch:** `integrate/phase-AG`
**Based on:** Phase AF (`integrate/phase-AF` → develop)

---

## Goal

Deliver a working `sc-composer` library and `sc-compose` CLI published to
crates.io as v0.42.0. External projects (scmux, others) consume from crates.io
— no path dependencies.

The library skeleton is already in the workspace (v0.41.0) but `compose()` and
`validate()` return `NotImplemented`. This phase makes them real.

---

## Primary Consumers After Publish

| Consumer | How |
|----------|-----|
| **scmux** | `sc-composer = "0.42.0"` in Cargo.toml; `sc-compose` binary via Homebrew |
| **atm spawn** | `sc-composer` workspace dep; renders `.md.j2` templates before passing to runtime |
| **CI/dev workflows** | `sc-compose render` binary from Homebrew or `cargo install` |

---

## Sprint Plan

### AG.1 — Library MVP: Frontmatter + Render (file mode)

**Goal:** Implement working `compose()` and `validate()` for file-mode requests.
This is the unlock — once this sprint ships, external projects can start using the library.

**Deliverables in `crates/sc-composer/src/`:**

- `frontmatter.rs` — parse optional YAML frontmatter from file text
  - fields: `required_variables: Vec<String>`, `defaults: BTreeMap<String, String>`
  - absent frontmatter → all empty (graceful)
- `context.rs` — variable merge with precedence `input > env > defaults`
  - track `VariableSource` per variable
  - emit `Diagnostic` for missing required vars or unknown vars per policy
- `render.rs` — Jinja2 rendering via `minijinja` (pure Rust)
  - strict undefined mode — undefined variable → `ComposerError`
  - context injected from merged variable map
- `lib.rs` — implement `compose()`: read file → parse frontmatter → merge context → render → return `ComposeResult`
- `lib.rs` — implement `validate()`: same through context merge, no render, return `ValidationReport`
- Remove `ComposerError::NotImplemented` variant

**New workspace dep:** `minijinja = "2"` in `Cargo.toml` `[workspace.dependencies]`

**Tests:**
- plain text (no frontmatter, no vars) → passthrough
- `.j2` file with `{{ var }}` and `vars_input` → correct substitution
- missing required var → `ComposerError` with diagnostic code `MISSING_VAR`
- unknown var, `policy = Error` → error; `policy = Warn` → warning in result; `policy = Ignore` → silent
- frontmatter defaults apply when input omits the key
- `validate()` on missing var → `ValidationReport { ok: false, errors: [..] }`

---

### AG.2 — Resolver + Include Expansion

**Goal:** Profile-mode file resolution and `@<path>` include expansion.

**Deliverables in `crates/sc-composer/src/`:**

- `resolver.rs` — resolve profile file by `RuntimeKind` + `kind` (agent/command/skill)
  - probe order within each candidate dir: `<name>.md.j2` → `<name>.md` → `<name>.j2`
  - runtime root dirs: `.claude`, `.codex`, `.gemini`, `.opencode`, `.agents`
  - explicit `template_path` bypasses probe (root safety check still applies)
- `include.rs` — expand `@<path>` directives recursively
  - confined to `policy.allowed_roots`
  - cycle detection via include stack
  - max depth guard (`policy.max_include_depth`, default 8)
  - included files may have their own frontmatter (merged into parent context)
- Update `compose()` to support `mode = Profile` — run resolver before render

**Tests:** probe order, explicit path override, include expansion, cycle detection,
depth limit, out-of-root escape rejected with `ROOT_ESCAPE` diagnostic

---

### AG.3 — `sc-compose` Binary

**Goal:** Standalone CLI binary. Thin wrapper over the library. Installable via
`cargo install sc-compose` and Homebrew.

**Deliverables:**

- New crate `crates/sc-compose/`
  - `Cargo.toml`: workspace version, description, binary name `sc-compose`
  - Added to workspace `members`
  - Added to `release.yml` inventory and `dependency_order`
  - Added to build step in `release.yml` (`--bin sc-compose`)
  - Added to Homebrew formula archives
- Commands:
  - `sc-compose render [OPTIONS] <template>` — render to stdout
  - `sc-compose resolve [OPTIONS] <agent>` — print resolved file path
  - `sc-compose validate [OPTIONS] <template>` — exit 2 on error
  - `sc-compose frontmatter-init <file>` — write default frontmatter stub
- Global flags: `--runtime`, `--root`, `--var key=val`, `--env-prefix`, `--json`
- Exit codes: 0 success, 2 validation/render failure, 3 usage/config error
- `--json` flag: emit `Diagnostic` list as JSON on stderr, rendered text on stdout
- Smoke tests: render round-trip, missing var exits 2, `--json` parses cleanly

---

### AG.4 — `atm spawn` Integration + Full Integration Tests

**Goal:** Wire sc-composer into `atm spawn`; comprehensive library integration tests.

**Deliverables in `crates/atm/`:**

- `crates/atm/Cargo.toml`: add `sc-composer.workspace = true` and add to `[workspace.dependencies]`
- `runtime_adapter.rs` / `spawn.rs`: when `--system-prompt` points to a `.md.j2` file,
  call `sc_composer::compose()` with spawn vars (`team`, `agent`, `runtime`, `model`, `cwd`),
  write rendered text to a temp file, pass temp path to runtime adapter
- Plain `.md` files: passthrough (backwards compat — no rendering)
- `SpawnSpec` extended with `prompt_vars: BTreeMap<String, String>` for additional caller-supplied vars

**Library integration tests in `crates/sc-composer/tests/integration.rs`:**
- file-mode end-to-end: `.md.j2` with vars → correct rendered output
- profile-mode end-to-end: mock runtime dir → resolver finds correct file → renders
- include expansion end-to-end: base template includes shared file → merged output
- error paths: missing required var, cycle, out-of-root
- cross-platform: all paths via `std::env::temp_dir()` or `tempfile::TempDir`

---

## Sprint Dependency Graph

```
AG.1 (library MVP — working compose())
  └── AG.2 (resolver + includes)
        └── AG.3 (sc-compose binary)  ← also depends on AG.1 for core API
              └── AG.4 (atm spawn + integration tests)
```

AG.3 can start after AG.1 (binary only needs file-mode compose); AG.2 and AG.3
can overlap if arch-ctm batches them.

---

## Version and Release

- Version: `0.42.0` (bump on integrate/phase-AG before final PR)
- Both `sc-composer` and `sc-compose` publish to crates.io
- `sc-compose` binary added to Homebrew formula and release archives (4 targets)
- After publish: scmux adds `sc-composer = "0.42.0"` — no ATM sprint needed

---

## Success Criteria

1. `sc_composer::compose()` renders a `.md.j2` template with variable injection. `NotImplemented` is gone.
2. `sc-compose render --var role=team-lead template.md.j2` prints rendered text.
3. `sc-compose validate` exits 2 when a required variable is missing.
4. `atm spawn --system-prompt agent.md.j2 --var project=scmux` renders and injects the prompt.
5. All CI green on ubuntu, macos, windows.
6. `sc-composer = "0.42.0"` and `sc-compose = "0.42.0"` available on crates.io.
