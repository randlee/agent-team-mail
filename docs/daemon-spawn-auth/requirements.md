# Daemon Spawn Authorization Requirements

**Status**: Active
**Supersedes**: `docs/phase-au-daemon-spawn-authorization.md`

## Scope

This document defines the minimal post-BB daemon launch contract:

- one canonical shared daemon per user
- one canonical runtime root
- one product-owned launch-token surface
- one manual smoke gate for dogfood validation

Legacy isolated-test lease behavior and dev-shared launch classes are obsolete
and are no longer part of the product contract.

## Ownership Boundary

- `crates/atm-daemon-launch` is the only allowed owner for daemon launch-token
  issuance.
- `atm-core`, `atm`, and plugin crates may consume launch tokens, but they MUST
  NOT define or issue competing launch-token schemas.

## Single-Daemon Invariant

- `atm-daemon` MUST only start against the canonical shared runtime root for the
  current user.
- Startup MUST reject:
  - missing launch tokens
  - invalid, expired, replayed, or mismatched tokens
  - attempts to start a second shared daemon while the canonical daemon is live
- There is one approved daemon binary selection policy for the shared daemon.
  Product code MUST NOT loop across competing binary candidates or runtime-home
  ownership claims.

## Token Contract

`DaemonLaunchToken` remains the canonical cross-process launch contract.

Required fields:
- `launch_class`
- `atm_home`
- `binary_identity`
- `issuer`
- `token_id`
- `issued_at`
- `expires_at`

Supported launch classes:
- `shared`

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
  structured fields
- the emission timestamp is always top-level `ts`

## Dogfood Gate

The canonical manual daemon smoke protocol is Phase AR:
- `docs/project-plan.md` section `17.26`
- `scripts/dev-daemon-smoke.py`

Any daemon reset or release-readiness change MUST use that protocol as the
manual dogfood gate before merge or release.

## CI / Boundary Contract

- Any non-canonical daemon spawn path is a blocking violation.
- `scripts/ci/gh_boundary_check.sh` remains the required CI enforcement surface
  for daemon-launch boundary regressions.

## Non-Goals

- multi-daemon ownership arbitration
- per-test isolated daemon runtime support as a product behavior
- daemon launch-token fields that embed GitHub/provider-specific payloads
