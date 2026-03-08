# Phase AE Planning: GH Monitor Reliability + Daemon Logging Isolation

## Goal

Complete the GH monitor operational contract and close daemon observability/runtime
gaps discovered during dogfooding.

## Scope (Issue Mapping)

| Item | Issue | Planned Sprint |
|---|---|---|
| Plugin config discovery parity (daemon + CLI) | [#499](https://github.com/randlee/agent-team-mail/issues/499) | AE.1 |
| `atm gh init` guided setup | [#500](https://github.com/randlee/agent-team-mail/issues/500) | AE.1 |
| Monitor restart does not reload updated config | [#502](https://github.com/randlee/agent-team-mail/issues/502) | AE.3 |
| Status reads stale cache instead of live daemon query | [#503](https://github.com/randlee/agent-team-mail/issues/503) | AE.2 |
| `atm gh monitor status --json` missing | [#504](https://github.com/randlee/agent-team-mail/issues/504) | AE.2 |
| Status/reachability inconsistency + duplicate status rendering | [#505](https://github.com/randlee/agent-team-mail/issues/505) | AE.2 |
| Self-send ambiguity (same identity, concurrent sessions) | [#506](https://github.com/randlee/agent-team-mail/issues/506) | AE.5 |
| `DaemonWriter` producer channel not set (events dropped) | [#472](https://github.com/randlee/agent-team-mail/issues/472) | AE.4 |
| Autostart hides startup failure context | [#473](https://github.com/randlee/agent-team-mail/issues/473) | AE.4 |
| Plugin init failure aborts daemon startup | [#474](https://github.com/randlee/agent-team-mail/issues/474) | AE.4 |

## Requirements References

1. `docs/requirements.md` §4.7 (daemon startup/single-instance behavior).
2. `docs/requirements.md` §5.8 (plugin namespace + command availability).
3. `docs/plugins/ci-monitor/requirements.md` GH-CI-FR-19..24 and GH-CI-TR-7.

## Dependency Graph

1. AE.1 is foundational (config/init contract).
2. AE.2 depends on AE.1.
3. AE.3 depends on AE.1 and AE.2.
4. AE.4 can run in parallel with AE.2/AE.3.
5. AE.5 runs after AE.2 and AE.4.

## Sprint Summary

| Sprint | Name | PR | Branch | Issues | Status |
|---|---|---|---|---|---|
| AE.1 | Config Discovery + `atm gh init` Baseline | [#518](https://github.com/randlee/agent-team-mail/pull/518) | `feature/pAE-s1-config-init` | #499, #500 | COMPLETE |
| AE.2 | Live Status + JSON + Output Consistency | [#519](https://github.com/randlee/agent-team-mail/pull/519) | `feature/pAE-s2-live-status-json` | #503, #504, #505 | COMPLETE |
| AE.3 | Monitor Reload Semantics | [#521](https://github.com/randlee/agent-team-mail/pull/521) | `feature/pAE-s3-reload-semantics` | #502 | COMPLETE |
| AE.4 | Daemon Logging/Autostart/Plugin Isolation | [#522](https://github.com/randlee/agent-team-mail/pull/522) | `feature/pAE-s4-daemon-observability` | #472, #473, #474 | COMPLETE |
| AE.5 | Identity Ambiguity + Phase Closeout | [#523](https://github.com/randlee/agent-team-mail/pull/523) | `feature/pAE-s5-identity-closeout` | #506 | COMPLETE |

## AE.1 — Config Discovery + `atm gh init` Baseline

### Objective

Make GH monitor setup deterministic and discoverable in one command path.

### Deliverables

1. Shared daemon/CLI config-resolution behavior for repo + global config.
2. `atm gh init` command flow with actionable setup output and dry-run support.
3. Tests for config-source parity and init command outcomes.

### Acceptance Criteria

1. Repo-local config visibility is identical for daemon and CLI paths.
2. `atm gh init` succeeds/fails with explicit remediation and no dead-end state.
3. CI tests cover positive + negative setup flows.

## AE.2 — Live Status + JSON + Output Consistency

### Objective

Ensure status surfaces reflect live daemon state and are automation-safe.

### Deliverables

1. Remove stale cache-only status path in `atm gh` surfaces.
2. Add `--json` support with stable schema.
3. Unify status rendering so outputs are non-duplicated and consistent.

### Acceptance Criteria

1. Status and monitor commands show consistent daemon reachability state.
2. JSON mode exists and is validated by integration tests.
3. Human output has a single canonical status block.

## AE.3 — Monitor Reload Semantics

### Objective

Make restart/reload operations apply updated config without manual daemon kill.

### Deliverables

1. Reload path re-reads monitor config and rebinds runtime state.
2. Deterministic error reporting when reload fails.
3. Regression tests for config edit -> reload -> status update loop.

### Acceptance Criteria

1. Config changes are visible after reload without process restart.
2. Failure states are explicit and actionable.
3. No status drift between reload and monitor status.

## AE.4 — Daemon Logging/Autostart/Plugin Isolation

### Objective

Harden daemon resilience and preserve observability under failure.

### Deliverables

1. Ensure structured producer channel is initialized for daemon event logging.
2. Preserve startup failure context in autostart surfaces.
3. Isolate plugin init failures so daemon remains operational for other features.

### Acceptance Criteria

1. Structured daemon events are emitted consistently.
2. Startup failure diagnostics are visible to operator and logs.
3. A single plugin failure does not abort whole daemon startup.

## AE.5 — Identity Ambiguity + Phase Closeout

### Objective

Resolve concurrent identity ambiguity and complete AE regression coverage.

### Deliverables

1. Define/implement identity conflict behavior for same-name concurrent sessions.
2. Add regression tests for collision scenarios.
3. Phase-level verification checklist update.

### Acceptance Criteria

1. Self-send/identity routing is deterministic in concurrent-session conditions.
2. AE scope issues are closed or explicitly deferred with owner + rationale.
3. Phase AE test plan maps all issue requirements to coverage.
