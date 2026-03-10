# Observability Architecture

**Status**: Active (Phase AH baseline; AJ/AK updates in planning)
**Primary crate**: `sc-observability`
**See also**:
- `docs/observability/requirements.md`
- `docs/project-plan.md` (Phase AJ and Phase AK sections)

## 1. Architecture Goals

- One shared observability implementation for ATM ecosystem tools.
- Deterministic, structured event schema.
- Default-on logging with fail-open behavior.
- Mandatory OpenTelemetry export with local file logging always available.

## 2. Components

- `sc-observability` (library): event model, validators, redaction, sink traits,
  health evaluator, default init path.
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
6. A mirrored OTel exporter sink emits selected traces/metrics.

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
- Canonical log root: `${home_dir}/.config/atm/logs`.
- Canonical sink path pattern: `${home_dir}/.config/atm/logs/<tool>/<tool>.log.jsonl`.
- Canonical spool path pattern: `${home_dir}/.config/atm/logs/<tool>/spool`.
- Companion tools maintain their own default root while preserving schema compatibility.

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

OTel export failures must never block core command execution; local structured
logging remains continuously available.
