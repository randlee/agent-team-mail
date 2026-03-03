# Phase X Test Plan: Onboarding + TUI/Doctor Stability

Last updated: 2026-03-02

## Goal

Define verification coverage for all Phase X planned issues so implementation can be validated against requirements without ambiguity.

## Scope

- Onboarding atomicity (`atm teams add-member` + mailbox bootstrap)
- Daemon env-sensitive test stability
- TUI foundational blockers and core usability
- Doctor duration validation correctness

## Requirements References

- `docs/requirements.md` section `4.3` (`atm teams add-member`)
- `docs/requirements.md` section `4.3.3` (`atm doctor --since` duration validation)
- `docs/requirements.md` section `4.3.3b` (TUI baseline correctness)
- `docs/requirements.md` section `4.7` (daemon auto-start + roster seeding)
- `docs/requirements.md` section `8.3` (env-mutation test serialization)

## Issue Mapping

| Sprint | Focus | Issue |
|---|---|---|
| X.1 | Atomic inbox creation on member add | [#338](https://github.com/randlee/agent-team-mail/issues/338) |
| X.2 | Serialize env-mutating daemon tests | [#337](https://github.com/randlee/agent-team-mail/issues/337) |
| X.3 | Daemon auto-start reliability for TUI entry | [#181](https://github.com/randlee/agent-team-mail/issues/181) |
| X.4 | Roster seeding from config at startup | [#182](https://github.com/randlee/agent-team-mail/issues/182) |
| X.5 | TUI panel state convergence | [#184](https://github.com/randlee/agent-team-mail/issues/184) |
| X.6 | Doctor `parse_since_input` boundary validation | [#287](https://github.com/randlee/agent-team-mail/issues/287) |
| X.7 | TUI message viewing capability | [#185](https://github.com/randlee/agent-team-mail/issues/185) |
| X.8 | TUI header version visibility | [#187](https://github.com/randlee/agent-team-mail/issues/187) |

## X.1 Verification — `atm teams add-member` Atomic Inbox Creation (#338)

| Check | Command / Test | Expected |
|---|---|---|
| Roster update + inbox creation converge | `atm teams add-member <team> <agent>` then check `config.json` + `inboxes/<agent>.json` | Both exist immediately after success |
| Idempotent re-run | run add-member twice for same agent | No inbox corruption/truncation |
| First-send path | `atm send <agent>@<team> "ping"` immediately after add-member | Send succeeds without mailbox bootstrap warning |
| Doctor integrity | `atm doctor --team <team>` | No roster/mailbox drift for new member |

Pass criteria: all checks pass.

## X.2 Verification — Env-Mutating Daemon Tests Serialized (#337)

| Check | Command / Test | Expected |
|---|---|---|
| Serialization coverage in target files | inspect `crates/atm-daemon/tests/issues_error_tests.rs`, `ci_monitor_error_tests.rs`, `issues_integration.rs` | Env-mutating tests are marked serialized |
| Parallel stability baseline | `cargo test -p agent-team-mail-daemon -- --test-threads=8` | No `ATM_HOME` race flakes |
| CI matrix stability | Linux/macOS/Windows CI for daemon test suites | No intermittent env-race failures |

Pass criteria: no flake reproduction under repeated parallel runs.

## X.3 Verification — Daemon Auto-Start Reliability (#181)

| Check | Command / Test | Expected |
|---|---|---|
| Status auto-start | stop daemon, run `atm status` | Daemon auto-starts and status returns |
| Doctor auto-start | stop daemon, run `atm doctor` | Report produced; no manual daemon start |
| TUI entry auto-start | stop daemon, launch TUI path | Daemon starts and TUI does not land in silent empty state |

Pass criteria: daemon-backed commands/TUI recover automatically from no-daemon state.

## X.4 Verification — Roster Seeding from `config.json` (#182)

| Check | Command / Test | Expected |
|---|---|---|
| Startup seeding | pre-populate team config, start daemon | In-memory roster matches config |
| Config watcher add | add member to `config.json` | New member appears within one watch cycle |
| Config watcher remove | remove member from `config.json` | Roster reconciles remove/update correctly |

Pass criteria: seeded + watched roster state converges to config state.

## X.5 Verification — TUI Panel Convergence (#184)

| Check | Command / Test | Expected |
|---|---|---|
| Unified panel state source | TUI panel consistency tests | Left/right panel state cannot diverge for same agent |
| Stream/status consistency | TUI stream-panel tests with state transitions | Stream emptiness and status indicators remain coherent |

Pass criteria: no contradictory panel output in covered fixtures.

## X.6 Verification — Doctor Duration Parser Boundaries (#287)

| Check | Command / Test | Expected |
|---|---|---|
| Reject `0m` | `atm doctor --since 0m` | Validation error, non-success exit |
| Reject negative duration | `atm doctor --since -5m` | Validation error, non-success exit |
| Accept positive durations | `atm doctor --since 30m` and `atm doctor --since 45s` | Valid execution path |

Pass criteria: invalid durations are rejected; valid durations continue to work.

## X.7 Verification — TUI Message Viewing Capability (#185)

| Check | Command / Test | Expected |
|---|---|---|
| Message list rendering | TUI message list tests with fixture inboxes | Expected message rows appear |
| Message detail rendering | TUI detail view tests | Full content visible |
| Mark-read persistence | TUI mark-read tests with atomic write path | Read status persists in inbox file |

Pass criteria: operators can list, inspect, and mark messages read from TUI.

## X.8 Verification — TUI Header Version Visibility (#187)

| Check | Command / Test | Expected |
|---|---|---|
| Header version render | TUI header render tests | Version string shown from build metadata |
| Manual smoke | launch TUI in dev build | Visible version in header |

Pass criteria: version is consistently visible in header.

## Suggested Execution Commands

```bash
cargo test -p agent-team-mail-daemon -- --test-threads=8
cargo test -p agent-team-mail-tui
cargo test -p agent-team-mail doctor -- --nocapture
```

## Exit Criteria

- Every X.* issue has at least one explicit verification path in this plan.
- Requirements references for each verification area are present and current.
- CI matrix is green for touched suites.
