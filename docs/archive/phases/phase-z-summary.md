# Phase Z Summary

## Scope
Phase Z hardened daemon single-source-of-truth behavior and observability across
`atm`, `atm-daemon`, and `atm-core`.

## Completed Sprints

### Z.1 Quick Wins
- Prevented false PID/backend mismatch findings when process lookup is inconclusive.
- Improved human-readable session-id formatting in doctor/member/status views.
- Stopped reconcile from re-overwriting mismatch-offline states.
- Reworked release verification path to avoid fragile external curl checks.

### Z.2 Log Format + Doctor UX
- Normalized send log identity formatting and sender/recipient fields.
- Enforced `ATM_LOG_MSG=1` as the only message-preview enablement path.
- Improved doctor log-window display semantics.

### Z.3 SSoT Fast Path
- Added daemon `register-hint` command path for runtime session registration.
- Removed send-path liveness shortcuts in favor of daemon session truth.
- Ensured spawn metadata persistence aligns with daemon-backed launch paths.

### Z.4 Canonical Member State Completion
- Completed team-scoped canonical member-state union (config + daemon-only sessions).
- Added daemon-only ghost/unregistered surfacing in status/member/doctor views.
- Aligned PID/backend mismatch handling across command paths.

### Z.5 Lifecycle Logging + Hook Events
- Added structured lifecycle transition logging events.
- Added first-class hook lifecycle events (`session_start`, compact, `session_end`, failures).
- Aligned Unix-scoped lifecycle assertions in plan/docs.

### Z.6 Cross-Folder Spawn + QA Blocker Closure
- Added cross-folder spawn behavior and launch command preview fidelity.
- Added mismatch policy for `ATM_TEAM` vs `.atm.toml` with `--override-team` contract.
- Closed QA blockers around register-hint compatibility and ownership guard coverage.

### Z.7 Review Findings Hardening (1-7 merged)
- Removed duplicate spawn metadata write before launch.
- Added daemon-only PID mismatch diagnostics and parsed mismatch details.
- Added typed backend mismatch/recovery tests for register-hint/session state.
- Shared ghost/unregistered display constants across doctor/members/status.
- Added `ATM_LOG_MSG=""` disable-preview coverage.
- Added session-start env fallback support for no-`.atm.toml` contexts.

## Follow-Up (Z.7 deliverables 8-12)
- Regression coverage for `--override-team` mismatch handling (fail/pass paths).
- Ownership guard fail-closed path coverage when daemon/session sync is unavailable.
- Explicit spawn-path env precedence test (`ATM_TEAM` over `.atm.toml` default team).
- Plan updates reflecting Z.1-Z.6 completion and Z.7 review hardening progression.
