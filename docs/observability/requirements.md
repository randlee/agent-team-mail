# Observability Requirements

**Status**: Active (Phase AH baseline; Phase AV complete; Phase AW complete; Phase AY dogfood-readiness planned)
**Scope**: `atm`, `atm-daemon`, `atm-tui`, `atm-agent-mcp`, `sc-compose`, `sc-composer`
**See also**:
- `docs/observability/architecture.md`
- `docs/project-plan.md` (Phase AV and Phase AW sections)

## 1. Purpose

Define the single source of truth for structured logging and mandatory OpenTelemetry
behavior across ATM tools and companion tooling.

## 2. Core Contract

- `sc-observability` is the shared structured-logging implementation.
- Logging is enabled by default.
- Logging must be fail-open; logging failures must not block core command flows.
- Tool outputs are namespaced under per-tool log directories beneath a common root.
- Schema and health-state semantics are shared across tools; no per-tool drift.
- OpenTelemetry export is required for in-scope tools in this document;
  non-optional logs export is effective in Phase AV rollout, with traces and
  metrics expanding in Phase AW.

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

Context fields (conditionally required by scope rules below):
- `team`, `agent`, `runtime`, `session_id`
- `trace_id`, `span_id`, `parent_span_id`, `subagent_id`
- `request_id`, `correlation_id`
- `outcome`, `error`
- `fields` (structured map)
- `spans` (ordered span-ref chain; each item includes
  `{trace_id, span_id, parent_span_id?, name?}`)
- `session_id` semantics: canonical ATM session identifier for correlation
  across runtime adapters. Runtime-native IDs (`CLAUDE_SESSION_ID`,
  `CODEX_THREAD_ID`, Gemini `sessionId`) must be normalized into this field.

Validation requirements:
- Reject payloads missing required fields.
- Enforce serialized-size guard (`64 KiB` max per line, initial default).
- Apply built-in redaction before enqueue/write.
- `action` must be stable snake_case; baseline vocabulary lives in `../logging-l1a-spec.md`.
- For agent/runtime-scoped events (`atm.send`, `atm.read`, spawn/resume, hook lifecycle,
  daemon member/session state transitions), `team`, `agent`, `runtime`, and
  `session_id` are mandatory correlation fields.

### 4.1 Correlation Rules by Event Scope

| Event scope | Mandatory correlation fields |
|---|---|
| System-level events (no actor/session context) | none |
| Agent/runtime-scoped events | `team`, `agent`, `runtime`, `session_id` |
| Trace events | `trace_id`, `span_id` |
| Sub-agent events (trace/log) | `team`, `agent`, `runtime`, `session_id`, `trace_id`, `span_id`, `subagent_id` |

### 4.2 `spans` Chain Semantics (ATM-QA-007)

If `spans` is present:
- It is an ordered chain from root to leaf.
- Every item must contain `trace_id` and `span_id`; `name` and `parent_span_id`
  are optional.
- All items must share the same `trace_id`.
- First item must omit `parent_span_id` (or set it `null`).
- For every item after the first, `parent_span_id` must equal the previous item's
  `span_id`.
- When top-level `trace_id`/`span_id` are present, the final `spans` item must
  match those top-level values.
- Violations are schema errors (`INVALID_PAYLOAD`).

## 5. Sink, Queue, Rotation, and Merge Requirements

Path profiles (ATM-QA-004):
- ATM-managed profile (default for ATM ecosystem binaries):
  - log root: `${home_dir}/.config/atm/logs`
  - sink: `${home_dir}/.config/atm/logs/<tool>/<tool>.log.jsonl`
  - spool: `${home_dir}/.config/atm/logs/<tool>/spool`
- Standalone profile (default for standalone companion tools):
  - log root: `${home_dir}/.config/<tool>/logs`
  - sink: `${home_dir}/.config/<tool>/logs/<tool>.log.jsonl`
  - spool: `${home_dir}/.config/<tool>/logs/spool`
- `home_dir` resolves via `get_home_dir()`.
- Explicit operator override may set log root; sink/spool still derive from the
  same profile formulas.

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
- Startup spool merge and runtime writer must target the same canonical per-tool sink path.
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
  `docs/observability/troubleshooting.md`.

Compatibility:
- Logging-health JSON schema is versioned and stable.
- `doctor --json` and `status --json` must use the same overlapping field semantics.

Required JSON keys for both `atm doctor --json` and `atm status --json`:
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

Contract lock (ATM-QA-009):
- The `logging_health` object name and key set above are stable contract keys.
- `atm doctor --json` and `atm status --json` must use the same field names,
  types, and nullability for these keys.

Formal `logging_health` JSON schema (v1):

| Field | Type | Nullable | Notes |
|---|---|---|---|
| `logging_health.schema_version` | string | no | currently `v1` |
| `logging_health.state` | string enum | no | `healthy|degraded_spooling|degraded_dropping|unavailable` |
| `logging_health.log_root` | string | no | resolved log root directory |
| `logging_health.canonical_log_path` | string | no | canonical structured sink path |
| `logging_health.spool_path` | string | no | spool directory path |
| `logging_health.dropped_events_total` | integer (`u64`) | no | dropped event counter |
| `logging_health.spool_file_count` | integer (`u64`) | no | spool file count |
| `logging_health.oldest_spool_age_seconds` | integer (`u64`) | yes | null when spool empty |
| `logging_health.last_error.code` | string | yes | null when no current error |
| `logging_health.last_error.message` | string | yes | null when no current error |
| `logging_health.last_error.at` | string (RFC3339 UTC) | yes | null when no current error |

## 7. Event Coverage Requirements

Minimum required coverage:
- `atm`: `send`, `broadcast`, `request`, `read`, watermark updates, teams ops.
- `atm-daemon`: lifecycle, session-registry transitions, plugin lifecycle/errors.
- `atm-agent-mcp`: tool-call audit + lifecycle context.
- `atm-tui`: startup/shutdown, stream attach/detach, control-send/ack summaries.
- `sc-compose`: render/validate command lifecycle, missing-var diagnostics,
  output-path decisions, and runtime errors.
- `sc-composer`: library render lifecycle, include expansion outcomes, and
  variable resolution diagnostics.

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

- OTel export is mandatory for in-scope tools (logs in Phase AV; traces and
  metrics expand in Phase AW).
- Local structured file logging remains mandatory regardless of OTel state.
- OTel exporter startup is enabled by default.
- Temporary disablement is allowed only for tests/controlled diagnostics paths
  with explicit operator intent and warning emission.
- `session_id` is a required OTel attribute for all agent/runtime-scoped spans,
  events, and metrics dimensions where identity applies.
- Runtime-native identifiers (`Claude session-id`, `Codex thread-id`, `Gemini session-id`)
  must be normalized into one OTel attribute name: `session_id`.
- OTel payloads that include `session_id` must also include `team`, `agent`, and
  `runtime` so logs can be joined back to runtime session JSONL artifacts.
- `trace_id` and `span_id` remain valid correlation fields when present in log
  records; native traces and metrics are Phase AW scope.
- Phase AV is logs-only. It delivers OTLP/HTTP `/v1/logs` collector export plus
  local `.otel.jsonl` mirroring for fail-open auditing.
- Phase AV must add live collector export for the binaries that ship from this
  repository: `atm`, `atm-daemon`, `atm-tui`, `atm-agent-mcp`, `sc-compose`,
  and `sc-composer`.
- `scmux` and `schook` remain explicit follow-on work in their own repositories;
  Phase AV must document the handoff instead of implying those tools are
  delivered here.
- Export targets must be explicit:
  - primary: OTLP/HTTP collector export
  - secondary: stdout exporter for controlled diagnostics
  - retained local mirror: per-tool `.otel.jsonl` sidecar for fail-open auditing
- OTel partition boundary is mandatory:
  - `sc-observability` owns neutral event shaping, `OtelRecord`, correlation
    requirements, and exporter traits only
  - one dedicated transport adapter crate owns OTLP endpoint config, auth/TLS,
    batching, retry behavior, and all `opentelemetry*` / OTLP SDK dependencies
  - non-transport crates must not import `opentelemetry*`,
    `opentelemetry-otlp`, or collector-specific SDK crates directly
- Caller-facing crates (`atm`, `atm-daemon`, `atm-tui`, `atm-agent-mcp`,
  `sc-compose`, `sc-composer`) must emit through the shared observability
  facade and must not construct exporters directly.
- Phase AV must define one canonical transport config surface: endpoint,
  protocol, auth/TLS material, export enablement, and debug exporter controls.
  Per-binary config drift is forbidden.
- Retry/backoff controls are part of the Phase AV config surface. Native
  traces, metrics, and transport batching/flush controls are Phase AW scope.
- High-value in-repo telemetry required in Phase AV includes:
  - CLI request/response log correlation for `atm read`, `atm send`, and daemon commands
  - daemon request, plugin dispatch, and lifecycle log coverage

## 9.1 Live Grafana Verification and Dogfood Readiness

After Phase AW, the repository must satisfy a live Grafana verification
contract before shared dev-daemon dogfooding can be considered ready:

- OTel log records from `atm` must be queryable in Loki via
  `service_name="atm"` for a live smoke session.
- OTel trace records from `atm-daemon` must be queryable in Tempo via
  `resource.service.name = "atm-daemon"` for a fresh daemon start that
  inherited the active OTel env/config.
- OTel metrics must be queryable in Mimir/Prometheus using the canonical
  exported metric names, not dashboard-local aliases or guessed names.
- Shared dev-daemon startup paths (`scripts/dev-install`, canonical launcher,
  and operator restart flows) must preserve the active OTel export config so a
  newly started shared daemon participates in live collector export without ad
  hoc manual patching after startup.
- Grafana Cloud read verification must use backend-specific Basic auth
  usernames (Loki, Tempo, Mimir instance IDs) plus a read token with the
  required scopes. Reusing one precomputed auth header across all three
  backends is forbidden.
- Any smoke requirement that depends on daemon-owned traces must explicitly
  control daemon lifecycle so an already-running daemon without OTel env does
  not invalidate the result.

AY closes this contract after the AW stack lands.
  - GitHub firewall/ledger correlation fields in log records
  - worker/session lifecycle log coverage already represented in local JSONL logs
  - MCP request/session log correlation within `atm-agent-mcp`

### 9.1.1 In-Repo Scope Lock

Phase AV implementation scope for this repository is limited to:
- `atm`
- `atm-daemon`
- `atm-tui`
- `atm-agent-mcp`
- `sc-compose`
- `sc-composer`
- `sc-observability`
- `sc-observability-otlp`

`scmux` and `schook` are explicit follow-on work in their own repositories.
They must not be treated as delivered by this repository's AV implementation.

### 9.1.2 Import Boundary Enforcement

- `scripts/ci/observability_boundary_check.sh` is the canonical CI/review gate
  for `ARCH-BOUNDARY-002`.
- The gate must fail on direct `opentelemetry*`, `opentelemetry-otlp`, or
  `sc-observability-otlp` imports/dependencies outside the approved adapter and
  entry-point layer.
- The gate must also fail on direct `sc-observability` imports from modules
  outside the approved entry-point/facade wiring set.
- CI must run the observability boundary check before AV.2 begins, and the rule
  remains mandatory after AV.2 lands.

## 9.2 Phase AV Grafana Rollout Requirements

- Phase AV is the logs-first rollout to a Grafana-compatible OTLP HTTP logs
  receiver.
- AV acceptance requires the smoke protocol in
  `docs/observability/grafana-rollout-smoke.md` to pass.
- The rollout must prove all of the following at the same time:
  - remote OTLP HTTP log delivery works
  - canonical local JSONL logging still works
  - collector outage/auth/TLS failure remains fail-open
- Correlation fields exposed remotely must include at minimum:
  - `team`
  - `agent`
  - `runtime`
  - `session_id`
- This AV rollout is sufficient for centralized logs in Grafana but is not yet
  sufficient to claim full traces/metrics support.

## 9.3 Phase AW Traces + Metrics Expansion Requirements

- Phase AW extends AV from logs-only collector export to native traces and
  metrics.
- `sc-observability` may add neutral `TraceRecord` / `MetricRecord` contracts,
  but transport-specific payload shaping remains outside it.
- `sc-observability-otlp` must remain the only OTLP transport-owning crate for:
  - logs
  - traces
  - metrics
- Grafana acceptance in AW requires:
  - queryable logs
  - usable trace views for ATM flows
  - metric panels for export health and runtime activity

## 10. Cross-Tool Integration Requirements

- `sc-compose` and `sc-composer` must use `sc-observability` instead of local,
  duplicated logger implementations.
- Embedded-library usage must allow host-injected sink/path configuration.
- Standalone tool defaults remain per-tool scoped (for example `sc-compose` log root).

### 10.1 External Consumer Contract (`scmux` / `schook`)

- External consumers must integrate through the shared facade and adapter
  surfaces, not by inventing their own OTLP transport wiring.
- They must honor the same env/config surface used by ATM.
- AV only requires logs-first carry-forward notes and smoke alignment.
- AW is responsible for traces/metrics adoption planning across those repos.

## 11. Delivery Mapping and Testability

Phase mapping:
- AH.1: shared crate contracts (`LogEventV1`, socket contract, queue/rotation/spool baseline).
- AJ: session identity canonicalization (`session_id` normalization and SSoT alignment).
- AV: OTLP HTTP logs rollout, dogfood, Grafana hookup smoke, and external handoff.
- AW: native traces+metrics expansion, Grafana dashboards, and external repo rollout.

Testability gate:
- Every requirement section above is enforced by at least one unit or integration test.
- CI must fail when any required JSON key, required correlation field, or
  required event coverage contract is absent.
