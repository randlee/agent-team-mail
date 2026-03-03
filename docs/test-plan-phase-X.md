# Phase X Test Plan: Team Join, Spawn Path, and `atm init` One-Command Setup

Last updated: 2026-03-02

## Goal

Define verification coverage for Phase X planning items before implementation:
`/team-join` UX contract, spawn folder normalization, and enhanced `atm init`
one-command setup behavior.

## Requirements References

- `docs/requirements.md` §4.3.2a (`/team-join` + `atm teams join` contract and JSON fields)
- `docs/requirements.md` §4.9 (`atm init` one-command setup + flags + idempotency)

## Issue Mapping

| Sprint | Focus | Issue |
|---|---|---|
| X.1 | `/team-join` contract and flow verification | [#351](https://github.com/randlee/agent-team-mail/issues/351) |
| X.2 | Spawn `--folder` normalization across runtimes | [#361](https://github.com/randlee/agent-team-mail/issues/361) |
| X.3 | `atm init` one-command setup + default-global hooks | [#357](https://github.com/randlee/agent-team-mail/issues/357) |
| X.4 | Doctor duration parser boundary fix | [#287](https://github.com/randlee/agent-team-mail/issues/287) *(deferred)* |
| X.5 | Serialize env-mutating daemon tests (`ATM_HOME`) | [#337](https://github.com/randlee/agent-team-mail/issues/337) *(deferred)* |
| X.6 | `teams add-member` inbox atomicity | [#338](https://github.com/randlee/agent-team-mail/issues/338) *(deferred)* |

## X.1 Verification — `/team-join` Contract (#351)

| Check | Command / Test | Expected |
|---|---|---|
| Help contract surface | `atm teams join --help` | Help text includes `<agent>`, optional `--team`, and output mode guidance |
| Existing-team caller path | Run `/team-join` from a session already on a team | Team-lead-initiated mode selected; omitting `--team` succeeds |
| Self-join path | Run `/team-join` with no current team context and missing `--team` | Command exits non-zero with actionable "`--team` required" message |
| Team mismatch rejection | Provide mismatched `--team` in lead-initiated path | Command exits non-zero and error text includes mismatch guidance |
| JSON output schema | `atm teams join ... --json` | Output includes `team`, `agent`, `folder`, `launch_command`, `mode` (no missing required fields) |
| Human launch contract | `atm teams join ...` | Human output includes copy-pastable launch command and explicit folder |

Pass criteria:
- All rows above are binary pass/fail.
- JSON field assertions conform to `docs/requirements.md` §4.3.2a.

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
| Has `.atm.toml`, no hooks | `atm init my-team` in repo with existing `.atm.toml` | Installs hooks; `.atm.toml` content unchanged |
| Has hooks, no `.atm.toml` | `atm init my-team` in repo where hooks installed but `.atm.toml` removed | Creates `.atm.toml` and team; no duplicate hooks added |
| Fully initialized (idempotent) | Run same command twice on fully-configured repo | No duplicate hooks, no destructive rewrites, no `.atm.toml` overwrite |
| Identity flag | `atm init my-team --identity arch-ctm` | `.atm.toml` identity set to `arch-ctm` |
| Skip team creation | `atm init my-team --skip-team` | Team create step skipped; other init steps still run |
| Local install override | `atm init my-team --local` | Hooks installed project-locally, global untouched |
| Quickstart created | Verify `docs/quickstart.md` exists | File exists and is tracked |
| Quickstart required sections | Review `docs/quickstart.md` headings/content | Includes prerequisites, `atm init` usage, first send/read flow, worktree/global hook rationale, and `docs/team-protocol.md` reference |

Pass criteria: all command forms are idempotent and produce expected setup state.

## Deferred Follow-On Verification Stubs (X.4/X.5/X.6)

These are intentionally deferred from X.1-X.3 and retained here so the issues are
explicitly mapped with rationale and baseline verification intent.

| Sprint | Issue | Baseline Verification Target |
|---|---|---|
| X.4 | [#287](https://github.com/randlee/agent-team-mail/issues/287) | Reject `atm doctor --since 0m` and negative durations with actionable validation error |
| X.5 | [#337](https://github.com/randlee/agent-team-mail/issues/337) | Parallel daemon test run remains stable when env-mutating tests are serialized |
| X.6 | [#338](https://github.com/randlee/agent-team-mail/issues/338) | `atm teams add-member` creates inbox atomically and avoids doctor drift finding |

## Suggested Execution Commands

```bash
atm teams join --help
atm teams join <agent> --team <team> --json
atm init <team> --check
cargo test -p agent-team-mail teams
cargo test -p agent-team-mail init
cargo test -p agent-team-mail-daemon runtime_adapter
```

## Exit Criteria

- Each Phase X sprint has at least one explicit verification path.
- Acceptance checks for #351, #361, and #357 are fully represented in tests/docs.
- Any unresolved runtime parity gaps are tracked as explicit follow-up issues.
