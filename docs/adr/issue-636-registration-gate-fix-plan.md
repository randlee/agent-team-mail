# Issue #636 Fix Plan: Registration Gate vs PID Validation

Date: 2026-03-10
Status: planned (implementation pending on `fix/636-pid-ssot-registration-gate`)
Issue: https://github.com/randlee/agent-team-mail/issues/636

## Problem

`register-hint` currently uses `validate_pid_backend(...)` as a hard gate. When
the PID/backend pattern does not match, daemon returns `PID_PROCESS_MISMATCH`
before writing session state to the registry.

This breaks daemon SSoT behavior and leaves members in `Unknown` state.

## Root Cause

PID/backend validation is mixed into write-path authorization.

Expected architecture:
- registration writes are accepted when payload and ownership are valid
- PID/backend mismatch is diagnostic-only
- canonical member state is derived from daemon session registry

Current architecture (bug):
- registration write is blocked on backend process-name heuristics

## Required Code Changes

## 1) Remove blocking gate from `handle_register_hint`

File:
- `crates/atm-daemon/src/daemon/socket.rs`

Current behavior:
- calls `validate_pid_backend(&member, process_id)`
- returns `PID_PROCESS_MISMATCH` error on mismatch

Target behavior:
- never reject registration due to backend mismatch
- still emit mismatch diagnostics (`PID_PROCESS_MISMATCH`) as advisory
- continue to `upsert_runtime_for_team(...)` and state update

## 2) Remove/soften other write-path blockers

File:
- `crates/atm-daemon/src/daemon/socket.rs`

Call sites identified in scope:
- session-start flow (`registration` stage)
- bootstrap flow (`bootstrap` stage)

Target behavior:
- mismatch must not block session write paths
- mismatch logging remains for observability

## 3) Keep display-time validation, but informational-only

File:
- `crates/atm-daemon/src/daemon/socket.rs` (`derive_canonical_member_state`)

Target behavior:
- retain validation check and warning emission for mismatch visibility
- do not force member to `offline/unknown` solely due to backend mismatch
- member state must still derive from session registry liveness and tracker
  activity rules

## 4) Preserve ownership and payload guards

This fix does **not** relax:
- cross-identity ownership checks
- missing/invalid payload field validation
- session-id conflict handling

Only backend comm-pattern mismatch transitions from blocking to advisory.

## Test Plan Updates

## A) Registration mismatch regression tests

File:
- `crates/atm-daemon/src/daemon/socket.rs` tests

Add/adjust tests:
- `handle_register_hint` succeeds even when backend mismatch would previously
  fail
- response is success path (`registered`) and session registry entry exists
- advisory mismatch event is emitted/logged

## B) Canonical member-state behavior

Add/adjust tests:
- live registered session with backend mismatch is not rendered as `Unknown`
  solely due to mismatch
- mismatch remains visible as diagnostic annotation/event

## C) CLI-level integration guard

File:
- `crates/atm/tests` integration coverage

Add regression:
- `atm status` after register-hint no longer shows all members `Unknown` in
  mismatch scenario

## Acceptance Criteria

- `handle_register_hint` no longer returns `PID_PROCESS_MISMATCH` as a write gate
- session registry updates succeed under mismatch conditions
- mismatch diagnostics still logged (`PID_PROCESS_MISMATCH`) for triage
- canonical member state derives from daemon registry SSoT, not backend gate
- targeted tests pass and prevent regression

## Risk and Mitigation

Risk:
- accepting mismatched PID/backend could register stale or wrong process hints

Mitigation:
- keep ownership guard intact
- keep PID liveness checks in canonical state derivation
- keep mismatch diagnostics and doctor findings for operator action
- do not treat backend mismatch as success signal; treat as advisory anomaly
