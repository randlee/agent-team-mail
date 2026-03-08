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
- If frontmatter is absent, engine must extract referenced template variables
  from the template/include graph and treat all extracted variables as required.
- Missing required variables must fail render.
- Missing referenced template variables must fail render (strict undefined).
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
- `claude`: `.claude/agents/<agent>.md` -> `.agents/<agent>.md`
- `codex`: `.codex/agents/<agent>.md` -> `.agents/<agent>.md` -> `.claude/agents/<agent>.md`
- `gemini`: `.gemini/agents/<agent>.md` -> `.agents/<agent>.md` -> `.claude/agents/<agent>.md`
- `opencode`: `.opencode/agents/<agent>.md` -> `.agents/<agent>.md` -> `.claude/agents/<agent>.md`

Profile-kind path conventions:
- `kind=agent`:
  - runtime-specific `<runtime>/agents/<name>.md` then shared `.agents/<name>.md`
- `kind=command`:
  - runtime-specific `<runtime>/commands/<name>.md` then shared `.commands/<name>.md`
- `kind=skill`:
  - runtime-specific `<runtime>/skills/<name>/SKILL.md` then shared `.skills/<name>/SKILL.md`

For ATM repository compatibility, `.claude/<kind>/...` remains a valid fallback
for all runtimes when runtime-specific/shared paths are absent.

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

CLI must support:
- `--mode <profile|file>` (default: `file`),
- `--kind <agent|command|skill>` (required when `--mode profile`),
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
  derived from template filename:
  - `<name>.xml.j2` -> `<name>.xml`
  - `<name>.md.j2` -> `<name>.md`
  - `<name>.txt.j2` -> `<name>.txt`
  - `<name>.j2` -> `<name>`
- If `--output <path>` is provided, it overrides derived output path.

`frontmatter-init` behavior:
- Generates a minimal frontmatter block with:
  - `required_variables` (auto-discovered from template references),
  - `defaults` (empty map),
  - `metadata` (empty map).
- Must preserve existing body content and prepend frontmatter at file start.
- Must fail unless `--force` is provided when frontmatter already exists.

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

`sc-compose` is a full CLI and must emit structured logs compatible with
ATM logging conventions.

Required behavior:
- Use the same event schema conventions and field naming as ATM unified logging
  (shared observability surface, not a divergent schema).
- Log at minimum:
  - command start/end,
  - template/profile resolution decisions,
  - include expansion decisions and failures,
  - validation failures,
  - render success/failure (with output target metadata, not full content).
- Support human-friendly and JSON log output modes.
- Support log level control (`error|warn|info|debug|trace`).
- Logging must be enabled by default with safe message truncation behavior for
  large rendered content.
- Standalone default log root must be `sc-compose` scoped (for example,
  `~/.config/sc-compose/logs`), not ATM-owned paths.
- When embedded as a library in another product, logger sink/path must be
  host-injected so events flow to host logging paths (for example ATM logger
  path) without duplicating logging implementations.
- Log records must never include secrets from environment/input unless explicitly
  requested by a debug-redaction override.

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
