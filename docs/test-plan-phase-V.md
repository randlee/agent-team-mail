# Phase V Test Plan: Doctor State + Lifecycle Convergence

Last updated: 2026-03-02

## Goal

Define implementation-ready tests for Phase V doctor/lifecycle fixes so each issue has explicit acceptance coverage before coding merges.

## Scope

- Team-scoped doctor reconciliation
- Teardown convergence (lead vs non-lead)
- `isActive` (busy signal) vs liveness separation
- Recommendation quality/actionability
- Doctor output context ordering
- Logging process identity coverage (`pid`/`ppid`)

## Issue Mapping

| Sprint | Focus | Issue(s) |
|---|---|---|
| V.0 | Baseline fixtures + failing-path capture | prerequisite (no dedicated issue) |
| V.1 | Team-scoped reconciliation | [#333](https://github.com/randlee/agent-team-mail/issues/333) |
| V.2 | Lead/non-lead teardown semantics | [#332](https://github.com/randlee/agent-team-mail/issues/332) |
| V.3 | `isActive`/liveness separation | [#330](https://github.com/randlee/agent-team-mail/issues/330) |
| V.4 | Terminal-member cleanup convergence *(depends on V.2)* | [#331](https://github.com/randlee/agent-team-mail/issues/331), [#334](https://github.com/randlee/agent-team-mail/issues/334) |
| V.5 | Recommendation engine hardening | [#336](https://github.com/randlee/agent-team-mail/issues/336) |
| V.6 | Doctor UX snapshot ordering | [#335](https://github.com/randlee/agent-team-mail/issues/335) |
| V.7 | Logging identity contract coverage | track under Phase V umbrella (issue optional) |

**Change-control note (V.2+V.3 execution)**: V.2 and V.3 are being implemented/reviewed as a combined delivery stream in PR [#347](https://github.com/randlee/agent-team-mail/pull/347) because teardown and liveness-semantics updates share the same send/status/doctor touchpoints.

## Coordination Constraint

- `send.rs` overlap exists between Phase W.1 (offline prefix behavior) and V.3 (`isActive` semantics). Merge-order constraint: Phase W sprint W.1 (`feature/pW-s1-offline-fix`, merged to `integrate/phase-W`) must merge to `develop` before `integrate/phase-V` merges to `develop`.

## V.0 Baseline Fixture Tasks

1. Add reproducible test fixtures for currently observed finding classes:
   - `DAEMON_TRACKS_UNKNOWN_AGENT`
   - `PARTIAL_TEARDOWN`
   - `TERMINAL_MEMBER_NOT_CLEANED`
   - `ACTIVE_WITHOUT_SESSION`
2. For each fixture, codify current behavior as:
   - failing test with `#[ignore]` (documenting known drift), or
   - passing guard test where behavior is already corrected.
3. Record fixture locations and invocation commands in sprint PR notes.

## V.4 Acceptance Criteria (Terminal Cleanup Convergence)

- Dead terminal non-lead members absent from team config are pruned only after a full extra reconcile cycle.
- Active sessions are never pruned by reconcile logic even when absent from config.
- Kill-timeout fallback converges to full cleanup (session registry removal + roster/mailbox cleanup).
- `integration_send` test harness disables daemon autostart (`ATM_DAEMON_AUTOSTART=0`) to keep offline/unknown-path assertions deterministic.

## Harness Targets

- `crates/atm/src/commands/doctor.rs` (classification/recommendation unit tests)
- `crates/atm-daemon/tests/` (lifecycle/cleanup integration)
- `crates/atm/tests/` (CLI human output + recommendation wiring)
- `crates/atm-core/tests/` (logging contract: `pid`/`ppid`)

## Execution Commands (Baseline)

```bash
cargo test -p agent-team-mail doctor -- --nocapture
cargo test -p agent-team-mail-daemon -- --nocapture
cargo test -p agent-team-mail-core log -- --nocapture
```

## Exit Criteria

- Every V.* sprint has explicit tests mapped to acceptance criteria.
- Critical doctor findings are reproducible via fixtures and eliminated by covered fixes.
- CI matrix remains green for touched suites.
