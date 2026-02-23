# ADR 003: CLI-JSON Mode Transport

Status: Accepted
Date: 2026-02-20

## Context

The `atm-agent-mcp` proxy was originally hardcoded to spawn `codex mcp-server` as its only child process communication method. The MCP (Model Context Protocol) transport works well for structured tool calls but does not support the JSONL event stream that `codex exec --json` provides.

The JSONL event stream offers advantages for certain use cases:

- **Idle detection**: The `idle` event signals when the agent is waiting for input, enabling non-destructive message injection via a file-based stdin queue.
- **Event visibility**: All agent activities (messages, tool calls, file changes) are visible as structured events.
- **Simpler protocol**: No JSON-RPC framing overhead; each line is a self-contained event.

## Decision

Add `JsonCodecTransport` as a second production transport alongside `McpTransport`. Both implement the `CodexTransport` trait introduced in Sprint C.2a.
In current terminology, this mode is named `cli-json`.

### Key design choices

1. **Transport selection via config**: The `.atm.toml` `transport` field selects the transport:
   - `"mcp"` (default) -> `McpTransport` (spawns `codex mcp-server`)
   - `"cli-json"` -> `JsonCodecTransport` (spawns `codex exec --json`)
   - `"mock"` -> `MockTransport` (in-memory test double)

2. **Idle flag with atomic bool**: `JsonCodecTransport` (`cli-json`) maintains a shared `Arc<AtomicBool>` idle flag. A background task monitors child stdout for `idle` JSONL events and sets the flag. The flag is also stored in `RawChildIo.idle_flag` so the proxy reader task can access it.

3. **Duplex stream forwarding**: Rather than giving the proxy direct access to the real child stdout, the background task reads from the child and forwards all lines to a `tokio::io::duplex` stream. This allows the background task to intercept `idle` events without disrupting the proxy's line-by-line reading.

4. **Stdin queue**: A file-based message injection queue (`stdin_queue.rs`) enables external processes to enqueue messages for delivery to the Codex child. Messages are atomically claimed via `create_new(true)` / `O_CREAT|O_EXCL` lock files (`{uuid}.lock`) to prevent double-delivery. On both POSIX and Windows this is a single atomic kernel operation — exactly one concurrent drainer will succeed in creating the lock. On success the drainer reads `{uuid}.json`, writes it to stdin, then removes both `{uuid}.json` and `{uuid}.lock`. On write failure only the lock file is removed, leaving `{uuid}.json` for a retry on the next drain cycle. The rename-based approach (`{uuid}.json` → `{uuid}.claimed`) used previously was replaced because `MoveFileEx` without `MOVEFILE_REPLACE_EXISTING` still races under concurrent `spawn_blocking` on Windows, whereas `O_CREAT|O_EXCL` provides true atomicity on all platforms. The queue is drained on idle events and on a 30-second periodic timer.

5. **Renamed test double**: The original `JsonTransport` (in-memory channel-based test double) was renamed to `MockTransport` to avoid confusion with the production `JsonCodecTransport`.

## Consequences

### Positive

- Three production transport modes are planned (`mcp`, `cli-json`, `app-server`), selectable via configuration
- Idle detection enables non-blocking message injection mid-session
- File-based stdin queue is safe for concurrent writers (atomic `O_CREAT|O_EXCL` claim)
- Clean separation: `MockTransport` for tests, `JsonCodecTransport` for production cli-json mode

### Negative

- Additional background task per JSON-mode session (idle detection + forwarding)
- Stdin queue adds filesystem I/O on every drain cycle (mitigated by 30s interval)
- Two code paths to maintain for transport-specific behavior in the proxy

### Neutral

- `RawChildIo` gained an `idle_flag` field (always `None` for MCP/Mock transports)
- The `CodexTransport` trait gained an `is_idle()` method with a default `false` implementation

## Follow-up

`app-server` is now a planned third production mode for richer thread/turn control (`turn/start`, `turn/steer`, `turn/interrupt`), while preserving MCP upstream compatibility.
