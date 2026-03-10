# Remove Member Design

## Scope

`atm teams remove-member <team> <agent>` removes a non-lead member from the roster and cleans up mailbox state.

## Safety Rules

- `team-lead` is protected and cannot be removed.
- The command checks liveness before mutation unless `--force` is present.
- If liveness is active, removal is refused by default.
- If liveness is unknown, removal is refused by default.

## External Members

External members can have daemon-tracked state keyed by `session_id`.

- If an external member has a `session_id`, ATM queries daemon state for that key.
- If an external member has no `session_id`, liveness is treated as unknown and the command requires `--force`.

This is a deliberate safety choice. `remove-member` is destructive and should not infer that an untracked external member is safe to remove.

## Inbox Archival

`--archive-inbox` copies the member inbox to:

`~/.claude/teams/.archives/<team>/removed-<agent>-<timestamp>/inboxes/<agent>.json`

Archived inboxes live outside `.backups/` so normal backup pruning does not delete them.
