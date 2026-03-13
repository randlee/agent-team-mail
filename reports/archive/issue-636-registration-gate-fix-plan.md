# Issue #636 Fix Plan: PID/Backend Mismatch as Diagnostic, Not Registration Gate

Date: 2026-03-10  
Status: planned (documentation revision for implementation kickoff)  
Issue: https://github.com/randlee/agent-team-mail/issues/636  
Branch: `fix/636-pid-ssot-registration-gate`

## 1. Problem Statement

Daemon registration/update paths currently use PID/backend mismatch checks as
hard gates in multiple places. That blocks or short-circuits session upserts,
which leaves daemon registry state incomplete and causes members to surface as
`Unknown`/offline despite valid activity.

Design intent remains:
- daemon session registry is SSoT for runtime state
- PID/backend mismatch is a diagnostic signal
- mismatch must not block registration writes

## 2. Write-Path Gates and Required Call-Site Fixes

This section enumerates the exact call sites identified by ATM-QA findings.

### 2.1 `socket.rs:1527` (processed=false early-ok path)

Current behavior:
- returns `make_ok_response(processed=false)` early in a mismatch branch
- write path silently exits before intended upsert/reconcile work

Required fix:
- remove early-ok-return behavior in mismatch branch
- continue through normal registration/upsert flow
- preserve mismatch diagnostics as WARN logging/finding output

### 2.2 `socket.rs:5264-5278` (`handle_register_hint`)

Current behavior:
- returns `PID_PROCESS_MISMATCH` error on backend mismatch
- blocks write path

Required fix:
- remove mismatch gate from `handle_register_hint`
- do not return mismatch as command error
- continue to `upsert_runtime_for_team(...)`
- keep WARN diagnostic emission

### 2.3 `socket.rs:5598-5601` (bootstrap registration path)

Current behavior:
- mismatch path returns early before `upsert_runtime_for_team(...)`
- bootstrap silently fails to register stale-but-recoverable members

Required fix:
- remove mismatch-driven early return
- allow bootstrap upsert when ownership/payload checks pass
- keep mismatch diagnostics (WARN + finding context)

### 2.4 `socket.rs:5648` and `socket.rs:5809` (`derive_canonical_member_state`)

Current behavior:
- mismatch influences display-time state derivation as an effective gate

Required fix:
- treat these as display/diagnostic-only call sites
- mismatch may emit `PID_PROCESS_MISMATCH` but must not force offline/unknown
  by itself
- state derivation must continue via normal liveness/activity logic

## 3. Replacement State-Derivation Contract (`derive_canonical_member_state`)

Applies to line ranges:
- `socket.rs:5648-5668`
- `socket.rs:5809-5828`

Required behavior:
1. Evaluate PID liveness (`is_pid_alive`) and existing session/activity fields.
2. Evaluate backend mismatch (`validate_pid_backend`) for diagnostics.
3. If mismatch is detected, emit `PID_PROCESS_MISMATCH` (WARN/doctor context).
4. Fall through to normal liveness-based derivation:
   - if session is alive, result must be non-offline (`Active` or `Idle`
     depending on activity metadata)
   - if session is not alive, derive offline/dead as normal
5. PID/backend mismatch alone must never force offline/unknown status.

## 4. Requirements Updates (Documentation SSoT)

`docs/requirements.md` updates required in this task:
- §4.3.3d PID Registration Verification now states mismatch is advisory/diagnostic,
  not a registration rejection gate.
- Bootstrap guidance now states mismatch does not block bootstrap registration.
- Reconciliation guidance now states mismatch emits diagnostics then falls through
  to normal liveness/activity derivation.

## 5. Test Plan Updates (No Deletion, Convert Existing Coverage)

### 5.1 Existing tests to convert (explicit names)

In `crates/atm-daemon/src/daemon/socket.rs` tests:
- `test_handle_register_hint_rejects_codex_backend_pid_mismatch_with_warn_log`
- `test_handle_register_hint_rejects_claude_backend_pid_mismatch_with_warn_log`

Convert expectations to:
- status/result is success (registration proceeds)
- WARN log/finding for mismatch is still asserted
- registry upsert side-effect is asserted

### 5.2 Missing edge cases to add

1. Stale-session re-registration under mismatch:
   - given stale session data and mismatch
   - `upsert_runtime_for_team` still succeeds
   - mismatch diagnostic remains visible

2. Cross-identity attempt with mismatch:
   - after removing PID mismatch gate
   - cross-identity write must still be blocked
   - error remains `PERMISSION_DENIED`

## 6. Acceptance Criteria

- ATM-QA blocking findings ATM-QA-001 through ATM-QA-004 are resolved.
- `docs/requirements.md` §4.3.3d reflects advisory mismatch behavior.
- `docs/issues.md` includes issue #636 with in-flight branch/ADR tracking.
- Section 2 documents all four call sites and distinct fixes.
- Section 3 defines concrete replacement derivation behavior.
- Acceptance test criterion exists: `derive_canonical_member_state` returns
  non-offline state when `session.is_alive=true`, even with mismatch.
- Existing mismatch tests are converted (not deleted) and retain WARN assertions.
- Task remains documentation-only (no code changes in this ADR revision task).

## 7. Risks and Mitigations

Risk A: accepting mismatch writes may preserve stale/wrong PID hints.
- Mitigation: keep mismatch diagnostics explicit and visible.
- Mitigation: preserve ownership and payload validation gates.

Risk B: PID reuse during bootstrap may attach a session hint to an unrelated
process at the same PID.
- Mitigation: bootstrap still records mismatch diagnostics.
- Mitigation: state derivation remains liveness + activity driven; mismatch is
  surfaced for operator action and follow-up registration correction.
