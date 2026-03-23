# Daemon Spawn Authorization Requirements

**Status**: Active
**Supersedes**: `docs/phase-au-daemon-spawn-authorization.md`

## Scope

This document defines the minimal post-BB daemon launch contract:

- one canonical shared daemon per user
- one canonical runtime root
- one product-owned launch-token surface
- one manual smoke gate for dogfood validation

dev-shared launch class is obsolete and no longer part of the product contract.

IsolatedTest is retained as a test-infrastructure-only launch class. It is not a
product-facing surface and MUST NOT be used outside of test harness contexts. Test
daemons started with IsolatedTest tokens are owned and reaped by their fixture; they
do not participate in the single-shared-daemon invariant.

## Ownership Boundary

- `crates/atm-daemon-launch` is the only allowed owner for daemon launch-token
  issuance.
- `atm-core`, `atm`, and plugin crates may consume launch tokens, but they MUST
  NOT define or issue competing launch-token schemas.

## Single-Daemon Invariant

- `atm-daemon` MUST reject startup without a valid launch token issued by the
  canonical launcher.
- Missing, invalid, expired, replayed, or mismatched tokens MUST cause
  immediate exit with structured rejection logs.
- `atm-daemon` MUST only start against the canonical shared runtime root for
  the current user.
- Shared startup MUST hard-fail duplicate starts while the canonical daemon is
  live.
- There is one approved daemon binary selection policy for the shared daemon.
  Product code MUST NOT loop across competing binary candidates or runtime-home
  ownership claims.

## Token Contract

`DaemonLaunchToken` remains the canonical cross-process launch contract.

Required fields:
- `launch_class`
  - product-facing enum: `shared`
  - test-infrastructure enum: `isolated-test` (test harness only; not a product surface)
- `atm_home`
- `binary_identity`
- `issuer`
- `token_id`
- `issued_at`
- `expires_at`

Compatibility note:
- older serialized values may still decode during migration work, but only the
  shared launch path is part of the normative product model after BB.

## Lifecycle Logging

The daemon MUST log:
- `launch_accepted`
- `daemon_start_rejected`
- `clean_owner_shutdown`

These events remain the primary evidence surface for daemon launch QA.

Serialization note:
- lifecycle events are stored as `LogEventV1` JSONL records
- the canonical event name is the top-level `action`
- `fields.event_name` may mirror the same value for consumers that still read
  structured field maps
