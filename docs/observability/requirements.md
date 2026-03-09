# Observability Requirements

**Status**: Draft (Phase AH)
**Scope**: `atm`, `atm-daemon`, `atm-tui`, `atm-agent-mcp`, `sc-compose`, `sc-composer`, `scmux`, `schook`

## 1. Purpose

Define the single source of truth for structured logging and optional OpenTelemetry
behavior across ATM tools and companion tooling.

## 2. Core Contract

- `sc-observability` is the shared structured-logging implementation.
- Logging is enabled by default.
- Logging must be fail-open; logging failures must not block core command flows.
- Tool outputs are namespaced under per-tool log directories beneath a common root.
- Schema and health-state semantics are shared across tools; no per-tool drift.

## 3. Canonical Logging Architecture Contract

- Producers emit `log-event` to daemon over the existing socket envelope.
- `atm-daemon` is the only writer to canonical ATM log files.
- If daemon is unavailable, producers spool locally; daemon merges spool on startup.

### 3.1 Socket Contract (`command = "log-event"`)

- Request envelope: existing `SocketRequest` (`version`, `request_id`, `command`, `payload`).
- Command: `log-event`.
- Payload: `LogEventV1`.
- Success response: `status="ok"` with `{ "accepted": true }`.
- Error response: `status="error"` with code:
  - `VERSION_MISMATCH`
  - `INVALID_PAYLOAD`
  - `INTERNAL_ERROR`

## 4. Event Schema Contract (`LogEventV1`)

Required fields:
- `v` (schema version)
- `ts` (RFC3339 UTC)
- `level` (`trace|debug|info|warn|error`)
- `source_binary`
- `hostname`
- `pid`
- `target`
- `action`

Optional correlation fields:
- `team`, `agent`, `session_id`
- `request_id`, `correlation_id`
- `outcome`, `error`
- `fields` (structured map), `spans` (span refs)

Validation requirements:
- Reject payloads missing required fields.
- Enforce serialized-size guard (`64 KiB` max per line, initial default).
- Apply built-in redaction before enqueue/write.
- `action` must be stable snake_case; baseline vocabulary lives in `docs/logging-l1a-spec.md`.

## 5. Sink, Queue, Rotation, and Merge Requirements

Canonical ATM log file:
- `${home_dir}/.config/atm/atm.log.jsonl` where `home_dir` resolves via `get_home_dir()`.

Producer fallback spool directory:
- `${home_dir}/.config/atm/log-spool`.

Spool filename:
- `{source_binary}-{pid}-{unix_millis}.jsonl`.

Queue/rotation defaults:
- Daemon in-memory queue capacity: `4096`.
- Overflow policy: `drop-new`.
- Overflow observability: increment dropped counter + rate-limited warning.
- Redaction denylist keys: `password`, `secret`, `token`, `api_key`, `auth`, plus bearer-token pattern.
- Rotation default: size-based at `50 MiB`, retain `5` rotated files.
- Retention default: `7 days`, configurable.

Merge semantics:
- Startup spool merge and runtime writer must target the same canonical sink path.
- Merge ordering is timestamp then file order, append-only.
- Source spool file deletion is allowed only after successful merge.

## 6. Logging Health Requirements

Health states:
- `healthy`
- `degraded_spooling`
- `degraded_dropping`
- `unavailable`

Rules:
- Silent degradation is forbidden.
- State transitions into degraded/unavailable must emit structured warning/error events.
- Health computation must be implemented once in shared logic consumed by both
  `atm doctor` and `atm status`.

Diagnostics surface:
- `atm doctor --json` must include health state, canonical log path, spool path,
  dropped-event counter, spool count/oldest age, and last logging error.
- Human-readable `atm doctor` must surface degraded/unavailable states with
  actionable remediation.
- `atm status --json` must expose logging health state.
- Runbook mapping of health states to remediation commands must exist in
  `docs/logging-troubleshooting.md`.

Compatibility:
- Logging-health JSON schema is versioned and stable.
- `doctor --json` and `status --json` must use the same overlapping field semantics.

## 7. Event Coverage Requirements

Minimum required coverage:
- `atm`: `send`, `broadcast`, `request`, `read`, watermark updates, teams ops.
- `atm-daemon`: lifecycle, session-registry transitions, plugin lifecycle/errors.
- `atm-agent-mcp`: tool-call audit + lifecycle context.
- `atm-tui`: startup/shutdown, stream attach/detach, control-send/ack summaries.
- `scmux`: team/orchestration lifecycle, message routing outcomes, and transport errors.
- `schook`: hook invocation lifecycle (`session_start`, `session_end`, compact events), policy decision outcomes, and hook failures.

Lifecycle and hook coverage:
- `member_state_change` (INFO) for `Offline ↔ Online` only.
- `member_activity_change` (DEBUG) for `Busy ↔ Idle` only.
- `session_id_change` (INFO), `process_id_change` (INFO).
- Hook events: `hook.session_start`, `hook.pre_compact`, `hook.compact_complete`,
  `hook.session_end`, `hook.failure`.

## 8. Runtime Controls

- `ATM_LOG=trace|debug|info|warn|error` controls stderr verbosity.
- `ATM_LOG_MSG=1` enables message preview text.
- `ATM_LOG_FILE` may override sink path for tests/ops.

## 9. OpenTelemetry Requirements

- OTel export support is optional and feature-gated (default off).
- Local structured file logging remains available regardless of OTel state.
- Initial OTel baseline must include:
  - traces: `subagent.run`, `atm.send`, `atm.read`, `daemon.request` (selected paths)
  - metrics: `subagent_runs_total`, `subagent_run_duration_ms`,
    `subagent_active_count`, `atm_messages_total`, `log_events_total`,
    `warnings_total`, `errors_total`

## 10. Cross-Tool Integration Requirements

- `sc-compose` and `sc-composer` must use `sc-observability` instead of local,
  duplicated logger implementations.
- Embedded-library usage must allow host-injected sink/path configuration.
- Standalone tool defaults remain per-tool scoped (for example `sc-compose` log root).
