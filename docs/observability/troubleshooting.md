# Observability Troubleshooting (AK.4)

This runbook defines how to interpret mandatory OpenTelemetry behavior without
blocking ATM workflows.

## Mandatory Behavior

- Local structured logging is always considered available (`local_structured=true`).
- OTel export is fail-open: exporter issues must not fail `atm` commands.
- `atm doctor --json` and `atm status --json` expose:
  - `logging_health.status` (`ok|degraded|unavailable`)
  - `logging_health.otel_exporter`
  - `logging_health.local_structured`
  - `logging_health.last_export_error` (optional)

## Health Interpretation

- `status=ok`: canonical local logging is healthy.
- `status=degraded`: local logging remains active, but backlog/drop conditions
  were detected.
- `status=unavailable`: local logging health is unavailable (for example,
  disabled by env or daemon/path failures).

`otel_exporter` follows the same state buckets and never blocks command flow.

## Fallback Paths

- Canonical local JSONL and spool fallback continue even if OTel is degraded.
- Default local sink/spool paths are daemon-resolved from ATM home/config roots.
- OTel sidecar output is derived from canonical log path with `.otel.jsonl`
  suffix when exporter path defaults are used.

## Common Error Conditions

- `logging_health.last_export_error` present:
  - Treat as non-fatal exporter/local-health signal.
  - Continue normal operations; investigate exporter/env/path configuration.
- `ATM_OTEL_ENABLED=false|0|off|disabled|no`:
  - `otel_exporter=unavailable` by design.
  - Local structured logging remains available.
- Spool growth / dropped queue signals:
  - Expect `status=degraded`; commands continue.
  - Remediate daemon availability and reduce burst log pressure.

## Operator Commands

- `atm doctor --json`
- `atm status --json`
- `atm logs --limit 50`
