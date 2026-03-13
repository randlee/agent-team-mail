# Refactor Sprint Plan

**Status**: Draft
**Scope**: Phase AM refactoring workflow and ordered sprint sequence

## Purpose

This document defines:

1. A repeatable process for every refactor sprint.
2. An ordered list of refactor sprints for decomposing the current daemon and plugin architecture without changing behavior accidentally.

The goal is to reduce complexity by clarifying authority boundaries, not by merely moving code into smaller files.

## Refactor Sprint Process

Every refactor sprint should follow the same loop.

### 1. Define the Boundary

Before editing code, write down:

- the exact files/modules in scope
- the exact files/modules intentionally out of scope
- whether the sprint is mechanical extraction only, or extraction plus interface cleanup
- the state or I/O boundaries being introduced

If the boundary is unclear, the sprint is not ready.

### 2. State the Invariants

Before code moves, document the invariants that must remain true.

Examples:

- which store is authoritative for online/offline state
- which signals may mutate `isActive`
- which component owns session registry updates
- which behavior must not change at the CLI or daemon socket surface

Each sprint should also list the specific tests that prove those invariants.

### 3. Extract Mechanically First

Move code with the smallest possible semantic change first.

Preferred order:

- move types and pure helpers
- move shared test support
- move logic behind existing call sites
- introduce traits only at true authority or I/O boundaries

Avoid redesigning behavior mid-extraction unless the sprint explicitly includes that behavior change.

### 4. Normalize the Interface

After the mechanical move is stable, clean up the new boundary:

- trim oversized parameter lists
- move cross-module state mutation behind service methods
- replace ad hoc helper calls with explicit interfaces where useful
- keep transport/adapters thin and push policy into core services

Traits are appropriate when a boundary owns state, external I/O, or is heavily mocked in tests.

### 5. Run Focused Verification

Each sprint must run:

- targeted unit/integration tests for the moved domain
- one broader regression pass covering the public surface touched by the sprint
- formatting and linting for the touched crates

Do not rely on full CI alone to prove a refactor is safe.

### 6. Run a Short Maintenance Review

Before handing off to QA, do a short cleanup/review pass:

- hidden invariant check
- missing regression test check
- flaky/shared-test-fixture check
- constant/config/magic-value cleanup only if directly relevant

This is where the Rust quality prompts should be applied selectively, not mechanically.

### 7. QA and Findings Capture

After the sprint is pushed:

- request QA immediately
- record findings before starting the next sprint
- if the sprint exposed a bad seam, add it to the next sprint instead of patching opportunistically in unrelated areas

### 8. Completion Contract

A sprint is complete only when all of these are true:

- boundary is clearer than before
- authority for the touched state is easier to explain
- tests cover the moved behavior
- QA findings are resolved or explicitly deferred
- the next sprint can build on the new boundary without reopening the same seam

## Cross-Sprint Rules

- Prefer subsystem extraction over file splitting by function name.
- Keep `socket.rs` moving toward transport and dispatch only.
- Keep hook/file-watcher/CLI inputs as adapters that emit normalized domain events.
- Keep one core service responsible for canonical agent/session state.
- Keep provider-specific CI logic separate from provider-agnostic CI policy.
- Do not create new crates until the module boundary is already stable.

## Proposed Sprint Sequence

### Sprint 1: Extract CI Monitoring Subsystem

Goal:

- remove CI monitoring business logic from `socket.rs`
- establish a clear subsystem boundary inside `atm-daemon`

Work:

- extract `ci_monitoring` module tree from daemon socket handling
- keep socket dispatch thin: parse request, call subsystem, return response
- move CI state/health/report shaping logic into subsystem code

Exit criteria:

- CI monitor logic no longer lives materially inside `socket.rs`
- existing CI monitor tests still pass with minimal behavior drift

### Sprint 2: Split CI Core from GitHub Adapter

Goal:

- separate provider-agnostic CI policy from GitHub-specific implementation

Work:

- define CI core model and workflow types
- move `gh` CLI calls and GitHub response translation into a GitHub adapter
- keep CI core responsible for policy, classification, and state transitions

Exit criteria:

- GitHub-specific code is isolated
- Azure support would fit as another adapter instead of a second policy stack

### Sprint 3: Extract Core Agent State Service

Goal:

- centralize agent/session state authority

Work:

- define a service that owns:
  - session registry updates
  - activity state updates
  - online/offline derivation
  - ownership/conflict decisions
- move policy out of socket handlers and scattered helper logic

Exit criteria:

- one service is the obvious home for canonical state mutation
- socket handlers no longer directly encode state policy in multiple places

### Sprint 4: Normalize Lifecycle Domain Events

Goal:

- make hook/file-watcher/CLI/runtime signals feed the same core model

Work:

- define normalized domain events such as:
  - `SessionStarted`
  - `SessionEnded`
  - `AgentActive`
  - `AgentIdle`
  - `ConfigChanged`
  - `InboxTouched`
  - `SessionHintObserved`
- translate existing input sources into those events

Exit criteria:

- the same lifecycle concept is not represented differently in each adapter path

### Sprint 5: Extract Hook Adapter Boundary

Goal:

- treat hooks as an event source, not as direct state mutators

Work:

- isolate hook payload parsing and validation
- map hook payloads into normalized domain events
- keep hook-specific logging and rejection reasons near the adapter boundary

Exit criteria:

- hook logic is no longer tightly interleaved with core state policy
- hook audit/logging behavior has a clear home

### Sprint 6: Extract File Watcher Boundary

Goal:

- isolate filesystem observation from state policy

Work:

- move watcher-specific code into an adapter layer
- emit normalized events into the core state service
- keep watcher recovery/retry details out of domain logic

Exit criteria:

- file watcher code is separately testable
- inbox/config/session observation does not directly encode agent-state policy

### Sprint 7: Normalize CLI/Daemon Hint Paths

Goal:

- fold command-side hints into the same lifecycle model

Work:

- review `register_hint`, `send`, and related daemon hint paths
- route them through the same core service contract as hook/watcher inputs
- remove duplicated ownership and conflict checks

Exit criteria:

- command-side hints are no longer a separate half-authoritative lifecycle path

### Sprint 8: Thin `socket.rs` to Transport + Dispatch

Goal:

- reduce `socket.rs` to request parsing, dispatch, and response shaping

Work:

- remove remaining business logic from `socket.rs`
- group dispatch code by domain entrypoint instead of hidden policy
- extract shared test support if still embedded in socket tests

Exit criteria:

- `socket.rs` is mostly transport glue
- state and plugin policy live elsewhere

### Sprint 9: Stabilize Interfaces and Shared Test Support

Goal:

- make the new boundaries maintainable

Work:

- extract reusable test fixtures/builders/guards
- prune helper sprawl
- tighten trait boundaries where mocks and external I/O justify them

Exit criteria:

- parallel feature work can happen without every test touching `socket.rs`

### Sprint 10: Crate Promotion Review

Goal:

- decide whether stable subsystems should become crates

Candidates:

- `atm-ci-core`
- GitHub provider adapter
- possibly shared lifecycle/state interfaces if reuse justifies it

Exit criteria:

- crate splits happen only after module boundaries are stable
- no crate is created just to mirror a directory split

## Initial Trait Candidates

These are the most likely trait boundaries after the first extractions:

- session registry access
- agent state store access
- roster/member lookup
- audit/log emission
- CI provider adapter

These are not immediate trait candidates:

- trivial response builders
- pure formatting helpers
- raw dispatch tables

## Deliverables Per Sprint

Each sprint should produce:

- one planning note or sprint description
- one bounded code change set
- focused validation output
- QA handoff
- a short findings update for the next sprint

## Recommended First Sprint

Start with **Sprint 1: Extract CI Monitoring Subsystem**.

Reason:

- it is already a subsystem
- it is low-risk compared to lifecycle/state refactors
- it removes obvious non-transport logic from `socket.rs`
- it creates a cleaner base for later provider and state-service work
