# Logging Overhaul L.1a/L.1b Spec (Issue #188)

Status: Implemented (L.1a/L.1b delivered, Phase L follow-on hardening complete; AH.1 constants/contracts refreshed)
Owner: `arch-ctm`
Scope: Unified structured logging fan-in design for `atm`, `atm-daemon`, `atm-tui`, `atm-agent-mcp`

## Goal

Replace fragmented logging (`tracing` to stderr plus partial `events.jsonl`) with one canonical structured pipeline that is:

- complete across binaries,
- fail-open,
- safe under multi-process load,
- and consumable by both CLI and TUI.

## Architecture Decision

Canonical sink is daemon-owned.

- Producers (`atm`, `atm-tui`, `atm-agent-mcp`, and daemon internals) emit structured records.
- Non-daemon binaries send records to daemon via socket command `log-event`.
- `atm-daemon` is the only process that writes `atm.log.jsonl` and performs rotation.

Rationale:

- avoids cross-process append interleaving,
- avoids rotation race conditions,
- centralizes validation/redaction/policy.

## Wire Contract

### Request

Existing envelope (`SocketRequest`) with:

- `version`
- `request_id`
- `command = "log-event"`
- `payload = LogEventV1`

### Response

Success:

```json
{
  "version": 1,
  "request_id": "req-123",
  "status": "ok",
  "payload": { "accepted": true }
}
```

Error:

```json
{
  "version": 1,
  "request_id": "req-123",
  "status": "error",
  "error": { "code": "INVALID_PAYLOAD", "message": "..." }
}
```

Error codes:

- `VERSION_MISMATCH`
- `INVALID_PAYLOAD`
- `INTERNAL_ERROR`

Code ownership:
- Canonical error-code constants are exported by `crates/sc-observability`:
  - `SOCKET_ERROR_VERSION_MISMATCH`
  - `SOCKET_ERROR_INVALID_PAYLOAD`
  - `SOCKET_ERROR_INTERNAL_ERROR`

## Canonical Event Schema

```rust
pub struct LogEventV1 {
    pub v: u8,                    // schema version, currently 1
    pub ts: String,               // RFC3339 UTC timestamp
    pub level: String,            // trace|debug|info|warn|error
    pub source_binary: String,    // atm|atm-daemon|atm-tui|atm-agent-mcp
    pub hostname: String,         // producer host name
    pub pid: u32,                 // producer process id
    pub target: String,           // tracing target/module path
    pub action: String,           // stable event name
    pub team: Option<String>,
    pub agent: Option<String>,
    pub session_id: Option<String>,
    pub request_id: Option<String>,
    pub correlation_id: Option<String>,
    pub outcome: Option<String>,  // ok|err|timeout|dropped|...
    pub error: Option<String>,
    pub fields: serde_json::Map<String, serde_json::Value>,
    pub spans: Vec<SpanRefV1>,
}

pub struct SpanRefV1 {
    pub name: String,
    pub fields: serde_json::Map<String, serde_json::Value>,
}
```

Validation:

- required: `ts`, `level`, `source_binary`, `hostname`, `pid`, `target`, `action`
- serialized-size guard (initial target: 64 KiB per line)
- redaction policy runs before persistence

### Action Vocabulary (Canonical Baseline)

`action` is a stable machine-readable event name. It is not a closed enum, but
the following baseline values are canonical and should not be renamed without a
versioned migration:

- lifecycle/control: `daemon_start`, `daemon_stop`, `control_request`, `control_ack`
- stream boundary signals: `stream_error_summary`, `stream_dropped_counters`
- CLI ops: `send`, `broadcast`, `read`, `read_mark`
- TUI ops: `tui_start`, `session_connect`, `stream_attach`, `stream_detach`
- MCP transport/session: `transport_init`, `transport_shutdown`, `turn_started`, `turn_completed`, `turn_terminal_crash`, `idle_detected`, `codex_done`
- MCP audit queueing: `audit_event`, `stdin_queue_enqueue`, `stdin_queue_drain`

Guidance:
- New actions are allowed when needed, but must remain snake_case and be added
  to docs when they become operationally significant for dashboards/alerts.

## Initialization API

```rust
pub enum UnifiedLogMode {
    ProducerFanIn {
        daemon_socket: std::path::PathBuf,
        fallback_spool_dir: std::path::PathBuf,
    },
    DaemonWriter {
        file_path: std::path::PathBuf,
        rotation: RotationConfig,
    },
    StderrOnly,
}

pub struct RotationConfig {
    pub max_bytes: u64,
    pub max_files: u32,
}

pub fn init_unified(
    source_binary: &str,
    mode: UnifiedLogMode,
) -> anyhow::Result<LoggingGuards>;
```

Behavior:

- all modes include human stderr layer,
- producer mode adds non-blocking fan-in forwarding layer,
- daemon mode adds JSON file writer layer and rotation,
- any initialization failure degrades to `StderrOnly` (fail-open).

## Migration Bridge (`emit_event_best_effort`) — REMOVED (Phase M.1b)

The dual-write bridge and legacy `events.jsonl` sink were removed in Phase M.1b.
`emit_event_best_effort` now routes exclusively through the unified producer channel.
`ATM_LOG_BRIDGE` is no longer a recognized environment variable.

## Fallback When Daemon Is Unavailable

If producer cannot reach daemon:

- append to per-process fallback spool file in `fallback_spool_dir`.

On daemon startup:

- claim spool files with rename/lock pattern,
- merge into canonical log in append order,
- delete claimed file only after successful merge.

Merge invariants:

- append-only; no in-place rewrites,
- deterministic ordering by event timestamp then file order,
- idempotent claim behavior.

## Rotation

Daemon-only.

- size-based rotation (`max_bytes`, `max_files`),
- close current writer before rotate,
- rename N..1, reopen base path,
- never block producer path on rotation longer than bounded queue drain.

## L.1 Work Breakdown

### L.1a

- Add `LogEventV1` + serde/schema tests.
- Add daemon `log-event` socket handler.
- Add bounded in-memory queue for accepted log events.
- Add daemon writer task (JSONL + rotation).
- Add fallback spool format and producer write path.

### L.1b

- Add `init_unified()` and `UnifiedLogMode` wiring in all 4 binaries.
- ~~Implement dual-write bridge in `emit_event_best_effort`~~ (removed in Phase M.1b)
- Add daemon startup spool merge path (claim + append + cleanup).
- Add integration tests for fan-in, fallback, merge, and rotation.

## Acceptance Criteria (L.1 gate)

- non-daemon binaries do not append directly to canonical `atm.log.jsonl`,
- daemon accepts/rejects `log-event` deterministically with schema validation,
- log write failures do not crash any process,
- spool fallback + merge works across daemon restart,
- rotation works under sustained concurrent producer load.

## Resolved Decisions

- Queue capacity and overflow policy: bounded queue capacity is `4096` with drop-new behavior when saturated.
- Redaction denylist behavior: denylist keys are `password`, `secret`, `token`, `api_key`, and `auth`; bearer-token values are also redacted.
- Canonical schema v1 includes `hostname` and `pid` fields for producer/process attribution.
