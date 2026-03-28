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
- `crates/atm-daemon/src/roster/`

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
- `crates/atm-tui/src/daemon_launch.rs`
- `crates/atm-daemon/src/plugins/worker_adapter/`

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

### 5. Plugin Host Framework

The daemon owns a general plugin-host runtime that survives even if ci-monitor
is extracted.

What the daemon owns today:

- plugin trait / erased trait abstraction
- plugin registry and plugin lifecycle management
- plugin context and shared mail service wiring
- plugin-oriented runtime composition in the daemon binary

Representative current surfaces:

- `crates/atm-daemon/src/plugin/`
- `crates/atm-daemon/src/plugins/mod.rs`
- `crates/atm-daemon/src/main.rs`

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

Gate condition:

- ATM observability adapter is merged and used by ATM binaries without daemon
  log-writer ownership
- ci-monitor runtime and command implementation live outside daemon-owned
  plugin code
- daemon no longer owns ci-monitor business logic; if a daemon plugin remains,
  it is adapter-only

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
- ATM or hook-owned state updates replace daemon roster/session ownership
  semantics for live members

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
- worker-adapter responsibilities move to `scterm` or are retired if they are
  daemon-specific glue only

Gate condition:

- `schook` gate is satisfied and Phase 2 is complete
- `scterm` provides the required live attach/injection/session surfaces needed
  by ATM callers
- no required live interaction flow still depends on daemon-owned
  `worker_adapter` behavior without an assigned replacement

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
- daemon plugin host framework
- daemon roster and daemon-core client/stream helper coupling

Replacement:

- direct ATM CLI ownership where still needed
- `scterm` or hook-owned state where daemon calls used to exist
- removal or delegation of daemon-specific CLI commands
- retirement of the plugin host framework once no remaining daemon-hosted
  plugins exist

New ownership:

- ATM owns mailbox/config/team-state CLI behavior only
- launch and live-session behaviors belong to the replacement owners already
  established in Phases 2 and 3

Gate condition:

- Phases 1 through 3 are complete
- no required ATM binary still calls daemon socket/client surfaces
- no required ATM binary still depends on `atm-daemon-launch`
- no required daemon-hosted plugin remains
- no required TUI path still depends on daemon launch or daemon stream state

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

## Additional Retirement Targets

The final daemon-removal phase must explicitly retire the remaining daemon
couplings outside `crates/atm-daemon`.

Targets to retire in Phase 4:

- `crates/atm-daemon-launch/`
- `crates/atm-core/src/daemon_client.rs`
- `crates/atm-core/src/daemon_stream.rs`
- `crates/atm/src/commands/daemon.rs`
- daemon-specific launch/TUI wiring such as
  `crates/atm-tui/src/daemon_launch.rs`

Worker-adapter-specific targets to move or retire across Phases 2 and 3:

- `crates/atm-daemon/src/plugins/worker_adapter/agent_state.rs`
- `crates/atm-daemon/src/plugins/worker_adapter/hook_watcher.rs`
- `crates/atm-daemon/src/plugins/worker_adapter/capture.rs`
- `crates/atm-daemon/src/plugins/worker_adapter/codex_tmux.rs`
- `crates/atm-daemon/src/plugins/worker_adapter/pubsub.rs`
- `crates/atm-daemon/src/plugins/worker_adapter/tmux_sender.rs`
- `crates/atm-daemon/src/plugins/worker_adapter/lifecycle.rs`
- `crates/atm-daemon/src/plugins/worker_adapter/nudge.rs`

Planned ownership:

- hook-watching and normalized state responsibilities move with Phase 2 to
  `schook`
- tmux/session interaction and live attach/injection responsibilities move with
  Phase 3 to `scterm`
- daemon-specific pubsub/router glue is retired in Phase 4 rather than migrated

Roster retirement target:

- `crates/atm-daemon/src/roster/`

Planned ownership:

- roster semantics move with Phase 2 to the hook/state-driven ownership model
  or into ATM CLI-owned file/state updates, depending on the final `schook`
  contract

Plugin host retirement target:

- `crates/atm-daemon/src/plugin/`

Planned ownership:

- no new owner is required if ci-monitor and worker-adapter responsibilities
  have already moved out; the framework is retired in Phase 4 with the daemon
  binary

## Open Questions

1. What is the minimum ATM CLI surface that must survive unchanged for daemon
   removal to count as complete?
2. Should `atm gh` remain an ATM-owned command namespace, or should it become a
   ci-monitor-owned plugin/extension surface?
3. How much health/status parity is required across `atm status`, `atm doctor`,
   and any daemon-era JSON outputs during the observability transition?
4. Which current TUI features must survive Phase 3, and which may be retired if
   `scterm` provides a different live interaction model?

## Risks

### Risk 1. `schook` Is Assumed Too Early

If ATM deletes daemon session/state ownership before `schook` reaches a stable
runtime contract, the replacement source of truth will still be moving.

### Risk 2. Logging Migration Is Underscoped

The daemon currently does most of the logging fan-in work. Removing it without
an ATM-owned observability adapter will create a product-wide regression, not a
small refactor.

Mitigation:

- Phase 1 should use an interim compatibility shim only if needed, but the
  default plan should be direct ATM adapter adoption rather than an additional
  temporary logging architecture
- ATM binaries should switch to the ATM-owned observability adapter before
  ci-monitor or daemon-binary removal proceeds
- status/doctor/log surfaces must be proven against the adapter before Phase 1
  is considered complete

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

The specific sequencing answer for observability is:

- do not create a long-lived temporary observability path unless the direct ATM
  adapter migration proves too disruptive
- the default Phase 1 plan should be to land the ATM-owned adapter directly and
  treat that as the compatibility layer for the rest of the removal work

That keeps daemon removal aligned with product ownership transfer instead of
turning it into a large, risky internal rewrite.
