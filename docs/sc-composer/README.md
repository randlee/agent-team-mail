# sc-composer Docs

This folder contains standalone design documentation for the `sc-composer`
product and `sc-compose` CLI.

## Documents

- [requirements.md](requirements.md): product requirements and CLI behavior.
- [architecture.md](architecture.md): component/module design and request flow.

## Current Key Decisions

- Product split:
  - library crate: `sc-composer`
  - CLI command: `sc-compose`
- Two operating modes:
  - `file` mode: explicit template path, no precedence lookup
  - `profile` mode: precedence-based resolution for `agent`, `command`, `skill`
- Runtime support target: `claude`, `codex`, `gemini`, `opencode`
- Template support:
  - plain files (`.md`, `.txt`, `.xml`)
  - Jinja2 (`*.j2`, e.g. `*.xml.j2`)
  - YAML frontmatter (optional)
  - `@<path>` include expansion with cycle/depth protection
- CLI quality-of-life:
  - `validate` subcommand
  - `frontmatter-init` subcommand
  - `--dry-run` for write-capable operations
- Logging:
  - standalone `sc-compose` logs to `sc-compose`-scoped path
  - embedded library logging is host-injected (ATM can route into ATM logs)
