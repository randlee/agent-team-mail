# Phase AU — Daemon Spawn Authorization

**Integration branch**: `integrate/phase-AU` off `develop`
**Prerequisites**: Phase AT merged to `develop` AND `docs/arch-boundary.md`
records the zero-violation audit state
**Status**: IN PROGRESS

## Overview

Phase AU makes daemon spawning impossible outside the authorized launcher path.
The daemon itself becomes the enforcement point: launches must present a valid
token, the daemon rejects missing or invalid launches, and test daemons become
lease-scoped processes with explicit owner and TTL metadata.

This phase is intentionally harsh:
- one canonical daemon launcher
- one startup firewall in `atm-daemon`
- one isolated-test lease model
- QA/CI failure on any bypass or forgotten daemon

The TTL model for tests is a fail-safe, not a substitute for proper fixture
teardown. Clean shutdown by the owning test fixture remains mandatory.

## Goals

1. Make non-canonical daemon launches fail immediately at daemon startup.
2. Distinguish `prod-shared`, `dev-shared`, and `isolated-test` launch classes.
3. Give isolated test daemons explicit ownership via `test_identifier` and
   `owner_pid`, plus bounded lifetime via TTL.
4. Emit lifecycle logs detailed enough for `daemon-spawn-qa` to explain any
   forgotten daemon without guesswork.
5. Make CI/QA block on bypasses, rogue daemons, and TTL/dead-owner terminations.

---

## Sprint Plan

### AU.1 — Canonical Launcher + Token Issuance

**Status**: COMPLETE
Implementation note: canonical daemon launch now routes through
`agent-team-mail-daemon-launch`, which owns the `DaemonLaunchToken` 7-field
struct (`launch_class`, `atm_home`, `binary_identity`, `issuer`, `token_id`,
`issued_at`, `expires_at`), the `LaunchClass` enum (`ProdShared |
DevShared | IsolatedTest`), the `issue_launch_token()` issuance surface in
`crates/atm-daemon-launch`, and the temporary `AU-BYPASS` annotations at
`crates/atm-core/src/daemon_client.rs:2131` and
`crates/atm-tui/src/main.rs:497` that track the remaining AU.5 reroutes.

**Scope**: define the only allowed daemon launcher and its token model.

**Deliverables**:
- one canonical launcher API for all daemon starts/adoptions
  The owning implementation lives in a dedicated product-layer launcher crate
  (planned target: `crates/atm-daemon-launch`). If the crate split is
  deliberately deferred, the only acceptable temporary owner is
  `agent_team_mail_daemon::spawn_auth`. `atm-core` and `atm-ci-monitor` must
  not own launcher code.
- token issuance surface owned by the canonical launcher
- token schema covering:
  - launch class
  - target `ATM_HOME`
  - binary/channel identity
  - issuer
  - nonce / token id
  - issue time
  - expiry
- removal/deprecation plan for all known bypass helpers

**Acceptance Criteria**:
- review can point to one launcher implementation as the only valid path, and
  that implementation lives in the dedicated launcher crate (or explicitly
  approved temporary `agent_team_mail_daemon::spawn_auth` fallback), with zero
  launcher ownership in `atm-core` and zero launcher ownership in
  `atm-ci-monitor`
- token schema is documented and stable enough for daemon-side validation
- every known daemon launch site is mapped to migrate to the canonical launcher

### AU.2 — Daemon Startup Firewall

**Status**: COMPLETE
Implementation note: `atm-daemon` now validates launch tokens at startup,
rejects missing/invalid/replayed/wrong-class launches, emits structured
rejection records, and hard-fails duplicate shared-runtime starts.

**Scope**: make `atm-daemon` reject unauthorized startup.

**Deliverables**:
- daemon startup validation of launch token presence and integrity
- hard rejection on:
  - missing token
  - invalid token
  - expired token
  - wrong launch class / wrong `ATM_HOME`
  - replayed token
- structured startup rejection logging
- shared-runtime singleton checks integrated with launch-class validation

**Implementation Note**:
- implemented `validate_startup_token()` with 6 rejection conditions:
  missing, invalid, expired, wrong-atm-home, wrong-class, and replayed
- implemented `emit_startup_rejection()` with the 5-field structured event
  contract
- implemented `SharedRuntimeAlreadyRunning` rejection for `ProdShared` and
  `DevShared`

**Acceptance Criteria**:
- raw `atm-daemon` execution outside the authorized launcher fails immediately
- `prod-shared` and `dev-shared` second launches hard-fail
- rejection records are queryable and attributable in logs

### AU.3 — Isolated-Test Lease + Clean Shutdown Contract

**Status**: COMPLETE
Implementation note: isolated-test launch tokens now carry `test_identifier` +
`owner_pid`, runtime metadata persists the lease, the daemon self-terminates on
TTL/dead-owner fail-safe conditions, and startup janitoring reaps stale
isolated runtimes only after lease expiry plus dead-owner confirmation.

**Scope**: give test daemons a real lease model and make clean fixture teardown
the normative success path.

**Deliverables**:
- `isolated-test` launch class
- required lease fields:
  - `test_identifier`
  - `owner_pid`
  - `issued_at`
  - `expires_at`
  - `atm_home`
  - token id / nonce
- daemon self-termination on dead `owner_pid` or TTL expiry
- janitor/sweep behavior for stale isolated runtimes
- explicit requirements/test-plan updates stating that fixture-owned clean
  shutdown is mandatory and TTL expiry is only a fail-safe

**Acceptance Criteria**:
- isolated test daemons cannot outlive both their owner and TTL
- every isolated test daemon is attributable to a specific test identifier
- TTL/dead-owner exits are distinguishable from clean fixture teardown

### AU.4 — Lifecycle Logging + QA Enforcement

**Scope**: make launch/termination reasons observable and actionable.

**Deliverables**:
- structured lifecycle log events for:
  - launch accepted
  - launch rejected
  - clean owner shutdown
  - TTL expiry shutdown
  - dead-owner shutdown
  - janitor/stale-runtime reap
- `daemon-spawn-qa` update to consume lifecycle logs for root cause analysis
- QA rule: TTL/dead-owner shutdown in tests is a blocking harness-gap finding
- CI/QA preflight/postflight rogue-daemon checks bound to launch metadata

**Acceptance Criteria**:
- `daemon-spawn-qa` can explain forgotten daemons using logged facts
- CI/QA fails when a daemon lacks canonical launch metadata
- CI/QA fails when a test daemon ends by TTL/dead-owner instead of clean teardown

### AU.5 — Bypass Removal + Final Audit

**Scope**: remove remaining non-canonical daemon launch paths and prove the
authorization model is the only one left.

**Deliverables**:
- remove or reroute all known bypasses, including:
  - `daemon_client` fire-and-forget spawn path
  - TUI private `ensure_daemon_running` path
  - helper/script launches that do not delegate to the canonical launcher
- repository-wide audit of daemon spawn sites
- CI grep/check that blocks new bypasses

**Acceptance Criteria**:
- no remaining daemon spawn path bypasses the canonical launcher
- repository audit finds zero untracked daemon launches
- QA can run a rogue-daemon sweep and attribute every surviving daemon to a
  valid active lease

---

## Dependency Graph

- `AU.1` starts first.
- `AU.2` depends on `AU.1`.
- `AU.3` depends on `AU.1` and the launch-class validation shape from `AU.2`.
- `AU.4` depends on `AU.2` for rejection/acceptance events and on `AU.3` for
  test-daemon lease fields.
- `AU.5` depends on `AU.2` through `AU.4`; it is the final removal/audit sprint.

---

## Inputs and References

- DSQ-001 through DSQ-009 daemon leak findings
- incident: 449 leaked `atm-daemon` processes during Phase AT session
- `crates/atm-core/src/daemon_client.rs`
- `crates/atm-tui/src/main.rs`
- existing test harness rules from Phase AQ / AP

---

## Exit Criteria

Phase AU is complete when:
- the daemon rejects unauthorized startup in all environments
- shared runtimes cannot double-start
- test daemons carry `test_identifier`, `owner_pid`, and TTL lease metadata
- test fixture teardown is the normal shutdown path and TTL/dead-owner exits are
  treated as blocking QA gaps
- `daemon-spawn-qa` can root-cause forgotten daemons from lifecycle logs
- CI/QA blocks any new non-canonical daemon spawn path
