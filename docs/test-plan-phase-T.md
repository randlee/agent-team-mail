# Phase T Test Plan (Draft)

Last updated: 2026-02-28
Status: Draft for ATM-QA review

## Scope

This document defines sprint-level acceptance criteria and test coverage for the
Phase T candidate execution order.

## Proposed Execution Sequence

1. T.1: #181 daemon auto-start and single-instance reliability
2. T.2: #182 roster seeding/config watcher + #183 agent state transitions
3. Parallel tranche:
   - T.5a: #284 CLI crate publishability
   - T.5b: atm-monitor implementation
   - T.5c: #46/#47 availability signaling clarification
4. T.15: #282 Gemini end-to-end spawn wiring
5. T.14: #281 Gemini resume correctness
6. T.7: permanent publishing process hardening + strengthened `publisher` role

`T.6` (test coverage closure for `U.1`-`U.4`) is independent of the
`T.1`-`T.5*` sequence and may be scheduled at any point once acceptance
criteria are fully scoped.
`T.7` should run after publishability fixes (`T.5a`) and before final release
publication, then remain active as the default publishing gate for future
sprints/releases through the existing `publisher` team-member role
(`.claude/agents/publisher.md`).

---

## T.1 — Daemon Auto-Start + Single-Instance (#181)

### Requirements Coverage

- `requirements.md` section 4.7 (daemon auto-start and single-instance guarantees)

### Acceptance Criteria

- Daemon-backed commands succeed without manual daemon startup.
- Exactly one daemon instance is authoritative per user scope.
- Concurrent CLI invocations do not create duplicate daemon instances.
- Startup failure returns actionable diagnostics.

### Test Matrix

- Unit:
  - readiness probe helpers
  - stale socket/pid detection helpers
  - lock ownership guards
- Integration:
  - command invokes auto-start when daemon absent
  - command no-ops when daemon already healthy
  - concurrent command startup race (single daemon survives)
- Failure-path:
  - lock contention (second daemon rejected)
  - unreadable/invalid state files
  - startup timeout path
- Cross-platform:
  - Windows CI validates spawn/readiness/lock behavior
  - Unix signal/file-socket paths validated
- Multi-team scale:
  - with multiple active teams (representative scale), verify only one daemon
    instance runs and startup/readiness behavior remains deterministic.

### Observability Checks

- Unified log contains daemon start attempt/outcome events.
- `atm doctor` reports daemon availability healthy after successful auto-start.

### Completion Gates

- Required tests pass in CI.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- No regression in existing daemon-backed command behavior.

---

## T.2 — Roster Seeding + State Transitions (#182, #183)

### Requirements Coverage

- `requirements.md` section 4.7: `Roster Seeding and Config Watcher Requirements`
- `requirements.md` section 4.7: `Agent State Transition Requirements`
- `requirements.md` section 4.3.1 cleanup invariants (drift safety)

### Acceptance Criteria

- Daemon startup seeds roster from `config.json`.
- `config.json` edits reconcile roster adds/removes/updates within one watch cycle.
- Agent states transition deterministically (`unknown/active/idle/offline`) with source/reason.
- Drift is surfaced via diagnostics.

### Test Matrix

- Unit:
  - roster reconciliation logic (add/remove/update)
  - state transition reducer/ordering rules
  - liveness reconciliation logic
- Integration:
  - startup with pre-populated config -> roster matches
  - file watcher update propagates to daemon roster
  - hook events + PID changes update visible state
- Failure-path:
  - malformed config update handling
  - conflicting/out-of-order lifecycle events
  - missing session_end with PID death fallback
- Cross-platform:
  - watcher + state behavior stable on Windows/macOS/Linux
- Multi-team isolation:
  - with multiple active teams (representative scale), updates in one team do not
    mutate roster/state/diagnostics for unrelated teams.
  - `atm doctor` default run reports findings only for the requested/default team.
  - `atm broadcast` targets only the resolved team scope.
  - explicit cross-team addressing (`<agent>@<team>`) continues to deliver to
    the selected team and does not bleed into other teams.
  - namespace-qualified cross-computer addresses (when transport is configured)
    remain routable and preserve resolved team isolation semantics.

### Observability Checks

- Unified logs include roster reconcile events and state transition events.
- `atm doctor` detects injected drift and reports actionable findings.
- `atm status` reflects reconciled state within one poll window.

### Completion Gates

- Required tests pass in CI.
- `clippy -D warnings` passes.
- No regressions to cleanup safety invariants.

---

## T.5a — CLI Crate Publishability (#284)

### Requirements Coverage

- `requirements.md` section 4.8.6 (CLI crate publishability requirements)

### Acceptance Criteria

- CLI crate packages/publishes without external-path include failures.
- Release workflow fails hard on publish failure.
- Installability check confirms expected CLI version.

### Test Matrix

- Unit:
  - n/a (mostly packaging/workflow)
- Integration/CI:
  - `cargo package` and `cargo publish --dry-run` for CLI crate
  - workflow failure simulation for publish error
  - post-release version install validation step
- Failure-path:
  - intentional publish failure is not masked

### Observability Checks

- Release logs clearly indicate publish success/failure.

### Completion Gates

- Packaging and publish dry-run checks pass in CI.
- Workflow no longer masks publish failures.

---

## T.7 — Permanent Publishing Process + Strengthened `publisher` Role

### Scope

1. Establish permanent publishing responsibilities in the existing `publisher`
   team-member agent workflow for every future sprint/release publication cycle.
2. Require a formal inventory of crates/artifacts that will be published for
   every release event.
3. Require formal post-publish verification for every required artifact after
   publishing completes.

### Requirements Coverage

- `requirements.md` section 4.8.6 (release and publish validation requirements)

### Acceptance Criteria

- `publisher` executes a standard pre-publish audit on every release cycle,
  mapping release scope to:
  - implemented behavior
  - present/absent tests
  - uncovered requirements
- A machine-readable and human-readable release inventory exists, containing:
  - package/crate/artifact name
  - version
  - source path/release source
  - publish target (registry/channel)
  - verification command(s)
- Post-publish verification runs for every inventory item and records pass/fail
  with evidence links/log pointers.
- Publishing is considered complete only if all required inventory items verify
  successfully or explicit waivers are recorded.
- Workflow is reusable and documented as the default publishing procedure for
  all subsequent sprints/phases (not Phase T only).

### Test Matrix

- Process/integration:
  - `publisher` dry-run with intentional missing test to confirm
    gap detection and reporting
  - inventory generation validation (required fields present; no duplicate
    identifiers; deterministic ordering)
  - post-publish verification runner executes all inventory checks and fails on
    any missing/bad artifact
- Failure-path:
  - missing inventory item causes release gate failure
  - artifact exists but wrong version/signature/checksum causes verification
    failure
  - partial publish success still fails final gate

### Observability Checks

- Audit and verification outputs are persisted in release logs/artifacts.
- Failure reason and remediation target are explicit for each failed item.

### Completion Gates

- `publisher` workflow is updated/documented and approved as default release
  procedure.
- Inventory file/spec is approved and checked into release workflow inputs.
- Post-publish verification passes for all required artifacts.
- Release gate blocks publication completion on unresolved failures.

---

## T.5b — Operational Health Monitor (`atm-monitor`)

### Requirements Coverage

- `requirements.md` section 4.3.3a (operational health monitor)
- `requirements.md` section 4.6 (logging diagnostics surfaces and shared health evaluator contract)

### Acceptance Criteria

- `atm-monitor` runs as a background ATM teammate agent.
- Monitor polls on interval and emits alerts for new critical findings.
- Alert deduplication works within cooldown window.
- Alerts contain severity/code/remediation context.
- Health polling reuses the shared logging-health evaluator module; no health
  state computation logic is duplicated between `atm-monitor`, `atm doctor`,
  and `atm status` handlers.
- Monitor sends ATM mail notifications to designated recipients when issues are detected.

### Test Matrix

- Unit:
  - dedupe window logic
  - finding diff logic
- Integration:
  - background teammate launch succeeds and polling loop remains active
  - injected daemon/session fault produces alert within 2 poll intervals
  - repeated fault within cooldown suppressed
  - fault clear + reintroduce produces new alert
- Failure-path:
  - monitor survives temporary daemon unavailability

### Observability Checks

- Alerts can be correlated to unified log events and doctor finding codes.

### Completion Gates

- Required tests pass and alert behavior is deterministic.

---

## T.5c — Availability Signaling Clarification (#46, #47)

### Requirements Coverage

- `requirements.md` section 4.3.10 (availability signaling contract)

### Acceptance Criteria

- Documented source-of-truth ownership: daemon state is authoritative.
- Hook events and pub/sub roles are explicitly bounded.
- Event payload contract includes idempotency key and required fields.
- Availability payload fields are explicitly named and validated:
  `agent`, `team`, `state`, `timestamp`, `idempotency_key`.

### Test Matrix

- Unit:
  - idempotency/dedup handling for duplicate availability events
- Integration:
  - hook-derived idle event transitions state within one window
  - duplicate replay does not double-transition
  - lost pub/sub message still converges via daemon reconciliation

### Observability Checks

- Availability state changes are visible via status + unified logs.

### Completion Gates

- Design and behavior contract approved before dependent implementation expands.

---

## T.15 — Gemini End-to-End Spawn Wiring (#282)

### Requirements Coverage

- `requirements.md` sections 4.3.4, 4.3.5, 4.3.8

### Acceptance Criteria

- Gemini spawn works end-to-end via runtime adapter path.
- Runtime metadata is persisted and queryable.
- Lifecycle mapping uses unified envelope.

### Test Matrix

- Unit:
  - runtime option mapping and env shaping
  - metadata persistence serialization
- Integration:
  - spawn -> registry metadata present (`runtime`, `runtime_session_id`, `runtime_home`)
  - status/query surfaces include runtime metadata
- Failure-path:
  - spawn failure surfaces actionable errors without corrupting registry

### Completion Gates

- Runtime adapter tests pass across supported platforms.

---

## T.14 — Gemini Resume Correctness (#281)

### Requirements Coverage

- `requirements.md` sections 4.3.4, 4.3.5, 4.3.8 (resume behavior)

### Acceptance Criteria

- Resume binds to correct prior runtime session for same `(team, agent)`.
- Explicit resume override works deterministically.
- Resume does not drift to wrong session/flags.

### Test Matrix

- Unit:
  - resume session resolution precedence
- Integration:
  - spawn fresh -> capture session -> resume same session
  - explicit override path
- Failure-path:
  - missing/stale session id fallback behavior is deterministic and reported

### Completion Gates

- Resume-specific integration tests pass in CI.
- No regressions in fresh spawn behavior.

---

## Unscheduled Backlog Coverage Placeholders

These issues are tracked but not in the first execution slice. Coverage is
defined now so they are not left unspecified.

These placeholders roll up into execution sprint `T.6` (test coverage closure).

### U.1 — TUI Panel Consistency (#184)

- Acceptance: right/left panel state cannot contradict for same agent snapshot.
- Tests: integration harness checks panel parity against shared state source.

### U.2 — TUI Message Viewing (#185)

- Acceptance: list/detail/read-state flows available in TUI.
- Tests: interaction tests for list -> detail -> mark-read behavior.

### U.3 — TUI Header Version (#187)

- Acceptance: header shows current ATM version from build metadata.
- Tests: render test verifies non-empty version token in header output.

### U.4 — `atm status --json` Logging Health Exposure

- Requirement: `requirements.md` section 4.6 logging diagnostics surface requirements.
- Status: Deferred post-Phase T pending logging field implementation.
- Scheduling: rolls up into `T.6`.
- Acceptance (when scheduled): status JSON includes logging health payload from
  shared evaluator contract.

---

## Global Quality Gates

- All changed behavior covered by tests in this plan.
- No implementation without corresponding requirements entry.
- `cargo test` + targeted integration suites pass.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.

## MCP Readiness Gates (Before Live MCP Testing)

- Logging readiness gate:
  - target-state: `atm doctor --json` includes `logging.health_state = "healthy"`
  - pre-implementation fallback: no degraded/unavailable logging findings are present
    in doctor output.
- Daemon/session/roster diagnostics show no critical findings.
- Unified logs contain required lifecycle and command-correlation events for
  at least one end-to-end smoke workflow.
- No unresolved logging path mismatch between producer and daemon diagnostics.
- Any remaining warnings are triaged and explicitly accepted before MCP test start.
