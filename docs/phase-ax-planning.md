# Phase AX Planning: Dev Install Hardening + Inbox/TUI Cleanup

**Status**: In progress
**Integration branch**: `integrate/phase-AX`
**Prerequisite**: Phase AU complete and merged

## Goal

Phase AX hardens two high-friction operational areas:

- dev-install and shared dev-daemon dogfooding
- inbox/TUI cleanup debt that still affects reliability and review quality

## Sprint Map

| Sprint | Focus | Primary Branch |
|---|---|---|
| AX.1 | Dev-install daemon ownership + restart reliability | `fix/dev-install-daemon-owner-835` |
| AX.2 | Inbox malformed-record tolerance + TUI daemon launch cleanup | `feature/pAX-s2-inbox-tui-fixes` |
| AX.3 | Test cleanup and duplicate GH type consolidation | `feature/pAX-s3-test-cleanup` |

## AX.1

**Scope**:

1. Shared `dev`/`prod` daemon launches must survive the caller session cleanly.
2. `scripts/dev-install` must confirm the restarted daemon is actually alive
   before reporting success.
3. The AX planning docs must record the correct root cause of issue `#793`.

**Correct root cause for `#793`**:

The failure was not `atm send` rewriting `config.json` and dropping members.
The actual bug lived in daemon reconcile:

- `crates/atm-daemon/src/daemon/event_loop.rs`
- reconcile logic previously called `config.members.retain(...)`
- that `retain()` path dropped sessionless members from the persisted roster

The AX.1 fix removes that `retain()` behavior so reconcile preserves configured
members while still cleaning dead session artifacts (inboxes, session-registry
rows, and related daemon-side state) for members that have actually terminated.

**Acceptance criteria**:

- `dev-install` exits non-zero if the restarted shared dev daemon is not alive
  within the bounded post-restart liveness window
- AX docs describe daemon reconcile, not `atm send`, as the root cause of `#793`

## AX.2

**Scope**:

1. Inbox readers skip malformed records instead of aborting the full inbox.
2. Legacy `content` is accepted as an alias for `text`.
3. Missing `read` defaults to `false`.
4. TUI daemon launch respects `ATM_DAEMON_BIN` / `ATM_HOME` scoped binaries and
   uses RAII cleanup for abnormal exit paths.

## AX.3

**Scope**:

1. Remove duplicate low-value GH CI alert test definitions.
2. Reap SIGTERM test children to prevent zombies during suite runs.
3. Deduplicate `GhRun` / `GhJob` / `GhStep` into one canonical shared definition.

## Exit Criteria

1. `dev-install` is a trustworthy dogfood entrypoint for the shared dev daemon.
2. Inbox/TUI flows tolerate malformed inbox history without breaking normal use.
3. AX test helpers no longer leave zombie children or duplicate GH schema paths.
