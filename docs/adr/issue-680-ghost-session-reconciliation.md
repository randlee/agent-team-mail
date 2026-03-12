---
issue: 680
title: "GHOST_SESSION False-Positive: isActive hint must not corroborate daemon liveness"
date: 2026-03-12
worktree: fix/issue-680-ghost-session-reconciliation
status: ready-to-implement
---

# Issue #680: GHOST_SESSION Reconciliation — Root Cause & Fix Blueprint

## Root Cause

`atm doctor` emits `GHOST_SESSION` (Warn) when the daemon reports a member as `active`/`idle`
but `config.json` has `isActive != true`. This is a **semantic conflation bug**: `isActive` is
an advisory activity hint (set by `atm send` heartbeats), not a liveness signal. A member can
be daemon-alive but have `isActive=null` after a restore/recreate cycle or simply having no
recent activity heartbeat.

**The daemon session registry is the sole liveness authority. `isActive` must never be
cross-checked against liveness.**

## Two-Layer State Model

| Field | Location | Meaning | Written by |
|-------|----------|---------|-----------|
| `isActive` | `config.json` member entry | Activity hint (recently busy?) | `atm send`, hooks, daemon timeout reconciler |
| session registry | daemon in-memory | Liveness truth (PID alive?) | daemon socket handler |

## Failure Scenario

1. Member has a live daemon session → daemon correctly reports `state: "active"`.
2. Member's `config.json` has `isActive: null` (no recent heartbeat, or after restore).
3. `doctor.rs:696-708` interprets mismatch as a ghost session — **incorrect**.

## Files to Change

### `crates/atm/src/commands/doctor.rs`

**Lines 696–708** — delete this match arm entirely:
```rust
Some("active") | Some("idle") if member.is_active != Some(true) => {
    findings.push(finding(Severity::Warn, "pid_session_reconciliation", "GHOST_SESSION", ...))
}
```
A daemon reporting `active`/`idle` for a config member is the normal, healthy state.
`isActive` being `false`/`null` simply means no recent activity heartbeat — not a ghost.

**Line 1144** — remove `has("GHOST_SESSION")` from the recommendation trigger condition
(or retain if GHOST_SESSION is repurposed for the narrower daemon-only-orphan case below).

### `crates/atm/src/commands/doctor.rs` (tests)

Rename `check_pid_session_reconciliation_detects_ghost_session_for_inactive_hint`:
- Change assertion from "GHOST_SESSION fires" → `findings.is_empty()`
- Add variant: `is_active: Some(false)` → no finding
- Add variant: `is_active: Some(true)` + daemon `"active"` → no finding (happy path)

## Optional: Narrow GHOST_SESSION for Daemon-Only Orphans

True ghost sessions are daemon entries where `state.in_config == false` — the daemon tracks
a session with no `config.json` backing. This is separate from the current bug and can be
deferred as a follow-up. If added, the check should use `!state.in_config` not `isActive`.

## Implementation Checklist

- [ ] Delete GHOST_SESSION match arm at `doctor.rs:696–708`
- [ ] Update test: rename + assert `findings.is_empty()`
- [ ] Add `is_active: Some(false)` variant test
- [ ] Add happy-path variant test
- [ ] Remove `has("GHOST_SESSION")` from recommendation trigger (line 1144)
- [ ] `cargo test -p agent-team-mail -- doctor` — all pass
- [ ] `cargo test -p agent-team-mail --test '*' -- pid_session_reconciliation` — all pass
- [ ] `cargo clippy --workspace` — clean

## Out of Scope

`event_loop.rs:616–635` seeds daemon `state_store` from `isActive` (issue #330). That is a
separate conflation tracked independently and does not affect this fix — `derive_canonical_member_state`
prioritizes session registry liveness over tracker state when a session record exists.
