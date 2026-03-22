# BB.5 Read-State Separation

## Status

- Sprint: `BB.5`
- Task: `BB5-DESIGN-001`
- Scope: design only, no implementation

## Problem Statement

Today `last_seen` is doing two different jobs:

1. It acts as a watermark for MCP/context injection, so already-seen mail is not re-injected into agent steering context.
2. It also acts as the default filter for `atm read`, which means messages can remain present and unread in the inbox while `atm read` returns nothing.

That dual use creates a semantic contradiction:

- `atm inbox` reports unread mail based on message state in the inbox.
- `atm read` hides messages based on a watermark that is not the same thing as explicit read/consume state.

The result is the current discrepancy where the inbox says "new mail exists" while `atm read` says "nothing to show".

## Goals

- Make `last_seen` MCP-only context state.
- Make `atm read` queue semantics depend on explicit per-message read/ack state.
- Preserve backward compatibility for existing inbox files.
- Keep the model easy to reason about for CLI, MCP, and inbox summary flows.

## Non-Goals

- No inbox file format rewrite in this sprint.
- No attempt to make `ack` and `read` the same action.
- No change to cross-team routing, dedup, or message transport.

## Proposed Model

Use two distinct mechanisms:

### 1. Watermark State: `last_seen`

`last_seen` remains external reader state stored in the seen-state file.

Purpose:

- MCP/context injection only
- optional explicit watermark-based CLI filtering

Rules:

- advanced only by MCP/context-injection flows or explicit watermark-oriented CLI operations
- not advanced by default `atm read`
- not used for `atm inbox` unread counts

### 2. Per-Message Read/Ack State

Read state remains message-local and queue-oriented.

Message fields:

- `read: bool`
- `acknowledgedAt: Option<timestamp>` or equivalent existing pending-ack/history state already derived from inbox messages

Purpose:

- determine whether a message is unread, pending-ack, or history
- drive default `atm read` bucket selection
- drive `atm inbox` "new" counts

Rules:

- unread means `read == false`
- pending-ack means `read == true` and not acknowledged
- history means acknowledged or otherwise outside the active queue bucket rules

## Invariants

1. `last_seen` and message read state are independent.
2. Reading mail through default `atm read` may mark messages as `read`, but must not advance `last_seen`.
3. MCP/context injection may advance `last_seen`, but must not mutate inbox message `read` state.
4. `atm inbox` unread counts are based on unread message state, not watermark position.
5. A message can be:
   - past the `last_seen` watermark but still unread
   - read but still pending ack
   - acknowledged and therefore history

## Affected Commands And Semantics

### `atm read`

New default behavior:

- filter/display by queue state, not `last_seen`
- unread and pending-ack buckets remain the default active queue
- history remains collapsed unless expanded
- reading with marking enabled advances message `read` state only

Flag behavior:

- `--no-mark` still prevents changing message read state
- `--unread-only` still shows only unread messages
- `--pending-ack-only` still shows only read-but-unacked messages
- `--history` still expands history

Watermark semantics:

- default `atm read` does not use or update `last_seen`
- `--since-last-seen` becomes an explicit opt-in watermark filter, not the default path
- `--no-since-last-seen` becomes unnecessary as the default inversion flag and should be deprecated after transition

### `atm inbox`

New default behavior:

- unread/new counts reflect unread message state only
- counts must stay in parity with what `atm read` would show as unread

This eliminates the current state where `atm inbox` reports unread messages hidden by a watermark-only filter.

### `atm read --timeout`

New default behavior:

- if unread or pending-ack messages already exist in the active queue, return immediately
- do not wait just because the `last_seen` watermark has already advanced

### `atm ack`

No fundamental semantic change in BB.5.

Interaction with the new model:

- `ack` continues moving messages out of pending-ack into history
- `ack` does not change `last_seen`

### ATM-MCP Context Injection

New default behavior:

- uses `last_seen` only
- does not inspect message `read` state as the source of truth for "already injected"
- reading mail in the CLI must not suppress future injection unless the injection path itself advances `last_seen`

## Data Storage Approach

The preferred BB.5 implementation path is:

1. Keep `last_seen` in the existing external seen-state store.
2. Continue using per-message inbox state for unread/read/ack transitions.
3. Change command semantics rather than inventing a second external spool/offset system.

Why this is preferred:

- lowest migration risk
- no inbox file schema expansion required to get the behavioral separation
- matches the conceptual model already present in the codebase

Alternative considered:

- a separate per-reader read cursor outside the inbox file

Rejected for BB.5 because:

- it would duplicate information already represented by `read` and ack state
- it increases reader-local complexity without solving a new product problem

## Migration Strategy

BB.5 should be backward-compatible with existing inbox files and seen-state files.

### Inbox Files

No format migration required.

Existing messages keep their current state:

- `read == false` remains unread
- `read == true` plus existing ack/pending state remains pending-ack/history according to current queue rules

### Seen-State File

Existing `last_seen` values remain valid, but their meaning narrows:

- before BB.5: CLI display filter + MCP watermark
- after BB.5: MCP watermark only, plus explicit CLI watermark filtering when requested

This is a semantic migration, not a storage migration.

### Default Behavior Transition

On upgrade:

- users may see unread messages in `atm read` that were previously hidden by `last_seen`
- that is correct behavior, not a migration bug

No attempt should be made to auto-mark those messages as read just because they are before the old watermark. That would preserve the wrong conflation.

## Implementation Notes

The likely implementation touchpoints are:

- `crates/atm/src/commands/read.rs`
  - remove watermark filtering as the default
  - keep explicit `--since-last-seen` support
  - ensure mark-as-read and watermark update are decoupled
- `crates/atm/src/commands/wait.rs`
  - immediate-return behavior when unread queue items already exist
- `crates/atm/src/commands/inbox.rs`
  - ensure summary counts align with explicit unread state
- `crates/atm/src/util/state.rs`
  - keep watermark storage but narrow its meaning
- `crates/atm-agent-mcp/src/atm_tools.rs`
  - keep context-injection semantics based on `last_seen`
  - ensure MCP read/count tools do not accidentally reintroduce the conflation

## Integration Test Coverage Plan

### Read-State Behavior

1. `atm read` advances read state and `atm inbox` unread count drops to zero.
   - seed unread messages
   - run default `atm read`
   - verify messages are now read/pending-ack as expected
   - verify `atm inbox` new count is `0`

2. `atm read --no-mark` leaves unread counts unchanged.
   - seed unread messages
   - run `atm read --no-mark`
   - verify `atm inbox` unread count remains unchanged

3. `atm read` no longer hides unread mail due to `last_seen`.
   - seed inbox with unread message older than watermark
   - set `last_seen` after the message timestamp
   - run default `atm read`
   - verify message is still shown

### Timeout Behavior

4. `atm read --timeout` returns immediately when unread messages already exist.
   - seed unread message
   - run timeout read
   - verify no blocking timeout wait occurs

5. `atm read --timeout` still waits when there are no unread or pending-ack queue items.
   - empty or fully acknowledged inbox
   - verify timeout path still behaves as expected

### Explicit Watermark Behavior

6. `atm read --since-last-seen` uses watermark semantics explicitly.
   - seed old and new messages around a watermark
   - verify only post-watermark messages are shown

7. default `atm read` does not advance `last_seen`.
   - record prior watermark
   - run default `atm read`
   - verify watermark is unchanged

### MCP Context Injection

8. MCP injection uses `last_seen` and reading does not suppress injection.
   - seed unread message before injection
   - run default `atm read`
   - verify watermark is unchanged
   - run MCP injection path
   - verify the message is still eligible based on watermark semantics until injection advances it

9. MCP injection advances watermark without mutating read state.
   - seed unread message
   - run injection path
   - verify `last_seen` moves forward
   - verify inbox message `read` state is unchanged

### Parity Tests

10. `atm inbox` before/after `atm read` stays in parity with queue state.
    - unread before read
    - zero unread after marked read
    - pending-ack/history behavior remains internally consistent

## Recommended Sprint Scope

BB.5 should be treated as a message-management sprint, not folded into BB.4.

Reason:

- it changes user-visible queue semantics
- it affects CLI, timeout semantics, inbox summary, and MCP behavior together
- it needs dedicated integration coverage to prevent reintroducing inbox/read divergence

## Open Question

The only meaningful follow-up decision is whether `--no-since-last-seen` should be retained as a compatibility alias after the default changes.

Recommendation:

- keep it temporarily for compatibility
- document it as deprecated once default behavior is queue-state based
