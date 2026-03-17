# Observability Architecture

**Status**: Active (Phase AH baseline; AJ complete; AV updates in planning)
**Primary crate**: `sc-observability`
**See also**:
- `docs/observability/requirements.md`
- `docs/observability/troubleshooting.md`
- `docs/project-plan.md` (Phase AJ and Phase AV sections)

## 1. Architecture Goals

- One shared observability implementation for ATM ecosystem tools.
- Deterministic, structured event schema.
- Default-on logging with fail-open behavior.
- Mandatory OpenTelemetry export with local file logging always available.
- OTel collector transport remains partitioned from generic observability logic.

## 2. Components

- `sc-observability` (library): event model, validators, redaction, sink traits,
  health evaluator, default init path.
- planned dedicated OTel transport adapter crate: owns OTLP/collector transport,
  auth/TLS, batching, retry, and SDK dependency integration.
- Producers: `atm`, `atm-daemon`, `atm-tui`, `atm-agent-mcp`, `sc-compose`,
  `sc-composer`, `scmux`, `schook`.
- Daemon writer path: `atm-daemon` canonical sink for ATM producer traffic.
- Mandatory OTel exporter path for in-scope tools (non-optional AV rollout).

## 3. Data Flow

1. Producer initializes logger via `sc_observability::init("<tool>")`.
2. Producer emits structured event.
3. ATM producers send `log-event` to daemon.
4. Daemon validates, redacts, queues, and writes canonical JSONL.
5. If daemon unavailable, producer writes spool event; daemon merges on startup.
6. `sc-observability` maps structured events into neutral OTel records.
7. The OTel transport adapter exports those records to the configured
   collector target and optional debug mirrors.

## 4. Canonical State and Health Computation

Health state is computed from canonical runtime inputs:
- daemon reachability,
- sink/spool path resolution,
- spool inventory/age,
- dropped-event counters,
- last logging error metadata.

States:
- `healthy`
- `degraded_spooling`
- `degraded_dropping`
- `unavailable`

Health evaluator is implemented once and reused by `atm doctor` and `atm status`.

## 5. Schema and Compatibility

- `LogEventV1` is the stable event envelope.
- Additive fields are allowed with compatibility notes.
- Field removal or semantic redefinition requires explicit migration documentation.

## 6. Pathing and Namespacing

- Shared schema, per-tool namespace in output pathing.
- Path profile A (ATM-managed default): `${home_dir}/.config/atm/logs/<tool>/...`
  (`<tool>.log.jsonl` sink + `spool/`).
- Path profile B (standalone default): `${home_dir}/.config/<tool>/logs/...`
  (`<tool>.log.jsonl` sink + `spool/`).
- Profile selection is deterministic by runtime mode; explicit operator override
  can replace root, but sink/spool derivation pattern stays profile-consistent.
- Companion tools preserve schema compatibility in either profile.

## 7. Failure Semantics

- Logging must not fail command execution.
- Socket send failures degrade to spool fallback.
- Merge is append-only with source deletion only after successful merge.

## 8. Security and Redaction

- Denylist key redaction and bearer-token pattern filtering are mandatory.
- Sensitive values are never emitted in clear text unless explicitly permitted by policy.

## 9. OpenTelemetry Baseline

Required baseline:
- Traces: `subagent.run`, `atm.send`, `atm.read`, `daemon.request`.
- Metrics: `subagent_runs_total`, `subagent_run_duration_ms`,
  `subagent_active_count`, `atm_messages_total`, `log_events_total`,
  `warnings_total`, `errors_total`.
- Correlation attributes for agent/runtime-scoped telemetry are mandatory:
  `team`, `agent`, `runtime`, `session_id`.
- Runtime-specific naming differences (`thread-id` vs `session-id`) are adapter
  internals only; export uses canonical `session_id` attribute.
- `trace_id`/`span_id` are required for traces; `subagent_id` is required for
  sub-agent telemetry.
- `spans` chain semantics are root→leaf ordered, same-trace constrained, and
  must parent-link each consecutive span (`parent_span_id == previous span_id`).
- When top-level `trace_id`/`span_id` are present, the final `spans` item must
  match those values.

OTel export failures must never block core command execution; local structured
logging remains continuously available.

## 9.1 OTel Partition Boundary

- `sc-observability` remains the generic observability layer. It owns:
  - structured event schema
  - validation and redaction
  - local JSONL sink/spool behavior
  - neutral `OtelRecord` shaping
  - exporter trait definitions and fail-open semantics
- The dedicated OTel transport adapter owns:
  - OTLP protocol/client dependencies
  - collector endpoint configuration
  - auth/TLS and headers
  - batching, flush, and remote retry policy
  - stdout/debug export modes
- Application and daemon crates call the generic observability facade only.
  They must not import collector SDK crates or construct OTLP exporters
  themselves.
- The partition goal for Phase AV is to keep collector-facing code in one
  replaceable layer so future backend changes do not leak through the rest of
  the codebase.

## 10. Diagnostics JSON Contract Lock

`atm doctor --json` and `atm status --json` share one locked `logging_health`
object contract:
- `status`, `otel_exporter`, `local_structured`, `last_export_error`.

No drift is allowed across these two command surfaces for shared keys.
