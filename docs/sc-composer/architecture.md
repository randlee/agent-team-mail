# sc-composer Architecture

> Status: Draft
> Product: standalone composition engine (`sc-composer` + `sc-compose`)

## 1. Design Goals

- Single implementation for prompt/template composition logic.
- Deterministic outputs and diagnostics.
- Reusable as a library by multiple products.
- Standalone CLI for local workflows and CI validation.

## 2. Top-Level Components

### 2.1 `sc-composer` (library)

Core modules:
- `frontmatter`: YAML frontmatter parse + typed metadata.
- `resolver`: runtime-aware profile file search (`.claude`, `.codex`, `.gemini`, `.opencode`, `.agents/agents` + legacy `.agents`).
- `include`: `@<path>` expansion with cycle/depth guards.
- `context`: variable merge + precedence + source tracking.
- `render`: Jinja2 rendering facade (strict undefined).
- `validate`: required/unknown variable policy checks.
- `diagnostics`: structured error/warning model and JSON schema.
- `observability`: structured logging facade aligned with ATM logging schema.
- `pipeline`: block composition (`agent` + `guidance` + `user`).

Resolver extension policy:
- `kind=agent|command` within each candidate directory, `resolver` should probe:
  1. `<name>.md.j2`
  2. `<name>.md`
  3. `<name>.j2`
- `kind=skill` should probe:
  1. `<name>/SKILL.md.j2`
  2. `<name>/SKILL.md`
  3. `<name>/SKILL.j2`
- Explicit file paths passed to compose/validate bypass probe order and are used
  directly (subject to root safety checks).

Public API shape (conceptual):
- `resolve_profile(request) -> ResolveResult`
- `compose(request) -> ComposeResult`
- `validate(request) -> ValidationReport`

### 2.2 `sc-compose` (binary)

Commands:
- `render`
- `resolve`
- `validate`
- `frontmatter-init`
- `init`

CLI uses library APIs directly and only handles argument parsing, output
formatting, and exit codes.
All command execution paths emit structured events through `observability`.

## 3. Data Model

### 3.1 Input Request

- `runtime`: enum (`claude|codex|gemini|opencode|custom`)
- `mode`: enum (`file|profile`)
- `kind`: optional enum (`agent|command|skill`) for profile mode
- `agent`: optional string
- `root`: path
- `template_path`: optional path
- `vars_input`: map
- `vars_env`: map/prefix filter
- `guidance_block`: optional string
- `user_prompt`: optional string
- `policy`: struct:
  - unknown variable mode (`error|warn|ignore`)
  - max include depth
  - allowed roots

### 3.2 Frontmatter

- `required_variables: Vec<String>`
- `defaults: Map<String, Value>`
- `metadata: Map<String, Value>`

### 3.3 Output

- `rendered_text: String`
- `resolved_files: Vec<PathBuf>` (profile + includes)
- `variable_sources: Map<String, SourceKind>` (`input|env|default`)
- `warnings: Vec<Diagnostic>`
- `diagnostics: Vec<Diagnostic>`

## 4. Request Flow

1. Resolve profile file path (profile mode) or use explicit template path (file mode).
2. Read file and parse frontmatter/body.
3. Expand includes in body (confined root, cycle/depth checks).
4. Merge context with precedence:
   - input > env > defaults
5. Validate:
   - extract referenced template variables (when frontmatter requirements absent),
   - emit undeclared-token warnings (or errors in strict mode),
   - required variables,
   - unknown variable policy.
6. Render template in strict mode.
7. Compose final output blocks in fixed order.
8. Return rendered output + diagnostics + trace metadata.

Output target policy:
- file mode render-to-file defaults to replacing `.j2` suffix beside template.
- profile mode render-to-file defaults to `.prompts/<name>-<ulid>.md` to avoid
  polluting version-controlled template directories.

Dry-run path:
- For write-capable operations, planner computes target paths + diff summary,
  then exits without file writes.

## 5. Safety Model

- Default deny for out-of-root file access.
- No shell execution inside compose pipeline.
- Strict undefined in rendering prevents silent variable drift.
- Include stack is always tracked for precise failure attribution.

## 6. Error and Exit Semantics

Library:
- returns typed error enum with stable codes.

CLI:
- `0`: success (no errors; warnings allowed depending on mode)
- `2`: validation/render failure
- `3`: usage/configuration error

## 7. Extensibility

- Runtime path policy is data-driven (not hardcoded in caller).
- Diagnostics format versioned for machine consumers.
- Future optional modules:
  - schema-based typed variable validation,
  - remote include providers,
  - template cache.

## 8. Packaging Strategy

- Keep `sc-composer` runtime-agnostic and ATM-independent.
- ATM integrates via library API wrappers.
- Other tools can use `sc-compose` directly in CI/dev workflows.
- `atm init` should call the same compose-initialization helper used by
  `sc-compose init` for `.prompts/` and `.gitignore` bootstrap behavior.
