# Phase AM Planning

**Status**: Draft
**Focus**: CI-monitoring refactor sprints

## Goal

Phase AM starts the daemon refactor by extracting CI monitoring out of
`socket.rs` and stabilizing the first subsystem boundary before deeper
lifecycle/state refactors begin.

The intent is to:

- remove obvious non-transport logic from `socket.rs`
- separate provider-agnostic CI business logic from GitHub-specific behavior
- create interfaces that Azure or other CI providers can adopt later
- avoid a premature crate split until boundaries are proven stable

This phase is intentionally narrower than a full daemon architecture rewrite.

## Non-Goals

Phase AM does not attempt to:

- redesign canonical agent-state ownership
- refactor hook or watcher lifecycle handling yet
- introduce `atm-ci` or `atm-gh` crates immediately
- change user-visible CI monitor behavior unless needed to preserve existing semantics

## Desired End State for Phase AM

At the end of Phase AM:

- `socket.rs` should dispatch CI monitor requests instead of containing CI monitor policy
- CI monitor business logic should live in a dedicated subsystem under `atm-daemon`
- GitHub-specific logic should be isolated behind a provider-shaped boundary
- CI monitor tests should be organized around subsystem boundaries instead of socket-only entrypoints
- the code should be ready for a later crate split if the boundaries hold

## Proposed Module Shape

Initial target shape inside `crates/atm-daemon/src/plugins/ci_monitor/`:

- `mod.rs`
  Public entrypoint and exports
- `plugin.rs`
  Plugin lifecycle wiring only
- `service.rs`
  Core CI monitor business logic and orchestration
- `state.rs`
  Monitor state model and transitions
- `routing.rs`
  Notify-target resolution and message shaping helpers
- `health.rs`
  Availability/degraded/disabled status handling
- `github.rs`
  GitHub adapter surface
- `provider.rs`
  Provider trait and provider-neutral request/response types

This structure is a target, not a requirement to land all at once.

## Interface Direction

### CI Service Boundary

The service layer should own:

- monitor start/stop/restart behavior
- run/workflow/PR monitor orchestration
- status transitions
- dedup decisions
- notification/report decisions
- structured failure/progress classification

The service should not know about raw socket requests.

### GitHub Adapter Boundary

The GitHub adapter should own:

- `gh` CLI invocation
- GitHub-specific payload parsing
- repo/provider-specific URL and status translation

The GitHub adapter should not own:

- cross-provider CI policy
- notification routing
- daemon plugin lifecycle

### Socket Boundary

`socket.rs` should only:

- parse request payload
- validate command shape
- call CI subsystem entrypoints
- translate subsystem results into socket responses

It should not keep CI monitor policy, state transitions, or provider logic.

## Sprint Sequence

### AM.1 Extract CI Domain Types and Shared Helpers

Scope:

- move CI monitor domain types, report/health helpers, and pure logic out of
  `socket.rs` into `plugins/ci_monitor`
- keep existing behavior and call sites stable

Key work:

- extract shared CI monitor data structures
- move pure formatting/classification helpers
- extract shared test support used by CI monitor tests

Validation:

- targeted CI monitor unit tests
- existing socket tests for CI monitor commands

Exit criteria:

- `socket.rs` no longer holds raw CI monitor helper logic that does not need
  socket context

### AM.2 Introduce CI Monitor Service

Scope:

- create a service entrypoint inside `plugins/ci_monitor`
- move orchestration logic out of socket handlers

Key work:

- add service methods for:
  - namespace status
  - PR monitor
  - workflow monitor
  - run monitor
  - restart/status operations
- keep plugin lifecycle separate from service orchestration

Validation:

- focused service tests
- existing end-to-end `atm gh` tests

Exit criteria:

- socket handlers become thin dispatch wrappers around service calls

### AM.3 Split Provider-Neutral Logic from GitHub Adapter

Scope:

- separate GitHub-specific fetch/translate logic from shared CI policy

Key work:

- define provider-neutral request/response types
- define provider trait for fetching run/job/log/report inputs
- move `gh`-specific operations to `github.rs`

Validation:

- GitHub adapter tests
- service tests using a mock provider

Exit criteria:

- Azure support would be an adapter implementation, not a second policy copy

### AM.4 Extract Routing and Notification Policy

Scope:

- isolate target resolution and notification construction

Key work:

- move notify-target interpretation into `routing.rs`
- isolate mail/report payload shaping
- keep failure/progress/final-summary decisions in one place

Validation:

- routing tests
- notification payload tests
- regression coverage for requirements in `docs/ci-monitoring/requirements.md`

Exit criteria:

- notification behavior is defined in subsystem code, not mixed into service or socket glue

### AM.5 Extract Health and Availability State Handling

Scope:

- isolate `healthy` / `degraded` / `disabled_config_error` lifecycle rules

Key work:

- move health snapshot construction and state transition helpers into `health.rs`
- make service/plugin call those interfaces instead of scattering status logic

Validation:

- health state transition tests
- CLI status/report contract tests

Exit criteria:

- CI monitor availability state has a clear implementation home

### AM.6 Thin `socket.rs` and Reorganize Tests

Scope:

- remove remaining CI-monitor business logic from `socket.rs`
- consolidate test support around the new subsystem boundaries

Key work:

- leave only request parsing, dispatch, and response shaping in `socket.rs`
- move CI-monitor-specific test fixtures/builders into subsystem test support

Validation:

- targeted CI monitor suite
- broader daemon/socket regression pass

Exit criteria:

- CI monitoring is clearly a subsystem, not a socket sidecar

## Traits to Introduce in Phase AM

Traits are useful here, but only at the subsystem boundaries.

Likely traits:

- `CiProvider`
  Provider-neutral fetch interface implemented first by GitHub
- `CiNotifier` or equivalent small mail/report emission boundary if notification logic remains hard to test
- `CiStatusStore` only if state persistence/health writes need isolation for tests

Traits to avoid for now:

- trivial helper traits
- dispatch/response traits
- generic trait wrappers around everything moved out of `socket.rs`

## Testing Strategy

Every AM sprint should keep the same verification pattern:

1. Focused tests for the moved CI-monitor domain.
2. One broader daemon/socket regression pass.
3. Clippy/fmt on touched crates.
4. QA review before starting the next sprint.

Recommended permanent test split after AM:

- provider adapter tests
- CI service tests
- routing/health tests
- daemon/socket integration tests

## Risks

### Hidden coupling to `socket.rs`

Risk:

- CI monitor behavior may still rely on socket-local helpers or shared fixture setup

Mitigation:

- extract shared test support early
- keep each sprint mechanically narrow

### Premature crate split

Risk:

- moving to `atm-ci` / `atm-gh` too early could create dependency churn instead of clarity

Mitigation:

- stabilize the module boundaries first
- revisit crate promotion only after AM completes

### Behavior drift during extraction

Risk:

- monitor status, routing, or health semantics could drift accidentally during code movement

Mitigation:

- document invariants before each sprint
- preserve existing requirements coverage
- run focused tests after every extraction

## Deliverables

Phase AM should deliver:

- a cleaner `plugins/ci_monitor` subsystem
- materially smaller and clearer CI-related surface in `socket.rs`
- a provider-shaped boundary for GitHub-specific code
- a test layout that supports later provider additions and deeper daemon refactors

## Recommendation

Start with **AM.1** immediately after the planning review lands.

Reason:

- it is the lowest-risk mechanical extraction
- it prepares the service/provider split without forcing interface decisions too early
- it should reduce `socket.rs` complexity quickly while preserving behavior
