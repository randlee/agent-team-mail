# Phase BA Planning: CLI Message Management + Plugin Boundary

**Status**: Planned
**Primary docs**:
- `docs/cli/requirements.md`
- `docs/cli/architecture.md`

## Goal

Stabilize ATM as an operational task queue and clean command boundary:

- remove idle-notification inbox bloat
- make `atm read` act like a queue instead of replaying stale backlog forever
- make task acknowledgement explicit, atomic, and message-bound
- remove current CLI coupling to daemon plugin implementation code
- establish plugin-advertised command namespaces instead of CLI-owned plugin
  semantics

## Problem Statement

Current dogfood issues fall into two linked groups:

### Message-management problems

- idle notifications dominate inbox volume
- `atm send` currently auto-subscribes the sender to recipient idle events,
  creating hidden notification churn
- `atm read` presents old pending-ack items as a flat oldest-first stream
- task acknowledgement is split between inbox-state mutation and a separate
  visible reply
- there is no first-class operator-friendly inbox cleanup command

### CLI boundary problems

- `crates/atm/Cargo.toml` depends directly on `agent-team-mail-daemon` and
  `agent-team-mail-ci-monitor`
- `crates/atm/src/commands/gh.rs` imports daemon plugin implementation helpers
- `crates/atm/src/commands/doctor.rs` imports a plugin-owned GitHub helper
- plugin command ownership is blurred: the CLI effectively implements plugin
  behavior instead of routing through plugin capability contracts

## Sprint Map

| Sprint | Focus | Primary issues / findings |
|---|---|---|
| BA.1 | Idle lifecycle hygiene + inbox clear | `#932`, `#933`, dogfood idle-notification growth |
| BA.2 | Queue semantics + atomic ack workflow | `#927`, `#928`, `#929`, `#930`, `#931` |
| BA.3 | CLI/plugin command-boundary extraction | current `atm -> daemon/ci-monitor` coupling audit |
| BA.4 | Boundary enforcement + plugin availability UX | CI boundary gate + capability-advertised namespace behavior |

The requested “sprint or two” for CLI/core/plugin boundary cleanup are BA.3 and
BA.4.

## BA.1 — Idle Lifecycle Hygiene + Inbox Clear

**Issues**: `#932`, `#933`

**Deliverables**:

- remove implicit send-time idle subscription side effects from `atm send`
- add automatic post-send idle lifecycle transition for non-Claude agents
- dedupe/suppress idle notifications at write time so inboxes retain at most
  one idle notification per sender
- add first-class `atm inbox clear` UX with:
  - default idle-notification removal
  - `--acked`
  - `--older-than <duration>`
  - `--idle-only`
  - `--dry-run`
- preserve send-time recipient state feedback without hidden subscriptions

**Acceptance**:

- repeated idle notifications no longer accumulate in normal inboxes
- `atm send` no longer creates recipient-idle subscriptions as a side effect
- non-Claude agents transition busy -> idle after successful send completion
- `atm inbox clear` removes idle notifications by default

## BA.2 — Queue Semantics + Atomic Ack Workflow

**Issues**: `#927`, `#928`, `#929`, `#930`, `#931`

**Deliverables**:

- unread / pending-ack / history queue buckets in `atm read`
- newest-first ordering within buckets
- `--unread-only` and `--pending-ack-only`
- summary header + JSON bucket counts
- reader-local history-collapse support
- canonical message-bound task acknowledgement flow:
  - `atm ack <message-id> "<reply>"`
- visible task reply and inbox-state acknowledgement become one atomic action

**Acceptance**:

- long-running agent workflows surface new tasks ahead of stale backlog
- replay noise is materially reduced on repeated reads
- task acknowledgement is explicit, auditable, and message-id bound

## BA.3 — CLI/Plugin Command-Boundary Extraction

**Findings driving the sprint**:

- `crates/atm/Cargo.toml` currently depends on `agent-team-mail-daemon`
  and `agent-team-mail-ci-monitor`
- `crates/atm/src/commands/gh.rs` imports
  `agent_team_mail_daemon::plugins::ci_monitor::*`
- `crates/atm/src/commands/doctor.rs` imports
  `run_attributed_gh_command_with_ids` from the daemon plugin layer

**Deliverables**:

- define or extract neutral plugin capability / command contracts for CLI use
- remove direct daemon plugin imports from `atm`
- move CLI-facing GH command data shaping behind neutral contracts
- document the command ownership rule:
  - plugin owns semantics
  - CLI owns routing/help/rendering

**Acceptance**:

- `atm` no longer imports daemon plugin implementation modules for GH command
  behavior
- direct CLI dependency on plugin implementation crates is removed or reduced to
  neutral contract crates only
- `atm gh` no longer depends on daemon plugin implementation helpers

## BA.4 — Boundary Enforcement + Plugin Availability UX

**Deliverables**:

- capability-advertised plugin namespace model
- absent / present-disabled / present-enabled UX rules for plugin commands
- CI/lint gate that forbids new CLI imports from daemon plugin modules
- docs/tests for plugin command availability and boundary enforcement

**Acceptance**:

- plugin namespaces are shown according to capability state, not hard-coded
  implementation ownership
- CI fails if the CLI reintroduces daemon-plugin implementation imports
- the CLI availability model is documented and tested

## Exit Criteria

1. Idle-notification inbox growth is controlled by write-time suppression and
   default cleanup.
2. `atm send` no longer mutates notification subscriptions implicitly.
3. `atm read` behaves like a task queue with bucketed visibility.
4. Task acknowledgement is message-id bound and atomic.
5. `atm` no longer depends directly on daemon plugin implementation code for GH
   command behavior.
6. Plugin namespace availability is capability-driven and covered by docs/tests.
