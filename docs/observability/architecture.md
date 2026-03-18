# Observability Architecture

**Status**: Active (Phase AH baseline; Phase AV rollout planned; Phase AW expansion planned)
**Primary crate**: `sc-observability`
**See also**:
- `docs/observability/requirements.md`
- `docs/observability/troubleshooting.md`
- `docs/project-plan.md` (Phase AV and Phase AW sections)

## 1. Architecture Goals

- One shared observability implementation for ATM ecosystem tools.
- Deterministic, structured event schema.
- Default-on logging with fail-open behavior.
- Mandatory OpenTelemetry export with local file logging always available.

## 2. Components

- `sc-observability` (library): event model, validators, redaction, sink traits,
  health evaluator, default init path.
- `sc-observability-otlp` (planned dedicated OTLP transport adapter crate):
  collector transport for logs in AV; traces and metrics expand there in AW.
- Producers: `atm`, `atm-daemon`, `atm-tui`, `atm-agent-mcp`, `sc-compose`,
  `sc-composer`, `scmux`, `schook`.
- Daemon writer path: `atm-daemon` canonical sink for ATM producer traffic.
- Mandatory OTel exporter path for in-scope tools (non-optional AK rollout).

## 3. Data Flow

1. Producer initializes logger via `sc_observability::init("<tool>")`.
2. Producer emits structured event.
3. ATM producers send `log-event` to daemon.
4. Daemon validates, redacts, queues, and writes canonical JSONL.
5. If daemon unavailable, producer writes spool event; daemon merges on startup.
6. In Phase AV, `sc-observability` shapes neutral OTLP-ready log records.
7. `sc-observability-otlp` exports those records to a Grafana-compatible OTLP
   HTTP logs receiver.
8. Phase AW extends the same adapter boundary to traces and metrics.

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
- Grafana/collector connectivity is fail-open in AV and must remain fail-open in
  AW for traces and metrics too.

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

## 9.1 AV Rollout Boundary

- AV is a logs-first rollout to a Grafana-compatible OTLP HTTP logs receiver.
- It is sufficient for centralized logs and field-based correlation.
- It is not yet sufficient to claim native traces or metrics support.
- The required rollout verification is captured in
  `docs/observability/grafana-rollout-smoke.md`.

## 9.2 AW Target Architecture

- AW adds real trace and metric signals while preserving the same partition:
  - `sc-observability`: neutral contracts and fail-open semantics
  - `sc-observability-otlp`: OTLP transport and backend concerns
  - entry-point binaries: wiring only
- External repos (`scmux`, `schook`) must consume the same adapter/facade
  boundary rather than implementing ad hoc transport layers.

## 10. Diagnostics JSON Contract Lock

`atm doctor --json` and `atm status --json` share one locked `logging_health`
object contract:
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

No drift is allowed across these two command surfaces for shared keys.
