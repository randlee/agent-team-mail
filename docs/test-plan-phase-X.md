# Phase X Test Plan: Team Join, Spawn Path, and `atm init` One-Command Setup

Last updated: 2026-03-02

## Goal

Define verification coverage for Phase X planning items before implementation:
`/team-join` UX contract, spawn folder normalization, and enhanced `atm init`
one-command setup behavior.

## Issue Mapping

| Sprint | Focus | Issue |
|---|---|---|
| X.1 | `/team-join` contract and flow verification | [#351](https://github.com/randlee/agent-team-mail/issues/351) |
| X.2 | Spawn `--folder` normalization across runtimes | tracker to create |
| X.3 | `atm init` one-command setup + default-global hooks | [#357](https://github.com/randlee/agent-team-mail/issues/357) |

## X.1 Verification — `/team-join` Contract (#351)

| Check | Command / Test | Expected |
|---|---|---|
| Existing-team caller path | Run `/team-join` from a session already on a team | Team-lead-initiated behavior selected; optional `--team` only validates |
| Self-join path | Run `/team-join` with no current team context | Explicit `--team` required and validated |
| Team mismatch rejection | Provide mismatched `--team` in lead-initiated path | Command fails with actionable mismatch guidance |
| Output contract | Complete join flow | Outputs copy-pastable resume command with team/member context |

Pass criteria: all join paths are deterministic and rejection paths are actionable.

## X.2 Verification — Spawn `--folder` Normalization

| Check | Command / Test | Expected |
|---|---|---|
| Canonical flag behavior | Spawn with `--folder <path>` | Runtime launches in canonicalized target folder |
| Compatibility alias | Spawn with `--cwd <path>` | Behavior matches `--folder` |
| Dual-flag validation | Spawn with both `--folder` and `--cwd` | Fails unless paths match after canonicalization |
| Cross-runtime parity | Claude/Codex/Gemini spawn command generation tests | Equivalent folder semantics across adapters |
| Prompt guidance contract | Codex/Gemini startup prompt tests | ATM usage guidance injected around caller prompt |

Pass criteria: folder semantics and startup guidance are consistent across supported runtimes.

## X.3 Verification — `atm init` One-Command Setup (#357)

| Check | Command / Test | Expected |
|---|---|---|
| Fresh init end-to-end | `atm init my-team` in fresh repo | Creates `.atm.toml`, creates team, installs global hooks |
| Idempotent rerun | Run same command twice | No duplicate hooks, no destructive rewrites |
| Identity flag | `atm init my-team --identity arch-ctm` | `.atm.toml` identity set to `arch-ctm` |
| Skip team creation | `atm init my-team --skip-team` | Team create step skipped; other init steps still run |
| Local install override | `atm init my-team --local` | Hooks installed project-locally, global untouched |
| Quickstart documentation | Review `docs/quickstart.md` | Reflects one-command setup and worktree/global rationale |

Pass criteria: all command forms are idempotent and produce expected setup state.

## Suggested Execution Commands

```bash
cargo test -p agent-team-mail init
cargo test -p agent-team-mail teams
cargo test -p agent-team-mail-daemon runtime_adapter
```

## Exit Criteria

- Each Phase X sprint has at least one explicit verification path.
- Acceptance checks for #351 and #357 are fully represented in tests/docs.
- Any unresolved runtime parity gaps are tracked as explicit follow-up issues.
