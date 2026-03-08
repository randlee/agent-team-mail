# Phase AF Planning: Team Management Reliability + Lifecycle Hardening

## Goal

Close remaining team-member lifecycle, spawn authorization, and cleanup reliability
gaps that impact operational correctness for multi-agent ATM usage.

## Scope (Issue Mapping)

| Item | Issue | Planned Sprint |
|---|---|---|
| Spawn auth failure should still print launch command preview | [#456](https://github.com/randlee/agent-team-mail/issues/456) | AF.2 |
| Task-tool transient agents auto-register into roster | [#393](https://github.com/randlee/agent-team-mail/issues/393) | AF.3 |
| Gate terminal spawn to team-lead/co-leaders | [#394](https://github.com/randlee/agent-team-mail/issues/394) | AF.2 |
| PID liveness cache TTL + periodic re-probe | [#449](https://github.com/randlee/agent-team-mail/issues/449) | AF.1 |
| `session_end` session-id scoping + stale dead/alive drift | [#448](https://github.com/randlee/agent-team-mail/issues/448) | AF.1 |
| `atm teams cleanup --dry-run` | [#373](https://github.com/randlee/agent-team-mail/issues/373) | AF.4 |
| tmux sentinel injection | [#45](https://github.com/randlee/agent-team-mail/issues/45) | AF.4 |

## Requirements References

1. `docs/requirements.md` §4.5 (session lifecycle + hook semantics).
2. `docs/requirements.md` §4.7 (daemon liveness and startup behavior).
3. `docs/requirements.md` §4.9.5 (`atm init` + runtime install/reliability context).

## Dependency Graph

1. AF.1 is foundational for lifecycle correctness.
2. AF.2 depends on AF.1 (auth and spawn state assumptions).
3. AF.3 depends on AF.1 and AF.2 (registration behavior on top of gated spawn).
4. AF.4 can run in parallel with AF.3 after AF.1 lands.
5. AF.5 final verification depends on AF.2-AF.4.

## Sprint Summary

| Sprint | Name | PR | Branch | Issues | Status |
|---|---|---|---|---|---|
| AF.1 | Lifecycle Correctness (Session + PID Liveness) | — | `feature/pAF-s1-lifecycle-correctness` | #448, #449 | PLANNED |
| AF.2 | Spawn Authorization + Preview UX | — | `feature/pAF-s2-spawn-auth-preview` | #394, #456 | PLANNED |
| AF.3 | Transient Agent Registration Controls | — | `feature/pAF-s3-transient-registration` | #393 | PLANNED |
| AF.4 | Cleanup Preview + tmux Sentinel | — | `feature/pAF-s4-cleanup-sentinel` | #373, #45 | PLANNED |
| AF.5 | Reliability Regression + Documentation Closure | — | `feature/pAF-s5-reliability-closeout` | #448, #449, #393, #394, #456, #373, #45 | PLANNED |

## AF.1 — Lifecycle Correctness (Session + PID Liveness)

### Objective

Make session lifecycle and PID liveness state deterministic and stale-resistant.

### Deliverables

1. Enforce strict `session_end` scoping by session-id.
2. Introduce PID liveness cache freshness policy and periodic re-probe strategy.
3. Add daemon/unit/integration tests for stale-state recovery paths.

### Acceptance Criteria

1. Dead/alive mismatch state converges correctly without manual cleanup loops.
2. PID liveness stale windows are bounded and test-validated.
3. Session replacement/resume behavior does not corrupt member state.

## AF.2 — Spawn Authorization + Preview UX

### Objective

Lock spawn authorization rules while preserving operator guidance.

### Deliverables

1. Enforce team-lead/co-leader-only spawn gating by team config.
2. Ensure auth-failure paths still print full copy/paste launch command preview.
3. Add authorization matrix tests (lead, co-lead, member, unknown).

### Acceptance Criteria

1. Unauthorized callers are blocked deterministically.
2. Failure output remains actionable with launch preview preserved.
3. Regression tests prevent future auth/UX drift.

## AF.3 — Transient Agent Registration Controls

### Objective

Prevent transient task-tool sessions from polluting persistent team roster state.

### Deliverables

1. Define transient vs persistent registration contract.
2. Prevent auto-registration side effects for transient task-tool agents.
3. Add tests for spawn/read/send paths to ensure roster invariants hold.

### Acceptance Criteria

1. Transient task agents do not persist unexpectedly in roster/config.
2. Persistent registration paths remain unchanged and deterministic.
3. Doctor/status/members reflect expected state under transient activity.

## AF.4 — Cleanup Preview + tmux Sentinel

### Objective

Improve cleanup safety and tmux process observability.

### Deliverables

1. Add non-mutating `atm teams cleanup --dry-run` preview output with reason codes.
2. Implement tmux sentinel injection contract for lifecycle tracing.
3. Add tests for dry-run output parity with actual cleanup behavior.

### Acceptance Criteria

1. Cleanup preview is comprehensive and operator-safe.
2. Sentinel behavior is deterministic and documented.
3. Cleanup + sentinel features have CI-backed coverage.

## AF.5 — Reliability Regression + Documentation Closure

### Objective

Finalize AF with a full regression pass and documentation alignment.

### Deliverables

1. End-to-end reliability matrix across lifecycle, spawn, registration, and cleanup.
2. Requirements/project-plan/test-plan synchronization updates.
3. Deferred-item list with owner + rationale for any non-closed scope items.

### Acceptance Criteria

1. AF issue scope is fully mapped to tests and implementation outcomes.
2. Docs and behavior are consistent and reviewable by atm-qa.
3. No unresolved blocking reliability gaps remain for next release tranche.
