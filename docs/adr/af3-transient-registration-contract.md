# ADR: AF.3 Transient Registration Contract

- Status: Accepted
- Date: 2026-03-08
- Sprint: AF.3
- Related Issues: #393

## Context

Task-tool and one-off helper agents can emit lifecycle-adjacent signals without being
persistent ATM roster members. Prior behavior risked mutating persistent state from
non-member events, causing roster/session drift and false doctor/status output.

## Decision

The daemon and CLI enforce a strict transient-registration contract:

1. Non-member lifecycle signals must not create persistent roster or session state.
2. `session_start` hook events from non-members are rejected with `processed=false`
   and reason `agent not in team`.
3. Team-member lifecycle events continue to register and transition state normally.
4. Non-member `send/read` paths remain messaging-fail-open but must not mutate roster.

## Consequences

- Persistent team state remains owned by explicit roster membership.
- Transient task-tool activity can coexist without polluting team config/session tables.
- Doctor/status/members outputs remain deterministic and trustable.

## Verification

- Daemon unit test: `test_hook_event_session_start_rejects_non_member`
- Daemon/unit branch logic tests in hook watcher for member vs non-member handling
- CLI integration tests in `integration_transient_registration.rs` for send/read/spawn
  no-mutation invariants

