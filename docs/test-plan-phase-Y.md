# Phase Y Test Plan — Doctor Source-of-Truth + State Model Cleanup

**Goal**: Verify doctor and status surfaces use daemon-authoritative state, eliminate `isActive` liveness facades, and lock env-var behavior to explicit defaults.

## 1. Scope

Phase Y validates four workstreams:
- **Y.1** Canonical state model + source-of-truth enforcement
- **Y.2** Doctor snapshot/status contract hardening
- **Y.3** Team-scoped reconciliation + daemon-unreachable semantics
- **Y.4** Environment variable discipline and defaults

## 2. Entry Criteria

- `docs/requirements.md` contains Phase Y state-model and doctor-output contracts.
- `docs/project-plan.md` contains Phase Y sprint mapping (Y.1-Y.4).
- Team fixtures available for multi-team validation (`atm-dev`, `annotations-test`).

## 3. Test Matrix

| Sprint | Focus | Core Assertions |
|-------|-------|-----------------|
| Y.1 | Source-of-truth | Doctor derives liveness from daemon/session registry, never from `isActive`. |
| Y.2 | Output contract | Doctor human output starts with member table and required columns; status taxonomy is stable. |
| Y.3 | Scoping + degraded mode | Doctor is team-scoped and non-failing; daemon-unreachable path reports `Unknown` states plus actionable finding. |
| Y.4 | Env defaults/discipline | `ATM_LOG` and `ATM_LOG_MSG` defaults hold when unset; daemon override vars are optional test/ops knobs and visible in diagnostics. |

## 4. Detailed Cases

### Y.1 — Canonical state model

1. **No `isActive` liveness fallback**
- Setup: member has `isActive=false`, daemon session is alive.
- Expect: doctor/status show member live (`Active`/`Idle` based on daemon), not dead/offline.

2. **Dead session classification**
- Setup: daemon registry marks member dead and PID not alive.
- Expect: doctor `Status=Dead` regardless of `isActive=true` residue in `config.json`.

3. **State inventory consistency**
- Validate every state variable in requirements inventory has owner, persistence location, and allowed values.

4. **Canonical daemon-struct consumption**
- Setup: capture daemon `list-agents`/member-state payload and doctor/status/members outputs.
- Expect: semantic parity for `status`, `activity`, `pid`, `session_id` with label-only formatting differences.
- Reject any code path where doctor/status/members recompute liveness independently.

### Y.2 — Snapshot/status contract

1. **Snapshot-first ordering**
- Run: `atm doctor --team atm-dev`
- Expect: member snapshot appears before findings.

2. **Required columns present**
- Expect columns include: `Name`, `Agent ID`, `Type`, `Model`, `PID`, `Session ID`, `Status`, `Activity`.

3. **Status taxonomy enforcement**
- Expect status values constrained to `Active|Idle|Dead|Unknown` only.

4. **Activity semantics enforcement**
- `isActive=true` corresponds to `Activity=Busy` (not liveness).
- `isActive=false` corresponds to `Activity=Idle` unless daemon activity is unknown.

### Y.3 — Team scope + non-failing behavior

1. **Team-scope isolation**
- Setup: daemon tracks members in two teams.
- Run: `atm doctor --team atm-dev`
- Expect: no findings/member rows leaked from other teams.

2. **Daemon unreachable non-failing report**
- Setup: stop daemon.
- Run: `atm doctor --team atm-dev`
- Expect: report still emitted, daemon-unreachable finding included, member liveness rendered `Unknown`.

3. **Severity-based exit behavior preserved**
- Expect: doctor returns `0` or `2` when report is produced; `1` reserved for report-generation failure.

4. **PID reuse safety**
- Setup: simulate registry record for a dead session, then spawn an unrelated process
  that reuses the same PID with different process-identity metadata.
- Expect: daemon/doctor classify original session as `Dead`; no false live revival.
- Expect: explicit diagnostic finding for PID-reuse/session-invalidated path.

### Y.4 — Environment variables and defaults

1. **`ATM_LOG` default**
- Setup: unset `ATM_LOG`.
- Expect: effective logging verbosity defaults to `info`.

2. **`ATM_LOG_MSG` default**
- Setup: unset/invalid `ATM_LOG_MSG`.
- Expect: persisted message text policy defaults to `truncated`.

3. **Daemon override vars optional**
- Setup: unset `ATM_DAEMON_BIN` and `ATM_DAEMON_AUTOSTART`.
- Expect: normal daemon-backed commands still work (autostart enabled by default).

4. **Override visibility in diagnostics**
- Setup: set non-default daemon override env var.
- Expect: doctor output/JSON includes contextual indication of active override.

## 5. Recommended Commands

```bash
# Doctor/status focused tests
cargo test -p agent-team-mail doctor -- --nocapture
cargo test -p agent-team-mail status -- --nocapture

# Daemon/session registry behavior
cargo test -p agent-team-mail-daemon session_registry -- --nocapture
cargo test -p agent-team-mail-daemon roster_tests -- --nocapture

# Logging defaults/policy
cargo test -p agent-team-mail-core event_log -- --nocapture
cargo test -p agent-team-mail-daemon log_writer_config -- --nocapture
```

## 6. Exit Criteria

- All Y.1-Y.4 assertions mapped to tests or explicitly queued follow-up issues.
- No remaining doctor paths that infer liveness from `isActive`.
- Doctor non-failing contract validated with daemon-down scenario.
- Env-var defaults and scope discipline validated and documented.
