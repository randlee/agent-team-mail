# Logging Troubleshooting Runbook

Last updated: 2026-02-28

## Purpose

This runbook maps unified logging health states to concrete diagnostics and
remediation actions.

## Health States

### `healthy`

Meaning:
- Events are reaching canonical log sink.

Checks:
- `atm doctor --json`
- `atm status --json`

Expected:
- target schema: `logging.health_state = "healthy"`
- until schema field lands: infer healthy from absence of degraded/unavailable
  logging findings in `atm doctor` output.

Action:
- No remediation required.

### `degraded_spooling`

Meaning:
- Producer cannot reach daemon/sink and is writing spool files.

Checks:
- `atm doctor --json`
- Verify spool directory exists and grows: `${ATM_HOME:-$HOME}/.config/atm/log-spool`

Likely causes:
- daemon not running
- startup race
- socket/path mismatch

Remediation:
1. Start/restart daemon and re-check:
   - `atm daemon status`
   - `atm status --json`
2. Validate resolved paths/env:
   - compare reported log/spool/socket paths in diagnostics
3. Re-run doctor:
   - `atm doctor --json`
4. Confirm spool merge completed and health returns to `healthy`.

### `degraded_dropping`

Meaning:
- Events are being dropped (queue overflow or unrecoverable emit path failures).

Checks:
- `atm doctor --json` dropped counter
- recent warnings in logs

Likely causes:
- sustained event burst beyond queue capacity
- prolonged sink unavailability with pressure

Remediation:
1. Restore sink availability (daemon health + path consistency).
2. Reduce burst source or increase processing headroom (implementation/config change).
3. Re-check dropped counter progression (must stop increasing under normal load).

### `unavailable`

Meaning:
- No active sink and spool fallback not succeeding.

Checks:
- `atm doctor --json`
- filesystem permissions/path existence for canonical log and spool dirs

Likely causes:
- permission/path errors
- invalid environment path configuration
- daemon and fallback both failing

Remediation:
1. Verify write permissions for:
   - `${ATM_HOME:-$HOME}/.config/atm/`
2. Clear path/env mismatch and restart daemon.
3. Run:
   - `atm doctor --json`
4. If still unavailable, capture diagnostics and escalate.

## PID Logging Semantics

### INFO-level PID fields

INFO log lines for registration and liveness events include `agent_pid=<N>` where `<N>`
is the **registered agent session PID** — the long-lived process running the agent
(for example the `claude` or `codex` process). This is the PID stored in the daemon
session registry and shown in `atm doctor` output.

### Subprocess pid/ppid at DEBUG level

The subprocess PID of each hook invocation and the hook's parent PID (ppid) are logged
at DEBUG level only. These values change on every hook call and are not meaningful for
liveness tracking. They appear in WARN and DEBUG entries to assist root-cause analysis
when diagnosing hook setup or PID correlation problems.

To expose subprocess pid/ppid in output, set:

```bash
ATM_LOG=debug atm doctor
```

This enables the full structured fields including `hook_pid`, `hook_ppid`, and
`agent_pid` for each daemon lifecycle event.

### PID Mismatch Warnings

If `atm doctor` reports a `PID_PROCESS_MISMATCH` finding, the registered PID is alive
but the process running under that PID is not the expected agent backend (for example
the PID was reused by an unrelated process after the agent exited). Remediation:

1. Run `atm register <team> <name>` from the affected agent to refresh the PID.
2. If the agent is no longer running, run `atm cleanup --agent <name>` to remove the
   stale registration.
3. Re-run `atm doctor` to confirm the finding is resolved.

## Fast Triage Commands

```bash
atm doctor --json
atm status --json
atm logs --level warn
atm logs --level error
```

## Escalation Criteria

Escalate when any are true:
- `unavailable` persists after remediation steps.
- `degraded_dropping` continues under normal load.
- spool age grows and never reconverges after daemon recovery.
