# Phase AF Test Plan: Reliability Regression + Documentation Closure

## Scope

This plan closes Phase AF by mapping each in-scope issue to implementation and
verifiable tests across:
- AF.1 lifecycle correctness (`#448`, `#449`)
- AF.2 spawn authorization + preview UX (`#394`, `#456`)
- AF.3 transient registration controls (`#393`)
- AF.4 cleanup dry-run + tmux sentinel (`#373`, `#45`)

## Sprint and PR Mapping

| Sprint | PR | Branch | Status |
|---|---|---|---|
| AF.1 | [#524](https://github.com/randlee/agent-team-mail/pull/524) | `feature/pAF-s1-lifecycle-correctness` | COMPLETE |
| AF.2 | [#526](https://github.com/randlee/agent-team-mail/pull/526) | `feature/pAF-s2-spawn-auth-preview` | COMPLETE |
| AF.3 | [#527](https://github.com/randlee/agent-team-mail/pull/527) | `feature/pAF-s3-transient-registration` | COMPLETE |
| AF.4 | [#528](https://github.com/randlee/agent-team-mail/pull/528) | `feature/pAF-s4-cleanup-sentinel` | COMPLETE |
| AF.5 | Pending (AF.5 PR not yet created at commit time) | `feature/pAF-s5-reliability-closeout` | COMPLETE |

## Issue Coverage Matrix

| Issue | Sprint | Primary Implementation | Test Coverage |
|---|---|---|---|
| [#448](https://github.com/randlee/agent-team-mail/issues/448) | AF.1 | session-end/session-id lifecycle correctness in daemon socket + registry | daemon socket lifecycle tests + AF.1 QA regression runs |
| [#449](https://github.com/randlee/agent-team-mail/issues/449) | AF.1 | PID liveness/state convergence hardening | daemon/core liveness tests + AF.1 QA runs |
| [#394](https://github.com/randlee/agent-team-mail/issues/394) | AF.2 | leaders-only spawn gate + caller identity resolution | `commands::teams::tests::*authorization*` + `integration_spawn_folder` |
| [#456](https://github.com/randlee/agent-team-mail/issues/456) | AF.2 | launch command preview on spawn failure (text + JSON) | `integration_spawn_folder` unauthorized + JSON preview assertions |
| [#393](https://github.com/randlee/agent-team-mail/issues/393) | AF.3 | transient non-member registration controls | `integration_transient_registration` + daemon hook watcher tests |
| [#373](https://github.com/randlee/agent-team-mail/issues/373) | AF.4 | cleanup dry-run preview reason-code and parity contract | `integration_teams_cleanup_dry_run` (including parity totals vs actual cleanup) |
| [#45](https://github.com/randlee/agent-team-mail/issues/45) | AF.4 | tmux sentinel nudge contract (`[agent-team-msg:<tier>] unread=<count>`) | worker-adapter nudge + config unit tests |

## End-to-End Reliability Matrix (Scenario x Guarantee x Coverage)

| Scenario | Guarantee | Coverage |
|---|---|---|
| Lifecycle session_end/session_start race windows (`#448`) | Session identity transitions are deterministic; stale session entries are rejected and replaced only via valid lifecycle events. | `crates/atm-daemon/src/daemon/socket.rs` lifecycle tests + AF.1 CI/QA regression run |
| PID liveness convergence (`#449`) | Member status converges on daemon SSoT liveness with no stale "alive" drift across refresh intervals. | daemon/core liveness unit tests + AF.1 QA pass |
| Spawn authorization failure path (`#394`) | Unauthorized callers are denied before mutation/launch and always receive copy-pastable launch preview output. | `commands::teams::tests::*authorization*` + `integration_spawn_folder` unauthorized assertions |
| Spawn preview/error UX (`#456`) | Launch command preview is printed on success and failure in text + JSON output modes. | `integration_spawn_folder` preview assertions (stdout + JSON payload) |
| Transient non-member hook events (`#393`) | Non-member `session_start` events are rejected without silent roster/session mutation. | `test_hook_event_session_start_rejects_non_member` + `integration_transient_registration` |
| Cleanup preview parity (`#373`) | `--dry-run` output is non-mutating and reason/totals parity matches executable cleanup behavior. | `integration_teams_cleanup_dry_run` + `commands::teams::tests::*cleanup*` |
| tmux sentinel nudges (`#45`) | Sentinel contract remains deterministic across info/urgent/blocked paths with config-aware rendering. | daemon `nudge::tests::*` + `config::tests::test_nudge_config_*` |

## AF.5 QA Finding Closure

### ATM-QA-AF3-001
- Requirement: document AF.3 transient-registration contract as an ADR.
- Resolution: added `docs/adr/af3-transient-registration-contract.md`.

### ATM-QA-AF3-002
- Requirement: explicit daemon unit test for non-member `session_start` rejection.
- Resolution: added `test_hook_event_session_start_rejects_non_member` in
  `crates/atm-daemon/src/daemon/socket.rs`.

## Deferred Item Record

| Item | Owner | Rationale | Target |
|---|---|---|---|
| AF.5 PR number insertion in planning tables | team-lead | PR number is assigned when PR is opened after push | AF merge closeout update |

## Validation Commands (AF.5 closeout run)

1. `cargo test -p agent-team-mail-daemon nudge::tests::`
2. `cargo test -p agent-team-mail-daemon config::tests::test_nudge_config_`
3. `cargo test -p agent-team-mail --bin atm commands::teams::tests::`
4. `cargo test -p agent-team-mail --test integration_teams_cleanup_dry_run`
5. `cargo test -p agent-team-mail-daemon test_hook_event_session_start_rejects_non_member`
