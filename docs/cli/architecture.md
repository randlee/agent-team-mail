# CLI Architecture

**Status**: Planned (Phase BA)
**Primary crate**: `atm`
**See also**:
- `docs/cli/requirements.md`
- `docs/project-plan.md` (Phase BA)
- `docs/arch-boundary.md`

## 1. Architecture Goals

- Keep CLI rendering and command dispatch thin and explicit.
- Keep durable state, schemas, and neutral contracts in shared crates.
- Prevent the CLI from depending directly on daemon plugin implementation code.
- Make inbox/message-management behavior operationally sane for long-running
  teammate workflows.

## 2. Components

### 2.1 `atm`

Owns:

- `clap` command tree and user-facing help
- argument parsing
- output formatting
- human/JSON rendering
- CLI-local UX flows

Must not own:

- plugin/provider business logic
- provider-specific helper implementations
- ad hoc daemon plugin imports for command semantics

### 2.2 `atm-core`

Owns neutral contracts:

- inbox schema and file I/O
- state schema
- daemon-client request/response contracts
- config discovery
- shared command payload and capability metadata contracts

### 2.3 `atm-daemon`

Owns:

- plugin registry
- plugin lifecycle
- plugin capability advertisement
- plugin command execution behind neutral contracts

### 2.4 Plugin/Provider Layer

Owns:

- plugin-specific command semantics
- provider-specific execution
- plugin capability descriptors

The plugin/provider layer must not be imported directly by the CLI for normal
command behavior.

## 3. Target Command Flow

### 3.1 Core Commands

For core commands:

1. CLI parses command.
2. CLI calls neutral shared/core contract.
3. Core/daemon performs work.
4. CLI renders result.

### 3.2 Plugin Commands

For plugin commands:

1. Plugin advertises its namespace and capabilities through a neutral contract.
2. CLI exposes that namespace based on capability state.
3. CLI forwards command intent through a neutral command request surface.
4. Daemon/plugin executes the command.
5. CLI renders the neutral response.

This keeps the CLI as a router/render layer rather than a plugin
implementation host.

## 4. Plugin Availability Model

The CLI must present plugin namespaces according to runtime state:

| State | CLI behavior |
|---|---|
| Plugin absent | Namespace hidden from normal UX |
| Plugin present but disabled | Only bootstrap / enable / status / management UX shown |
| Plugin present and enabled | Full namespace shown |

For `atm gh` specifically:

- if the gh plugin/provider is absent, normal `atm gh` UX should not be
  advertised
- if it is present but not enabled, only bootstrap/management affordances should
  appear
- if it is enabled, the CLI may present the full namespace while still routing
  execution through neutral plugin contracts

## 5. Inbox and Task-Queue Architecture

### 5.1 Queue Presentation

The queue problem is primarily a presentation and lifecycle problem, not a raw
transport problem.

Target model:

- unread actionable
- pending acknowledgement
- historical/collapsed

`atm read` should derive these buckets from shared inbox state and reader-local
presentation state.

### 5.2 Cleanup

Idle notifications are lifecycle chatter, not durable work items. The target
architecture is:

- suppress/dedupe them at write time
- remove them by default in `atm inbox clear`
- keep cleanup as a first-class CLI command rather than asking operators to
  prune JSON manually

### 5.3 Task Acknowledgement

Task acknowledgement should become a single message-bound atomic action rather
than today's split state/send workflow.

Target flow:

1. operator/agent chooses exact source `message-id`
2. CLI performs one atomic `ack + reply`
3. shared inbox state and visible conversation remain in sync

## 6. Current Violations

The current codebase does not yet meet the target architecture:

### 6.1 CLI -> Plugin Coupling

- `crates/atm/src/commands/gh.rs` imports daemon plugin implementation helpers
  directly from `agent_team_mail_daemon::plugins::ci_monitor`
- `crates/atm/src/commands/doctor.rs` imports a direct
  `agent_team_mail_ci_monitor` helper block for GH observer/ledger state
- `crates/atm/src/commands/doctor.rs` imports a daemon-plugin-owned helper for
  GitHub execution
- `crates/atm/src/main.rs` directly calls
  `agent_team_mail_ci_monitor::flush_gh_observability_records()` on shutdown

### 6.2 Crate Dependency Coupling

`crates/atm/Cargo.toml` currently carries non-dev dependencies on:

- `agent-team-mail-ci-monitor`
- `agent-team-mail-daemon`
- `agent-team-mail-daemon-launch`

That means the CLI crate is compiled against plugin-host and plugin-specific
implementation crates rather than purely neutral contracts.

## 7. Recommended Boundary Repair

Phase BA should repair the CLI boundary in two sprints:

### BA.3 — Command Boundary Extraction

- extract neutral plugin capability and command contracts into `atm-core`
  (recommended module family: `atm_core::plugin_contract` or
  `atm_core::gh_command`)
- remove direct daemon-plugin imports from the CLI
- move CLI-needed plugin data shaping behind neutral shared contracts
- move the current `doctor.rs` GH observer/ledger imports behind the same
  neutral contract surface
- resolve the `main.rs` shutdown flush path either by moving it behind an
  `atm-core` lifecycle hook or explicitly documenting a narrow permitted
  teardown exception
- stop treating `atm gh` as a CLI-owned implementation surface

### BA.4 — Boundary Enforcement and UX

- demote `agent-team-mail-daemon` and `agent-team-mail-ci-monitor` from
  non-dev `crates/atm/Cargo.toml` dependencies after BA.3 so the compiler
  enforces the boundary
- wire plugin namespace availability from capability descriptors
- add CI checks that forbid new CLI imports from daemon plugin modules
- keep a secondary grep/lint gate as belt-and-suspenders after dep demotion
- explicitly classify `agent-team-mail-daemon-launch` as either:
  - a permitted CLI dependency for canonical launcher lifecycle wiring
  - or a follow-on boundary violation to remove
- document the absent/present/enabled plugin command states

## 8. Architectural Direction for Cross-Team Commands

Cross-team command routing should preserve fully-qualified identity at the UX
boundary:

- qualified sender identity is valid data
- qualified destination identity is the routing contract
- the CLI should help users respond to cross-team messages correctly rather than
  silently collapsing the target back to the local team
