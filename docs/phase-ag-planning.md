# Phase AG Planning: sc-composer Full Implementation + sc-compose CLI

**Status:** Draft
**Target version:** v0.42.0
**Integration branch:** `integrate/phase-AG`
**Based on:** Phase AF (`integrate/phase-AF` → develop)

---

## Goal

Deliver a working `sc-composer` library and `sc-compose` CLI. The immediate
consumer is **scmux** (`/Users/randlee/Documents/github/scmux`), which launches
up to 30 Claude Code agents per project assignment and needs per-agent prompt
composition at launch time.

Secondary consumers: `atm spawn` (to replace raw `--system-prompt` path with
composed templates), and any non-Rust workflow using the `sc-compose` binary.

---

## Primary Use Case: scmux Team Launch

When a project is assigned to a team, scmux daemon (`scheduler.rs`) calls
`tmux::start_session()` with a tmuxp JSON config. Each pane currently launches:

```bash
claude --profile team-lead
```

With sc-composer integration, this becomes:

```bash
claude --profile team-lead --append-system-prompt /tmp/scmux-rendered/team-lead.md
```

Where `team-lead.md` is rendered from a template with project-scoped variables:
- `project` — assigned project name
- `team` — team name
- `role` — agent role (team-lead, arch-ctm, etc.)
- `cwd` — working directory
- any project-specific vars from session config

**scmux integration points:**
- `crates/scmux-daemon/src/scheduler.rs` — add render step before `start_session()`
- `crates/scmux-daemon/Cargo.toml` — add `sc-composer` path dependency
- Session config (tmuxp JSON): add optional `prompt_template` and `prompt_vars` per pane

---

## Reference Documents

- `docs/sc-composer/requirements.md` — canonical FRs and NFRs
- `docs/sc-composer/architecture.md` — module breakdown and data model
- `/Users/randlee/Documents/github/scmux/crates/scmux-daemon/src/scheduler.rs`
- `/Users/randlee/Documents/github/scmux/docs/example-session.json`
- `docs/cross-platform-guidelines.md` — mandatory cross-platform patterns

---

## Sprint Plan

### AG.1 — Library MVP: Frontmatter + Render (file mode)

**Goal:** Implement `compose()` end-to-end for the simplest case — render a
`.md` or `.md.j2` file with variable injection. This is the minimum scmux needs.

**Deliverables in `crates/sc-composer/src/`:**

- `frontmatter.rs`: parse optional YAML frontmatter from file text
  - `required_variables: Vec<String>`
  - `defaults: BTreeMap<String, String>`
  - frontmatter absent → empty defaults (graceful)
- `context.rs`: variable merge with precedence `input > env > defaults`
  - track `VariableSource` per variable
  - emit `Diagnostic` for missing required vars or unknown vars per policy
- `render.rs`: Jinja2 rendering via `minijinja` (pure Rust, no C deps)
  - strict undefined mode — undefined variable → error
  - context injected from `context.rs` output
- `lib.rs`: implement `compose()` — parse frontmatter, merge context, render, return `ComposeResult`
- `lib.rs`: implement `validate()` — same through context merge, no render

**New workspace dependency:** `minijinja = "2"` in `[workspace.dependencies]`

**Tests:**
- render plain text (no frontmatter, no vars) → passthrough
- render `.j2` with vars injected
- missing required var → `ComposerError` with diagnostic
- unknown var policy: Error/Warn/Ignore
- frontmatter defaults applied when input omits the key

**Definition of done:** `sc_composer::compose()` returns rendered text for
a file-mode request with variables. `ComposerError::NotImplemented` gone.

---

### AG.2 — scmux Integration

**Goal:** Wire sc-composer into scmux daemon so agent prompts are rendered at
team launch time.

**Deliverables in scmux:**

- `crates/scmux-daemon/Cargo.toml`: add `sc-composer = { path = "../../../agent-team-mail/crates/sc-composer" }` (path dep; switches to version dep after v0.42.0 publish)
- Extend tmuxp pane config (in `docs/example-session.json` and relevant scmux config structs) to support optional fields:
  ```json
  {
    "pane": "team-lead",
    "prompt_template": "docs/.prompts/team-lead.md.j2",
    "prompt_vars": { "project": "scmux", "role": "team-lead" }
  }
  ```
- `scheduler.rs`: before `start_session()`, for each pane with `prompt_template`:
  1. Call `sc_composer::compose(ComposeRequest { ... })` with merged vars
  2. Write rendered text to a temp file (e.g. `$TMPDIR/scmux-prompts/<session>-<agent>.md`)
  3. Inject `--append-system-prompt <path>` into the pane's shell command list
- Rendered prompt files are ephemeral — written at launch, not persisted in DB
- If `prompt_template` absent on a pane, launch proceeds unchanged (backwards compat)

**Tests:**
- Unit test: pane with template → shell command includes `--append-system-prompt`
- Unit test: pane without template → shell command unchanged
- Integration test: end-to-end render + inject for a real `.md.j2` template

---

### AG.3 — Resolver + Include Expansion

**Goal:** Profile-mode resolution and `@<path>` include expansion (needed for
multi-file prompt compositions with shared includes).

**Deliverables in `crates/sc-composer/src/`:**

- `resolver.rs`: resolve profile file by `RuntimeKind` + `kind` (agent/command/skill)
  - probe order: `.md.j2` → `.md` → `.j2` per architecture doc §2.1
  - runtime root dirs: `.claude`, `.codex`, `.gemini`, `.opencode`, `.agents`
  - explicit path override bypasses probe
- `include.rs`: expand `@<path>` directives recursively
  - confined to declared root (policy.allowed_roots)
  - cycle detection via include stack
  - max depth guard (policy.max_include_depth, default 8)
  - included files may have their own frontmatter (merged into parent context)
- Update `compose()` to support `mode=Profile` — resolver runs before render
- Wire include expansion into file-read step

**Tests:** probe order, explicit path, include cycle detection, depth limit,
out-of-root escape rejection

---

### AG.4 — `sc-compose` Binary

**Goal:** Standalone CLI binary. Thin wrapper over sc-composer library.

**Deliverables:**

- New crate `crates/sc-compose/` with binary target `sc-compose`
  - `Cargo.toml`: workspace version, description, binary name `sc-compose`
  - Added to workspace `members` and `release.yml` inventory + `dependency_order`
- Commands:
  - `sc-compose render [OPTIONS] <template>` — render to stdout
  - `sc-compose resolve [OPTIONS] <agent>` — print resolved file path
  - `sc-compose validate [OPTIONS] <template>` — validate only, exit 2 on error
  - `sc-compose frontmatter-init <file>` — write default frontmatter stub
- Global flags: `--runtime`, `--root`, `--var key=val`, `--env-prefix`, `--json`
- Exit codes: 0 success, 2 validation/render failure, 3 usage error
- `--json` flag: emit structured diagnostics matching `Diagnostic` JSON schema
- Smoke tests: round-trip render, missing var exits 2, `--json` parses

---

### AG.5 — `atm spawn` Integration

**Goal:** Wire sc-composer into `atm spawn` so the `--system-prompt` flag
resolves and renders a template rather than accepting a raw path.

**Deliverables in `crates/atm/`:**

- `SpawnSpec.system_prompt` changes from `Option<PathBuf>` (raw path) to a
  compose request — resolve + render at spawn time if a `.j2` template is given
- Plain `.md` files: passthrough (no rendering, backwards compat)
- `.md.j2` files: render via sc-composer with spawn vars injected
  (`team`, `agent`, `runtime`, `model`, `cwd`, `project` from worktree name)
- `crates/atm/Cargo.toml`: add `sc-composer` workspace dependency
- Tests: spawn with template → rendered file passed to runtime adapter

---

## Sprint Dependency Graph

```
AG.1 (library MVP — file mode compose)
  ├── AG.2 (scmux integration — uses library directly)
  └── AG.3 (resolver + includes)
        └── AG.4 (sc-compose binary — wraps complete library)
              └── AG.5 (atm spawn integration)
```

AG.2 and AG.3 can run in parallel after AG.1 completes.

---

## Version and Release

- Version: `0.42.0` (ATM workspace bump on integrate/phase-AG)
- scmux uses path dependency during development, switches to `=0.42.0` after publish
- `sc-compose` binary added to Homebrew formula

---

## Success Criteria

1. `sc_composer::compose()` renders a `.md.j2` file with variable injection — `NotImplemented` gone.
2. scmux daemon renders per-agent prompts at team launch and injects `--append-system-prompt`.
3. `sc-compose render --var role=team-lead template.md.j2` works from CLI.
4. `atm spawn` renders `.md.j2` templates before passing to runtime adapter.
5. All CI checks green on all three platforms.
6. `sc-composer` and `sc-compose` publish to crates.io as v0.42.0.
