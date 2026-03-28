# Full Daemon Removal

## Purpose

This document captures the first-pass removal plan for `atm-daemon` based on
inspection of the current ATM workspace plus the adjacent tool workspaces:

- `sc-observability`
- `sc-compose`
- `schook`
- `scterm`

The goal is not to refactor the daemon internally. The goal is to remove the
daemon from ATM's required runtime path by moving each of its remaining product
responsibilities to a better owner.

## Current Daemon Concerns

The daemon currently owns four distinct concerns.

### 1. Observability Fan-In And Health Projection

The daemon is the write-side owner for ATM unified logging and related health
surfaces.

What the daemon owns today:

- producer fan-in from `atm`, `atm-agent-mcp`, and related binaries
- log writer lifecycle and spool merge/drain
- OTLP export thread startup and related health
- daemon-oriented status/doctor health projection

Representative current surfaces:

- `crates/atm/src/main.rs`
- `crates/atm-agent-mcp/src/main.rs`
- `crates/atm-daemon/src/main.rs`
- `crates/atm-daemon/src/daemon/log_writer.rs`
- `crates/atm/src/commands/logs.rs`

### 2. Session / State / Launch Ownership

The daemon owns the live runtime/session registry and several liveness-backed
CLI behaviors.

What the daemon owns today:

- session registry keyed by team/member
- hook-event processing and state transitions
- liveness checks used by cleanup/resume/spawn flows
- agent launch and related socket commands
- plugin roster/state coupling for synthetic members

Representative current surfaces:

- `crates/atm-daemon/src/daemon/session_registry.rs`
- `crates/atm-daemon/src/daemon/socket.rs`
- `crates/atm/src/commands/launch.rs`
- `crates/atm/src/commands/teams.rs`
- `crates/atm/src/commands/spawn.rs`

### 3. Live Attach / Injection / Stream State

The daemon currently owns live stream/control state used by the TUI and MCP
sidecar paths.

What the daemon owns today:

- socket commands for control and stream state
- normalization and storage of live stream event state
- TUI refresh integration against daemon `list-agents` / `agent-stream-state`
- MCP best-effort stream event delivery into the daemon

Representative current surfaces:

- `crates/atm-daemon/src/daemon/socket.rs`
- `crates/atm-agent-mcp/src/stream_emit.rs`
- `crates/atm-tui/src/main.rs`
- `crates/atm-tui/src/app.rs`

### 4. CI Monitor Runtime And Control

The daemon still owns the actual ci-monitor runtime even though a reusable
library crate now exists.

What the daemon owns today:

- plugin execute loop / shared poller lifecycle
- gh monitor health/state persistence
- daemon transport routing for `atm gh`
- plugin registration and daemon lifecycle control

Representative current surfaces:

- `crates/atm-daemon/src/plugins/ci_monitor/`
- `crates/atm-daemon/src/daemon/gh_monitor_router.rs`
- `crates/atm/src/commands/gh.rs`
- `crates/atm-ci-monitor/src/lib.rs`

## Dependency Gate

The key dependency gate is `schook` ATM hook-extension maturity.

Current conclusion:

- `schook` documents the right target model for canonical per-session state,
  normalized `agent_state`, and ATM lifecycle relays.
- However, the ATM hook extension is still documented as a planned ATM-specific
  extension boundary rather than a finished, release-grade runtime contract.

Why this matters:

- ATM cannot move session/state ownership out of the daemon until the hook-side
  source of truth is stable enough to replace daemon session tracking for real
  callers.
- If `schook` owns session/state too early, ATM risks deleting the daemon while
  still depending on a moving hook/runtime contract.

Required gate condition before Phase 2:

- the `schook` ATM hook extension must reach runtime-contract status in both
  behavior and documentation for the session/state responsibilities ATM will
  delegate to it

## Phased Removal Sequence

### Phase 1. Observability Adapter Extraction + Standalone CI Monitor Runtime

Daemon concern(s) removed:

- observability fan-in / daemon log-writer ownership
- ci-monitor runtime ownership

Replacement:

- an ATM-owned observability adapter layered over `sc-observability`
- a standalone ci-monitor runtime built around `atm-ci-monitor`

New ownership:

- `sc-observability` owns generic logging / routing / OTLP layers
- ATM-owned adapter keeps ATM-specific spool, fan-in, env/config translation,
  and health projection
- `atm-ci-monitor` owns ci-monitor behavior and execute loop

Result after Phase 1:

- the daemon is no longer needed as the mandatory log writer
- the daemon no longer owns ci-monitor as a plugin runtime
- `atm gh` can move toward a plugin-owned or standalone ci-monitor command path

### Phase 2. Session / State Ownership Moves To `schook`

Daemon concern(s) removed:

- session registry as the source of truth
- daemon-owned lifecycle/state transitions from hook relays
- daemon-backed liveness ownership used for stateful CLI decisions

Replacement:

- canonical session-state and normalized agent-state owned by `schook`
- ATM consumes hook-managed state rather than daemon-managed state

New ownership:

- `schook` owns lifecycle/session persistence and normalized agent state
- ATM CLI consumes state files / hook-owned context instead of daemon socket
  state

Result after Phase 2:

- after `schook` owns agent state tracking, the daemon should no longer be
  needed for session/state truth
- the remaining daemon value is narrowed to live attach/injection, daemon socket
  command surfaces, and any leftover launch/TUI coupling not yet moved

### Phase 3. Live Attach / Injection Moves To `scterm`

Daemon concern(s) removed:

- live stream state normalization
- live input/control path used by TUI / MCP session steering
- daemon as the coordination point for session injection

Replacement:

- `scterm` session core for attach/detach and PTY ownership
- `scterm` ATM bridge adapter for inbound ATM message injection

New ownership:

- `scterm` owns attach/detach, PTY input serialization, log replay, and live
  session interaction
- ATM integrates with `scterm` as a client/adapter instead of via daemon socket

Result after Phase 3:

- the daemon is no longer required for live agent interaction
- TUI and MCP can target `scterm`-owned session/runtime surfaces instead of
  daemon stream state

### Phase 4. Socket / Launch / TUI Dependency Retirement + Binary Removal

Daemon concern(s) removed:

- daemon socket protocol
- daemon launch path
- daemon management commands and binary packaging
- daemon-only TUI dependency surfaces

Replacement:

- direct ATM CLI ownership where still needed
- `scterm` or hook-owned state where daemon calls used to exist
- removal or delegation of daemon-specific CLI commands

New ownership:

- ATM owns mailbox/config/team-state CLI behavior only
- launch and live-session behaviors belong to the replacement owners already
  established in Phases 2 and 3

Result after Phase 4:

- `atm-daemon` can be removed from the required product runtime
- daemon-only binaries and daemon management commands can be retired

## Ownership Map By Tool

### `sc-observability`

What it replaces:

- generic logging layer
- routing / projector infrastructure
- OTLP exporter layer

What it does not replace by itself:

- ATM-specific fan-in semantics
- ATM-specific spool / replay / durability rules
- ATM-specific status/doctor/health JSON projections

Conclusion:

- `sc-observability` is a required building block, but ATM still needs its own
  adapter layer around it

### `sc-compose`

What it replaces:

- prompt/template composition concerns now bundled in the ATM workspace

What it does not replace:

- any daemon concern directly

Conclusion:

- `sc-compose` is adjacent to the re-platforming effort but should not sit on
  the daemon-removal critical path

### `schook`

What it replaces:

- daemon-owned hook relay / state ownership after the ATM extension reaches
  runtime-contract status

What it must prove first:

- canonical session persistence
- normalized `agent_state` transitions
- ATM lifecycle relay behavior that ATM callers can depend on as the new source
  of truth

Conclusion:

- `schook` is the critical gate for deleting daemon session/state ownership

### `scterm`

What it replaces:

- daemon live attach/control/injection ownership
- daemon stream-state coupling for TUI/MCP-oriented live interaction

What it does not replace by itself:

- mailbox/team-state semantics
- observability fan-in
- hook-owned session truth

Conclusion:

- `scterm` is the right owner for live session interaction after state truth and
  observability are moved elsewhere

## Open Questions

1. What is the minimum ATM CLI surface that must survive unchanged for daemon
   removal to count as complete?
2. Should `atm gh` remain an ATM-owned command namespace, or should it become a
   ci-monitor-owned plugin/extension surface?
3. How much health/status parity is required across `atm status`, `atm doctor`,
   and any daemon-era JSON outputs during the observability transition?
4. Does Phase 1 need a temporary ATM-local observability path before the final
   adapter settles, or can the ATM adapter land directly?
5. Which current TUI features must survive Phase 3, and which may be retired if
   `scterm` provides a different live interaction model?

## Risks

### Risk 1. `schook` Is Assumed Too Early

If ATM deletes daemon session/state ownership before `schook` reaches a stable
runtime contract, the replacement source of truth will still be moving.

### Risk 2. Logging Migration Is Underscoped

The daemon currently does most of the logging fan-in work. Removing it without
an ATM-owned observability adapter will create a product-wide regression, not a
small refactor.

### Risk 3. CI Monitor Extraction Stalls Again

`atm-ci-monitor` already exists, but the real runtime/orchestration still lives
under `atm-daemon`. Phase 1 must force ownership inversion, not another partial
library split.

### Risk 4. `scterm` Integration Arrives Too Late

If live attach/injection is left until after daemon socket removal work begins,
ATM will delete a runtime dependency before its replacement is ready.

## Recommendation

Proceed with removal in the four phases above.

The key sequencing rule is:

- do not start daemon binary retirement until observability fan-in, ci-monitor
  runtime, session/state truth, and live attach/injection each already have a
  replacement owner

That keeps daemon removal aligned with product ownership transfer instead of
turning it into a large, risky internal rewrite.
