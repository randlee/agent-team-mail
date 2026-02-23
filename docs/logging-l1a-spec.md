# Logging Overhaul L.1a/L.1b Spec (Issue #188)

Status: Draft for architecture review
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

## Canonical Event Schema

```rust
pub struct LogEventV1 {
    pub v: u8,                    // schema version, currently 1
    pub ts: String,               // RFC3339 UTC timestamp
    pub level: String,            // trace|debug|info|warn|error
    pub source_binary: String,    // atm|atm-daemon|atm-tui|atm-agent-mcp
    pub hostname: String,         // host that emitted the record
    pub pid: u32,                 // process id of emitter
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
- v1 redaction: minimal built-in denylist for sensitive keys in `fields`/`error`
  (`password`, `secret`, `token`, `api_key`, `auth`) plus bearer-token-like values.

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

## Migration Bridge (`emit_event_best_effort`)

During L.1/L.2 migration:

- bridge maps legacy `EventFields` to `LogEventV1`,
- emits unified event path,
- and also writes legacy `events.jsonl` until sunset.

Bridge control:

- `ATM_LOG_BRIDGE=dual` (default during migration),
- `ATM_LOG_BRIDGE=unified_only`,
- `ATM_LOG_BRIDGE=legacy_only` (rollback switch).

Sunset target: remove legacy writes in L.4 after parity + soak.

## Fallback When Daemon Is Unavailable

If producer cannot reach daemon:

- append to per-process fallback spool file in `fallback_spool_dir`.

Fallback spool filename convention (required for producer/daemon interop):

- `~/.config/atm/log-spool/{source_binary}-{pid}-{unix_millis}.jsonl`
- examples: `atm-12345-1739990012345.jsonl`, `atm-tui-12398-1739990012401.jsonl`

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
  Queue defaults: capacity `4096`, overflow policy `drop-new`, warn + dropped counter.
- Add daemon writer task (JSONL + rotation).
- Add fallback spool format and producer write path.

### L.1b

- Add `init_unified()` and `UnifiedLogMode` wiring in all 4 binaries.
- Implement dual-write bridge in `emit_event_best_effort`.
- Add daemon startup spool merge path (claim + append + cleanup).
- Add integration tests for fan-in, fallback, merge, and rotation.

## Acceptance Criteria (L.1 gate)

- non-daemon binaries do not append directly to canonical `atm.log.jsonl`,
- daemon accepts/rejects `log-event` deterministically with schema validation,
- log write failures do not crash any process,
- spool fallback + merge works across daemon restart,
- rotation works under sustained concurrent producer load.

## Resolved Defaults

- Queue defaults: capacity `4096`, overflow policy `drop-new`, warn + dropped counter.
- Redaction v1: minimal denylist + bearer token pattern; full policy deferred to L.5.
- Schema v1 includes required `hostname` and `pid`.
