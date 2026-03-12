---
issue: 682
title: "ATM read message-state clarity"
date: 2026-03-12
worktree: fix/issue-682-atm-read-message-state
status: implemented
---

# Issue #682: ATM Read Message-State Clarity

## Problem

`atm read` previously marked messages `read=true` on first display and then hid them
from the default unread-only view. That collapsed two different states into one:

- delivered and merely seen
- seen and actually acknowledged/actioned

In dogfood, that made assignments appear to disappear after first read even when the
recipient had not explicitly acknowledged them yet.

## Decision

Keep the existing `read: bool` field for compatibility, but add explicit
pending/acknowledged tracking through forward-compatible inbox metadata:

- `pendingAckAt`
- `acknowledgedAt`

These are stored in `InboxMessage.unknown_fields`, so no schema migration is needed
and older clients still preserve them.

## Implemented Behavior

### Inbox message helpers

`crates/atm-core/src/schema/inbox_message.rs`

- `pending_ack_at()`
- `acknowledged_at()`
- `is_acknowledged()`
- `is_pending_action()`
- `mark_pending_ack()`
- `mark_acknowledged()`

Pending-action semantics are:

- unread messages are pending
- newly read messages become pending via `pendingAckAt`
- legacy historical `read=true` messages without `pendingAckAt` remain non-pending
- acknowledged messages are not pending

### CLI changes

`crates/atm/src/commands/read.rs`

- default visibility is now pending-action messages, not raw unread-only
- first read marks a message as both `read=true` and `pendingAckAt=<ts>`
- human output now shows explicit state markers
- pending messages print their `message_id` so they can be acknowledged directly

`crates/atm/src/commands/ack.rs`

- new CLI: `atm ack <message-id>...`
- also supports `--all-pending`
- writes `acknowledgedAt` and clears `pendingAckAt`

`crates/atm/src/commands/status.rs`

- status JSON now includes both `unreadCount` and `pendingCount`
- human status output surfaces `pending` counts so read-but-unacked work is visible

`crates/atm/src/commands/inbox.rs`

- non-last-seen inbox summaries now show `Pending` counts instead of `Unread`
- since-last-seen mode also treats pending-action messages as visible

## Why this design

This is smaller and safer than introducing a new enum state machine everywhere:

- no breaking schema change
- no migration step
- legacy `read=true` messages do not get resurrected as pending
- CLI behavior becomes explicit enough for the dogfood workflow immediately

## Validation

Validated with:

- `cargo test --workspace`
- `cargo clippy --all-targets --all-features -- -D warnings`

Added/updated regression coverage for:

- read marks `pendingAckAt`
- repeated read keeps pending work visible until explicit ack
- `atm ack` clears pending state
- inbox summary shows pending counts
- status JSON reports `pendingCount`
