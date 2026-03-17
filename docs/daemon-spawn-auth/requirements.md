# Daemon Spawn Authorization Requirements

**Status**: Draft
**Scope**: normative subsystem requirements for daemon launch authorization,
launch-token validation, isolated-test leases, and lifecycle logging.

## Ownership Boundary

- The canonical daemon launcher is product-layer owned.
- `atm-core` and `atm-ci-monitor` MUST NOT own daemon lifecycle, launch-token
  issuance, or daemon launch validation.
- Planned ownership target: a dedicated launcher crate (for example
  `crates/atm-daemon-launch`).
- If that crate split is intentionally deferred, the only acceptable temporary
  owner is a thin `agent_team_mail_daemon::spawn_auth` module.

## Mandatory Launch Firewall

- `atm-daemon` MUST reject startup without a valid launch token issued by the
  canonical launcher.
- Missing, invalid, expired, replayed, or mismatched tokens MUST cause
  immediate exit with structured rejection logs.
- Shared runtimes (`prod-shared`, `dev-shared`) MUST hard-fail duplicate starts.

## Launch Classes

- `prod-shared`
- `dev-shared`
- `isolated-test`

Each launch class MUST bind:
- target `ATM_HOME`
- binary/channel identity
- runtime kind
- singleton / lease policy
- issue time
- expiry
- nonce / token id

## Token Schema

`DaemonLaunchToken` is the canonical cross-process launch contract. It is
serialized with `serde` and currently represented as JSON-safe data.

- `launch_class`
  - enum: `prod-shared`, `dev-shared`, `isolated-test`
  - selects singleton policy, lease rules, and startup validation behavior
- `atm_home`
  - target `ATM_HOME` bound to this launch
  - daemon startup MUST reject tokens whose bound runtime does not match the
    requested runtime
- `binary_identity`
  - binary path or release channel identifier used to explain which launcher
    surface issued the token
- `issuer`
  - product-owned launcher identity issuing the token
  - used for auditability and rejection diagnostics
- `token_id`
  - nonce / UUID for replay detection and event correlation
- `issued_at`
  - RFC3339 UTC timestamp when the token was created
- `expires_at`
  - RFC3339 UTC timestamp after which startup MUST be rejected

No other crate may define or issue a competing launch token schema.

## Bypass Annotation Convention

- `AU-BYPASS` is the normative comment token for temporary daemon-launch bypass
  sites that have not yet been migrated into `atm-daemon-launch`.
- Required format:
  - `// AU-BYPASS: migrate <description> to atm-daemon-launch in AU.5`
- Complete bypass inventory for the current AU plan:
  - `crates/atm-core/src/daemon_client.rs:2102`
  - `crates/atm-tui/src/main.rs:493`
- Any additional bypass sites found during the AU.5 final audit MUST be added
  to this inventory before that sprint is considered complete.

## Isolated-Test Lease

- Every isolated test daemon MUST carry:
  - `test_identifier`
  - `owner_pid`
  - `issued_at`
  - `expires_at`
  - `atm_home`
  - token id / nonce
- TTL expiry and dead-owner shutdown are fail-safe conditions only.
- The owning fixture remains responsible for clean shutdown before TTL expiry or
  owner loss.

## Lifecycle Logging

- The system MUST log:
  - launch accepted
  - launch rejected
  - clean owner shutdown
  - TTL-expiry shutdown
  - dead-owner shutdown
  - janitor/stale-runtime reap
- These logs are the primary evidence source for `daemon-spawn-qa`.

## QA / CI Contract

- Any non-canonical daemon spawn path is a blocking violation.
- Any rogue daemon without canonical launch metadata is a blocking violation.
- Any test daemon whose termination reason is TTL expiry or dead `owner_pid`
  instead of clean fixture shutdown is a blocking harness-gap finding.

## Non-Goals

- Launch-token fields MUST NOT embed GitHub-specific metadata, runner context,
  or CI provider payload.
