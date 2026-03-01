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
