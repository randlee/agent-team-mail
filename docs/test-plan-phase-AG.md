# Phase AG Test Plan: sc-composer + sc-compose

Last updated: 2026-03-08

## Goal

Define required automated coverage for Phase AG deliverables so implementation
review can verify requirement-to-test traceability before merge.

## Coverage Matrix

| Sprint | Scope | Requirement Source | Required Automated Coverage |
|---|---|---|---|
| AG.0 | Stale daemon process hygiene (pre-AG gate) | `docs/requirements.md` §4.7 (single-instance + auto-start), issue #539 planning contract | Integration tests for stale lock-holder PID recovery, daemon identity mismatch detection on CLI connect, and bounded stale-daemon cleanup in test harnesses using isolated `ATM_HOME` scopes |
| AG.1 | Library MVP (`compose`/`validate`, frontmatter/context/render) | `docs/sc-composer/requirements.md` FR-1, FR-2, FR-8 | Unit tests for frontmatter parse (present/absent), variable precedence (`input > env > defaults`), required-var failures, unknown-var policy (`error|warn|ignore`), strict undefined render behavior, and deterministic output snapshots |
| AG.2 | Resolver + include expansion | FR-3, FR-4, FR-5, FR-6 | Unit/integration tests for runtime precedence resolution (`claude/codex/gemini/opencode`), profile-mode search-trace output on resolution failure, explicit-path bypass, include recursion/cycle detection, max-depth guard, root-escape rejection, rendered include output parity in stdout + file-output modes, and deterministic three-block pipeline composition order |
| AG.3 | `sc-compose` CLI (`render/resolve/validate/frontmatter-init/init`) | FR-7, FR-8, FR-9 | CLI integration tests for exit codes (`0/2/3`), `--json` diagnostics schema stability, `frontmatter-init` write + `--dry-run`, `init` idempotency (`.prompts` and `.gitignore`), help output including fix command guidance, and observability event emission for command lifecycle + resolver/validate outcomes |
| AG.4 | ATM integration (`atm teams spawn` + composer API wiring) | `docs/requirements.md` §4.3.2 + sc-composer FR-6/FR-7 integration contract | Integration tests verifying ATM calls library APIs directly (no subprocess composition path), `.md.j2` prompt rendering into spawn flow, plain `.md` passthrough, generated launch command emitted on success/failure, and prompt var forwarding (`team/agent/runtime/model/cwd`) |

## Cross-Platform and Reliability Requirements

- Run AG test matrix on ubuntu, macos, and windows.
- Use deterministic temp paths (`tempfile::TempDir`/`std::env::temp_dir`) only.
- No shell-dependent test harnesses for product/runtime behavior.
- Any flaky test must be redesigned before phase closeout (no retry-only masking).

## Validation Commands (Minimum)

```bash
cargo test -p sc-composer
cargo test -p sc-compose
cargo test -p agent-team-mail --test integration_spawn
cargo clippy --workspace --all-targets -- -D warnings
```
