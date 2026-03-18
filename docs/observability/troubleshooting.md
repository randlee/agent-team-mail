# Observability Troubleshooting

This runbook defines how to interpret mandatory OpenTelemetry behavior without
blocking ATM workflows.

## Mandatory Behavior

- Canonical local structured logging remains the source-of-truth sink for
  operator diagnostics.
- OTel export is fail-open: exporter issues must not fail `atm` commands.
- `atm doctor --json` and `atm status --json` expose the locked
  `logging_health` object:
  - `logging_health.schema_version`
  - `logging_health.state`
  - `logging_health.log_root`
  - `logging_health.canonical_log_path`
  - `logging_health.spool_path`
  - `logging_health.dropped_events_total`
  - `logging_health.spool_file_count`
  - `logging_health.oldest_spool_age_seconds`
  - `logging_health.last_error.code`
  - `logging_health.last_error.message`
  - `logging_health.last_error.at`

## Health Interpretation

- `state=healthy`: canonical local logging is healthy.
- `state=degraded_spooling`: canonical local logging remains active, but spool
  backlog was detected.
- `state=degraded_dropping`: canonical local logging remains active, but queue
  pressure caused dropped events.
- `state=unavailable`: local logging health is unavailable (for example,
  disabled by env or daemon/path failures).

## Fallback Paths

- Canonical local JSONL and spool fallback continue even if OTel is degraded.
- Default local sink/spool paths are daemon-resolved from ATM home/config roots.
- OTel sidecar output is derived from canonical log path with `.otel.jsonl`
  suffix when exporter path defaults are used.

## Common Error Conditions

- `logging_health.last_error.*` present:
  - Treat as non-fatal exporter/local-health signal.
  - Continue normal operations; investigate exporter/env/path configuration.
- `ATM_OTEL_ENABLED=false|0|off|disabled|no`:
  - remote export stays disabled by design.
  - canonical local logging remains available.
- Spool growth / dropped queue signals:
  - Expect `state=degraded_spooling` or `state=degraded_dropping`; commands continue.
  - Remediate daemon availability and reduce burst log pressure.

## Operator Commands

- `atm doctor --json`
- `atm status --json`
- `atm logs --limit 50`
