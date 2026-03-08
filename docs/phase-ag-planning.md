# Phase AG Planning: sc-composer Full Implementation + sc-compose CLI

**Status:** Draft
**Target version:** v0.42.0
**Integration branch:** `integrate/phase-AG`
**Based on:** Phase AF (`integrate/phase-AF` → develop)
**Canonical docs:** `docs/sc-composer/requirements.md` (requirements) |
`docs/sc-composer/architecture.md` (design) |
`docs/test-plan-phase-AG.md` (test plan)

---

## Goal

Deliver a working `sc-composer` library and `sc-compose` CLI published to
crates.io as v0.42.0. External projects (scmux, others) consume from crates.io
— no path dependencies.

This plan is an execution plan; detailed product semantics must be sourced from
the dedicated sc-composer requirements/architecture docs above.

ATM integration in this phase must be library-first:
- `atm` calls `sc-composer` APIs directly.
- `atm` must not execute `sc-compose` through shell/subprocess wrappers for
  core spawn/composition behavior.
- `atm` composition behavior must match `sc-compose` semantics.

The library skeleton is already in the workspace (v0.41.0) but `compose()` and
`validate()` return `NotImplemented`. This phase makes them real.

---

## Primary Consumers After Publish

| Consumer | How |
|----------|-----|
| **scmux** | `sc-composer = "0.42.0"` in Cargo.toml; `sc-compose` binary via Homebrew |
| **atm teams spawn** | `sc-composer` workspace dep; renders `.md.j2` templates before passing to runtime |
| **CI/dev workflows** | `sc-compose render` binary from Homebrew or `cargo install` |

---

## Sprint Plan

### AG.0 — Daemon Stale-Process Hygiene (Issue #539, pre-AG gate)

**Goal:** prevent stale `atm-daemon` processes from test worktrees and ensure
CLI auto-start/connect does not silently bind to stale/incorrect daemon
instances.

**Design decision (for implementation):**
- Do **not** perform broad cross-scope/global process kill on production daemon
  startup.
- Enforce correctness in current ATM scope via lock-holder liveness + daemon
  identity validation (PID/exe/home-scope) during client connect/startup.
- Add explicit test-harness cleanup behavior for test-scoped daemon processes.

**Deliverables:**
- `atm-daemon` lock/identity hardening:
  - lock metadata must include holder PID and executable path (or equivalent
    process identity metadata) for stale lock recovery in same scope.
  - startup must reclaim lock only when prior holder is confirmed dead.
- CLI/daemon connect hardening:
  - daemon-backed commands must validate that responding daemon matches current
    expected scope/identity contract before accepting it as healthy.
  - mismatch/stale daemon response must trigger actionable restart path.
- Test lifecycle hardening:
  - `DaemonProcessGuard` cleanup must cover panic/aborted test paths where
    possible and register best-effort teardown hooks.
  - add test-only stale-daemon sweep utility/fixture so new tests do not
    accumulate detached daemon processes across runs.

**Tests:**
- same-scope stale lock PID dead -> startup recovers and starts one daemon.
- same-scope stale/identity-mismatch daemon on socket -> CLI detect + restart.
- test abort/panic paths do not leave unbounded daemon processes.
- repeated test runs with isolated `ATM_HOME` do not accumulate stale daemons.

---

### AG.1 — Library MVP: Frontmatter + Render (file mode)

**Goal:** Implement working `compose()` and `validate()` for file-mode requests.
This is the unlock — once this sprint ships, external projects can start using the library.

**Deliverables in `crates/sc-composer/src/`:**

- `frontmatter.rs` — parse optional YAML frontmatter from file text
  - fields: `required_variables: Vec<String>`, `defaults: BTreeMap<String, String>`
  - absent frontmatter → validate/render diagnostics must recommend
    `sc-compose frontmatter-init <file>.j2`
- `context.rs` — variable merge with precedence `input > env > defaults`
  - track `VariableSource` per variable
  - emit `Diagnostic` for missing required vars or unknown/undeclared vars per policy
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
  - runtime root dirs: `.claude`, `.codex`, `.gemini`, `.opencode`, `.agents/agents`
    plus legacy `.agents` fallback for `kind=agent`
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
  - `sc-compose init` — repo bootstrap (`.prompts/`, `.gitignore`, template scan hints)
- Global flags: `--runtime`, `--root`, `--var key=val`, `--env-prefix`, `--json`
- Exit codes: 0 success, 2 validation/render failure, 3 usage/config error
- `--json` flag: emit `Diagnostic` list as JSON on stderr, rendered text on stdout
- Smoke tests: render round-trip, missing var exits 2, `--json` parses cleanly

---

### AG.4 — `atm teams spawn` Integration + Full Integration Tests

**Goal:** Wire sc-composer into `atm teams spawn`; comprehensive library integration tests.

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
              └── AG.4 (atm teams spawn + integration tests)

AG.0 (stale-daemon hygiene) runs as a pre-AG gate before AG.1.
```

AG.3 can start after AG.1 (binary only needs file-mode compose); AG.2 and AG.3
can overlap if arch-ctm batches them.

---

## Version and Release

- Version: `0.42.0` (bump on integrate/phase-AG before final PR)
- `sc-composer` and `sc-compose` are version-locked to ATM in this phase and
  are released together in the same ATM publish cycle.
- Both `sc-composer` and `sc-compose` publish to crates.io (co-released).
- `sc-compose` binary added to Homebrew formula and release archives (4 targets)
- ATM install/upgrade paths must install/upgrade the paired `sc-compose` CLI in
  the same release step (no separate/manual compose upgrade path).
- After publish: scmux adds `sc-composer = "0.42.0"` — no ATM sprint needed

---

## Success Criteria

1. `sc_composer::compose()` renders a `.md.j2` template with variable injection. `NotImplemented` is gone.
2. `sc-compose render --var role=team-lead template.md.j2` prints rendered text.
3. `sc-compose validate` exits 2 when a required variable is missing.
4. `atm teams spawn --system-prompt agent.md.j2 --var project=scmux` renders and injects the prompt.
5. All CI green on ubuntu, macos, windows.
6. `sc-composer = "0.42.0"` and `sc-compose = "0.42.0"` available on crates.io.
