# Logging Troubleshooting Runbook

Last updated: 2026-03-10

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

## OpenTelemetry Exporter Diagnostics

### Fail-open contract

OTel export is best-effort and must never block canonical logging writes.

Expected behavior during exporter outages:
- canonical log writes continue (`atm.log`)
- command flow remains non-failing (`atm doctor`, `atm status`)
- OTel health reports degraded/unavailable until exporter path recovers

### OTel sidecar output path

By default, OTel sidecar output is derived from canonical log path:
- canonical: `.../atm.log`
- sidecar: `.../atm.log.otel.jsonl`

If sidecar path is unreachable (permissions, path conflict, directory at file path),
OTel health degrades but canonical logging remains available.

### Environment toggles

`ATM_OTEL_ENABLED` controls exporter enablement:
- unset or truthy value: exporter enabled
- `false|0|off|disabled|no`: exporter disabled by design

When disabled:
- `otel_status`/`logging_health.otel_exporter` should report `unavailable`
- `otel_last_error`/`last_export_error` should explain exporter was disabled by
  `ATM_OTEL_ENABLED`

### Operator checks

```bash
atm doctor
atm doctor --json
atm status
atm status --json
```

Look for:
- `otel_status` in text output
- `logging_health.otel_exporter` in JSON output
- `otel_last_error`/`last_export_error` when exporter is disabled or degraded

### Remediation

1. Verify exporter is intentionally enabled:
   - `unset ATM_OTEL_ENABLED` or set to a truthy value.
2. Check sidecar path availability and permissions:
   - same parent as canonical log path.
3. Re-run `atm doctor --json` and confirm `logging_health.otel_exporter`.
4. If exporter stays degraded while canonical logging is healthy, capture
   diagnostics and escalate as exporter-path-specific incident.

## PID Logging Semantics

### INFO-level PID fields

INFO log lines for registration and liveness events include `agent_pid=<N>` where `<N>`
is the **registered agent session PID** — the long-lived process running the agent
(for example the `claude` or `codex` process). This is the PID stored in the daemon
session registry and shown in `atm doctor` output.

For human-readable `atm logs` output on `send` events:
- the line shows only sender/recipient session PID slots
  (`send <from>@<team> [<sender_pid>] -> <to>@<team> [<recipient_pid>]`).
- emitter process `pid/ppid` are intentionally omitted from the `send` line to
  avoid mixed PID semantics in one view.

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

To inspect emitter/runtime PID fields directly, use JSON log output:

```bash
atm logs --json --limit 50
```

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

## Required AH.5 Troubleshooting Cases

### Logging disabled

ATM producers can be hard-disabled with:

```bash
ATM_LOG=0 atm status --json
```

Expected:
- `logging.state` reports `unavailable` or a degraded state with explicit finding.

Remediation:
1. Remove disable flag (`unset ATM_LOG` or set `ATM_LOG=info`).
2. Restart daemon (`atm daemon restart`) if needed.
3. Re-run `atm doctor --json` and confirm health recovery.

### Queue full / dropped events

Symptoms:
- `logging.dropped_counter` increases over time.
- doctor/status show `degraded_dropping`.

Remediation:
1. Confirm daemon is reachable (`atm daemon status`).
2. Confirm canonical log path is writable.
3. Reduce burst source or increase queue headroom in implementation/config.
4. Verify dropped counter stops increasing under normal load.

### Spool path override mismatch

ATM path override:

```bash
ATM_LOG_FILE=/tmp/atm-custom.jsonl atm status --json
```

Expected:
- `logging.spool_path` resolves relative to active log path parent.

`sc-compose` override:

```bash
SC_COMPOSE_LOG_FILE=/tmp/sc-compose.log sc-compose --help >/dev/null
```

Expected:
- `sc-compose` writes log to `/tmp/sc-compose.log`.
- spool uses sibling `/tmp/log-spool` unless otherwise configured.

### Level filtering

ATM:
- `ATM_LOG=warn` suppresses info/debug event lines from stderr output.

`sc-compose`:
- `SC_COMPOSE_LOG_LEVEL=warn` suppresses debug/info events such as
  resolver-decision traces.
- `SC_COMPOSE_LOG_FORMAT=human` switches on-disk lines from JSONL to
  human-readable format for manual triage.

## Escalation Criteria

Escalate when any are true:
- `unavailable` persists after remediation steps.
- `degraded_dropping` continues under normal load.
- spool age grows and never reconverges after daemon recovery.
