# CLI Requirements

**Status**: Planned (Phase BA)
**Scope**: `atm` CLI, CLI-facing shared contracts in `atm-core`, inbox/message-management UX
**See also**:
- `docs/cli/architecture.md`
- `docs/project-plan.md` (Phase BA)
- `docs/requirements.md` (secondary requirements registry)

## 1. Purpose

Define the source-of-truth requirements for:

- CLI command ownership and boundary rules
- inbox/message-management behavior
- plugin command advertisement and availability
- task-acknowledgement workflow semantics

This document exists to keep CLI policy separate from observability,
daemon-spawn, and plugin-specific subsystem contracts.

## 2. Command Ownership Contract

### 2.1 Core vs Plugin Commands

- Core ATM commands (`send`, `read`, `inbox`, `ack`, `teams`, `doctor`,
  `daemon`, `status`, `cleanup`, `mcp`, `bridge`, `members`) are owned by the
  `atm` CLI and the neutral contracts in `atm-core`.
- Plugin-provided command namespaces are owned by the plugin/provider layer,
  not by ad hoc CLI command implementations.
- The CLI may provide generic routing, help, bootstrap, and availability UX for
  plugin commands, but it must not embed plugin implementation logic.

### 2.2 Plugin Command Advertisement

- A plugin command namespace must be advertised through a neutral capability
  contract.
- If the plugin is not present in the build/runtime, the namespace must not
  appear in standard CLI UX.
- If the plugin is present but not enabled, only bootstrap/management commands
  may appear.
- If the plugin is present and enabled, the full namespace may appear.

For the GitHub monitor stack specifically:

- the gh plugin/provider layer owns GitHub command semantics
- the CLI may route `atm gh ...` but must not directly depend on daemon plugin
  implementation modules to do so

### 2.3 CLI Boundary Rules

- `atm` may depend on `atm-core` and other neutral contract crates.
- `atm` must not depend on daemon plugin implementation modules for normal
  command execution.
- `atm` must not import provider-specific helper functions from daemon plugin
  crates.
- CLI-only UX concerns (rendering, `clap` routing, help text, summaries) must
  remain in the CLI crate.
- Product-layer command execution must cross the boundary via neutral request /
  response contracts, not direct plugin helper imports.

## 3. Inbox and Message-Management Requirements

### 3.1 `atm send`

- `atm send` must send the message and report the recipient's current state as
  immediate feedback when that state is available.
- `atm send` must not create notification subscriptions as an implicit side
  effect.
- The sender's own lifecycle state must be updated through explicit lifecycle
  logic, not by piggybacking on recipient-idle subscription behavior.

### 3.2 Lifecycle Notification Subscriptions

- Busy/idle notifications for orchestration must be explicit and edge-triggered.
- Subscription policy should be configured declaratively (for example in
  `.atm.toml`) and must not be mutated by normal `atm send` calls.
- Team-lead may subscribe to selected agents' state edges for orchestration.
- Repeated steady-state idle reminders are forbidden; only transitions should
  notify.

### 3.3 Idle Notifications

- An inbox must retain at most one idle notification per sender identity.
- A newer idle notification supersedes older idle notifications from the same
  sender.
- Idle-notification suppression/deduplication should happen at write time where
  possible, not solely through later cleanup.

### 3.4 `atm inbox clear`

- ATM must provide a first-class inbox cleanup command.
- Users must not be expected to hand-edit inbox JSON files.
- Default cleanup behavior must remove idle notifications without requiring a
  dedicated idle-only flag.
- Additional selectors such as `--acked` and `--older-than <duration>` may
  further constrain cleanup scope.
- `--idle-only` is allowed as a focused mode, but idle cleanup must not depend
  on that flag.
- Cleanup should support `--dry-run` before destructive execution.

### 3.5 `atm read`

- `atm read` must behave like a queue, not a flat replay dump.
- Default output must prioritize:
  1. unread actionable
  2. read/pending-ack
  3. historical entries only when explicitly requested
- Within buckets, messages must be shown newest-first.
- Focused filtering modes must exist for unread-only and pending-ack-only use.
- Human and JSON output must include summary counts for each bucket.
- Reader-local presentation state may be used to suppress already-presented
  history without mutating the shared inbox schema.

### 3.6 `atm ack`

- Normal task acknowledgement must be message-id bound and explicit.
- The target behavior is a single canonical acknowledgement path:
  `atm ack <message-id> "<reply>"`
- That action must be atomic:
  - mark the specific source message acknowledged
  - send the visible reply linked to that source message
- If either step fails, neither side should commit.
- Plain `atm send` must not count as acknowledging a task assignment.
- Bulk maintenance operations may exist separately, but they must not be the
  normal task-acceptance workflow.

## 4. Cross-Team Messaging Requirements

- Fully-qualified sender identities like `team-lead@src-dev` must be accepted
  and preserved in inbox message records.
- Cross-team responses must route back to the originating team identity rather
  than silently defaulting to the responder's local team.
- Cross-team task messages should carry explicit response guidance for runtimes
  that cannot safely infer the proper ATM reply target on their own.

## 5. Current Boundary Findings Driving Phase BA

The current codebase still contains CLI/plugin coupling that Phase BA must
remove:

- `crates/atm/Cargo.toml` has non-dev dependencies on
  `agent-team-mail-ci-monitor` and `agent-team-mail-daemon`
- `crates/atm/src/commands/gh.rs` imports
  `agent_team_mail_daemon::plugins::ci_monitor::*`
- `crates/atm/src/commands/doctor.rs` imports the daemon-plugin-owned helper
  `run_attributed_gh_command_with_ids`

These are boundary violations against the target command-ownership model above.

## 6. Phase BA Implementation Targets

- BA.1: idle dedupe/suppression + explicit inbox-clear behavior
- BA.2: queue-style `atm read` + atomic acknowledgement workflow
- BA.3: CLI/plugin command-boundary extraction
- BA.4: CLI boundary enforcement + plugin availability UX
