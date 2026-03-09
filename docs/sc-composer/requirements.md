# sc-composer Requirements

> Status: Draft
> Product: `sc-composer` (standalone library + CLI)

## 1. Problem Statement

Teams need one deterministic composition engine for agent prompts and instruction
templates across runtimes (Claude, Codex, Gemini, OpenCode). Today this logic is
duplicated in multiple places with inconsistent include handling and variable
validation.

`sc-composer` is a standalone product that provides:
- prompt/profile file resolution,
- Jinja2-style template rendering,
- include expansion,
- strict variable validation,
- consistent machine-readable diagnostics.

## 2. Product Scope

`sc-composer` has two deliverables:
- Library crate: `sc-composer`
- CLI command: `sc-compose`

The library is runtime-agnostic and reusable by ATM and non-ATM tooling.

Current packaging/release mode (required for this phase):
- `sc-composer` and `sc-compose` are part of the ATM workspace and are
  version-locked to the ATM release version.
- They are released together in one ATM publish cycle (no independent versioning
  during this mode).
- ATM distribution install/upgrade paths must install/upgrade both the
  `sc-composer` crate release and `sc-compose` CLI together.

ATM integration contract:
- ATM is a first-class consumer of `sc-composer` and must use the same
  composition semantics exposed by `sc-compose`.
- ATM integration must call `sc-composer` APIs directly; shell/subprocess
  invocation of `sc-compose` is not an acceptable core integration path.

## 3. Functional Requirements

### FR-1: Template Inputs

- Must support plain text/markdown files (`.txt`, `.md`).
- Must support Jinja2 template files (`.j2`, `.md.j2`).
- Must treat any filename ending in `.j2` as a template, including:
  - `.md.j2`
  - `.txt.j2`
  - `.xml.j2`
- Must support YAML frontmatter at file start:
  - `required_variables: [..]`
  - `defaults: { ... }`
  - `metadata: { ... }`
- Frontmatter is optional.

### FR-1a: File Extension and Discovery Conventions

- Profile/agent files must support these extension patterns:
  - plain: `*.md`, `*.txt`, `*.xml`
  - templates: `*.j2` and typed variants (`*.md.j2`, `*.txt.j2`, `*.xml.j2`)
- Resolver lookup for runtime profile directories (§FR-5) should prefer this order
  within each directory candidate:
  1. `<agent>.md.j2`
  2. `<agent>.md`
  3. `<agent>.j2`
- CLI `render`/`validate` must accept explicit template paths anywhere under root,
  including skill templates such as:
  - `.claude/skills/codex-orchestration/dev-template.xml.j2`
  - `.claude/skills/codex-orchestration/qa-template.xml.j2`

### FR-2: Variable Resolution and Precedence

- Final context is built with this precedence:
  1. explicit input variables (CLI flags/API map),
  2. environment variables,
  3. frontmatter defaults.
- Required variables from frontmatter must resolve after merge.
- If frontmatter is absent:
  - engine must extract referenced template variables from the template/include graph,
  - `validate` must emit a generated-frontmatter recommendation,
  - diagnostics must include a fix command:
    `sc-compose frontmatter-init <file>.j2`.
- Tokens referenced in template/include graph but not declared in frontmatter must:
  - be preserved in rendered output by default,
  - emit warnings in `validate` and `render` diagnostics.
- Missing frontmatter-declared required variables must fail render.
- Strict mode (`--strict`) must fail render/validate on undeclared referenced tokens.
- Undefined-variable render failures (template engine strict undefined) and
  undeclared-token validation warnings/errors are distinct diagnostics and must
  use distinct stable diagnostic codes.
- Missing-variable errors must include:
  - full list of missing variable names,
  - the template/include file where each variable was referenced,
  - line/column when available,
  - include chain to the failing reference.
- Unknown extra input variables must be policy-controlled:
  - `error`,
  - `warn`,
  - `ignore`.

### FR-3: Include Expansion

- Must support include directives in content: `@<path>`.
- Include resolution order:
  1. path relative to the containing file,
  2. path relative to repo root (when configured).
- Nested includes are allowed with:
  - cycle detection,
  - bounded max depth,
  - deterministic expansion order.
- Included files ending in `.j2` must be rendered using the same context/policy
  pipeline as the parent template.
- Include expansion must run in both file-output and stdout/stream render modes.
- Include failures must return actionable diagnostics with include chain.

### FR-4: Safety Constraints

- File reads must be confined to a configured root by default.
- Path traversal that escapes root (`..`, absolute paths outside root) must fail.
- Optionally allow explicit allowlist roots.
- Rendering must not execute arbitrary host code from templates.

### FR-5: Agent Prompt Resolution Conventions

The resolver module must support runtime-specific profile search paths for:
- `agent`
- `command`
- `skill`

Resolution precedence applies only in `profile` mode.

`file` mode:
- Caller passes an explicit template/prompt path.
- No precedence search is applied.

In `profile` mode, resolver must support runtime-specific search paths.
Default order:
- `claude`: `.claude/agents/<agent>.md` -> `.agents/agents/<agent>.md` -> `.agents/<agent>.md`
- `codex`: `.codex/agents/<agent>.md` -> `.agents/agents/<agent>.md` -> `.claude/agents/<agent>.md` -> `.agents/<agent>.md`
- `gemini`: `.gemini/agents/<agent>.md` -> `.agents/agents/<agent>.md` -> `.claude/agents/<agent>.md` -> `.agents/<agent>.md`
- `opencode`: `.opencode/agents/<agent>.md` -> `.agents/agents/<agent>.md` -> `.claude/agents/<agent>.md` -> `.agents/<agent>.md`

Ambiguity contract:
- When `--ai` (runtime selector) is provided, only that runtime precedence chain
  is used.
- When `--ai` is omitted, resolver must scan all runtime/shared roots; if more
  than one candidate matches, command must fail with actionable ambiguity error
  requiring `--ai`.
- If exactly one candidate is found across all roots, it may be selected without
  `--ai`.

Profile-kind path conventions:
- `kind=agent`:
  - runtime-specific `<runtime>/agents/<name>.md` then shared `.agents/agents/<name>.md`
- `kind=command`:
  - runtime-specific `<runtime>/commands/<name>.md` then shared `.agents/commands/<name>.md`
- `kind=skill`:
  - runtime-specific `<runtime>/skills/<name>/` probe order:
    1. `SKILL.md.j2`
    2. `SKILL.md`
    3. `SKILL.j2`
  - then shared `.agents/skills/<name>/` with the same probe order.

For ATM repository compatibility, `.claude/<kind>/...` remains a valid fallback
for all runtimes when runtime-specific/shared paths are absent.
For backwards compatibility with older shared layouts, resolver may also check
legacy `.agents/<name>.md` for `kind=agent` after `.agents/agents/<name>.md`.

The path policy must be configurable by callers.

### FR-6: Composition Pipeline

For composed outputs, engine must support deterministic concatenation blocks:
1. resolved agent profile body (rendered),
2. injected guidance block (caller-supplied),
3. user prompt block (caller-supplied).

Each block may be empty; output order is fixed.

### FR-7: CLI Surface

`sc-compose` must provide:
- `render`: render one file to stdout/file.
- `resolve`: print winning profile path and search trace.
- `validate`: validate variables/includes without emitting full output.
- `frontmatter-init`: generate default YAML frontmatter for a template/profile file.
- `init`: repository bootstrap (`.prompts/`, `.gitignore` entry, template scan + recommendations).

CLI must support:
- `--mode <profile|file>` (default: `file`),
- `--kind <agent|command|skill>` (required when `--mode profile`),
- `--agent-type <name>` (profile-mode selector; alias to `--agent`),
- `--ai <claude|codex|gemini|opencode>` (runtime selector; alias to `--runtime`),
- `--var key=value` (repeatable),
- `--var-file <json|yaml>`,
- `--env-prefix <PREFIX_>`,
- `--runtime <claude|codex|gemini|opencode>`,
- `--agent <name>`,
- `--root <path>`,
- `--json` diagnostics mode,
- `--dry-run` for write-capable commands.

`validate` behavior:
- Must run full include expansion + variable extraction/merge checks.
- Must not write output files.
- Must return non-zero exit code on validation errors.
- In `profile` mode, must validate search-path resolution trace and report all
  attempted locations before failing.

`--dry-run` behavior:
- For `render` with file output, `--dry-run` must report:
  - resolved input template path,
  - resolved output path,
  - whether output would change (when output exists),
  - validation/render diagnostics.
- For `frontmatter-init`, `--dry-run` must print the frontmatter that would be
  written and target path, without modifying files.

Render output path rules:
- If `render` writes to stdout (default), no output filename transform is applied.
- If `render` writes to a file without explicit `--output`, default output path is
  mode-dependent:
  - file mode: derived from template filename:
    - `<name>.xml.j2` -> `<name>.xml`
    - `<name>.md.j2` -> `<name>.md`
    - `<name>.txt.j2` -> `<name>.txt`
    - `<name>.j2` -> `<name>`
  - profile mode (`kind=agent|command|skill`): `.prompts/<prompt-name>-<ulid>.md`
    to avoid writing generated prompts beside version-controlled templates.
- If `--output <path>` is provided, it overrides derived output path.

`frontmatter-init` behavior:
- Generates a minimal frontmatter block with:
  - `required_variables` (auto-discovered from template references),
  - `defaults` (empty map),
  - `metadata` (empty map).
- Must preserve existing body content and prepend frontmatter at file start.
- Must fail unless `--force` is provided when frontmatter already exists.

`init` behavior:
- Create `.prompts/` at repo root.
- Ensure `.prompts/` is present in `.gitignore` (idempotent).
- Scan repository for `*.j2` templates and run validation.
- Print recommendations for missing/weak frontmatter with direct fix commands.
- CLI help for `validate` and `frontmatter-init` must include frontmatter edit
  guidance and the direct fix command form:
  `sc-compose frontmatter-init <file>.j2`.

ATM init integration:
- `atm init` must run compose-init-equivalent setup automatically (or call the
  same underlying library helper) so `.prompts/` + `.gitignore` policy is
  enforced without extra user steps.

### FR-8: Determinism and Diagnostics

- Same inputs must produce byte-identical outputs.
- Diagnostics must include:
  - error code,
  - message,
  - file path,
  - line/column when available,
  - include stack when applicable.
- JSON diagnostics schema must be stable and versioned.

### FR-9: Unified Logging (ATM-Compatible)

Detailed observability requirements are defined in:
- `docs/observability/requirements.md`
- `docs/observability/architecture.md`

`sc-compose`/`sc-composer` requirements in this document are integration-specific:
- Must use `sc-observability` as the logging implementation (no duplicate local logger).
- Must emit command lifecycle and composition diagnostics events required by
  observability requirements.
- Standalone defaults must keep `sc-compose` sink paths tool-scoped.
- Embedded usage must permit host-injected sink/path configuration.
- OTel support remains optional and feature-gated, aligned with observability
  baseline trace/metric naming.

## 4. Non-Functional Requirements

- Cross-platform support: macOS, Linux, Windows.
- No shell-specific dependencies.
- Render path should be fast enough for interactive CLI usage.
- Library API must be semver-stable once released.

## 5. Testing Requirements

- Unit coverage:
  - frontmatter parse,
  - precedence merge,
  - required/unknown variable policies,
  - strict undefined behavior,
  - include resolution and cycle/depth handling,
  - path confinement checks.
- Integration coverage:
  - CLI `render/resolve/validate/frontmatter-init`,
  - `--dry-run` no-write guarantees for write-capable commands,
  - JSON diagnostics contract,
  - cross-platform path behavior.

## 6. Out of Scope (Initial Release)

- Remote includes (http/https).
- Arbitrary runtime plugin execution from templates.
- Runtime-specific hook/event integration.
