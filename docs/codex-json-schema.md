# Codex CLI-JSON Event Schema

This document describes the `cli-json` transport mode (`codex exec --json`) used by `atm-agent-mcp`.
Each line is a self-contained JSON object with a `type` field indicating the event kind.

Related docs:
- `docs/atm-agent-mcp/codex-execution-modes.md` (all Codex execution modes)
- `docs/atm-agent-mcp/requirements.md` (proxy transport requirements)

## Event Types

### `agent_message`

Model output text from the Codex agent.

| Field     | Type   | Description                        |
|-----------|--------|------------------------------------|
| `type`    | string | `"agent_message"`                  |
| `content` | string | The agent's text output            |
| `role`    | string | Message role (typically `"assistant"`) |

### `tool_call`

Tool invocation requested by the agent.

| Field       | Type   | Description                           |
|-------------|--------|---------------------------------------|
| `type`      | string | `"tool_call"`                         |
| `name`      | string | Tool name (e.g. `"write_file"`)       |
| `call_id`   | string | Unique identifier for this invocation |
| `arguments` | object | Tool-specific arguments               |

### `tool_result`

Result of a tool execution, written back to the agent.

| Field     | Type   | Description                             |
|-----------|--------|-----------------------------------------|
| `type`    | string | `"tool_result"`                         |
| `call_id` | string | Matches the `call_id` of the tool_call  |
| `output`  | string | Tool execution output text              |
| `error`   | string | Error message if the tool failed (optional) |

### `file_change`

Notification that a file was written or edited.

| Field    | Type   | Description                             |
|----------|--------|-----------------------------------------|
| `type`   | string | `"file_change"`                         |
| `path`   | string | Absolute or relative path to the file   |
| `action` | string | `"write"`, `"edit"`, or `"delete"`      |

### `idle`

The agent is waiting for input. This is the stdin queue drain window -- when this event is received, enqueued messages can be written to the child's stdin.

| Field  | Type   | Description     |
|--------|--------|-----------------|
| `type` | string | `"idle"`        |

The `idle` event signals that the Codex agent has completed its current turn and is ready to accept new input. The `atm-agent-mcp` proxy uses this event to:

1. Set the `idle_flag` on the `JsonCodecTransport`
2. Trigger a drain of the stdin queue (messages enqueued via `stdin_queue::enqueue`)
3. Emit a C.1 structured log event (`action: "idle_detected"`)

### `done`

Session complete. The agent has finished processing and will exit.

| Field  | Type   | Description     |
|--------|--------|-----------------|
| `type` | string | `"done"`        |

## Communication Pattern

```
Parent Process                    Codex (codex exec --json)
      |                                    |
      |  --- stdin: tool_result JSON --->  |
      |                                    |
      |  <--- stdout: JSONL events ------  |
      |    (agent_message, tool_call,      |
      |     file_change, idle, done)       |
      |                                    |
```

- **stdin**: The parent writes JSON objects (one per line) to inject tool results or messages
- **stdout**: The child writes JSONL events (one per line) as it processes
- **stderr**: Suppressed (redirected to null) to prevent interference with JSON-RPC on the parent's stdout

## Stdin Input Format

Messages written to `codex exec --json` stdin by the stdin queue drain are encoded as
newline-delimited JSON tool result objects:

```json
{"type":"tool_result","content":"<ATM message text>"}
```

Each message is terminated with `\n`. The `drain()` function writes one JSON object per
queued message. The `content` field contains the raw ATM message text.

## Idle Detection Behavior

The `idle` event is the primary trigger for draining the stdin queue.  Here is
how the `JsonCodecTransport` background task handles it:

### idle event processing

When a line with `"type":"idle"` is received:

1. `idle_flag` is set to `true` (an `Arc<AtomicBool>` shared between the
   background task and the proxy).
2. `cli_json_turn_state` transitions to `TurnState::Idle` (using the shared
   `stream_norm::TurnState` type — not a parallel type).
3. A `DaemonStreamEvent::TurnIdle { transport: "cli-json", ... }` is emitted
   to the ATM daemon via `stream_emit::emit_stream_event` (best-effort).
4. A C.1 structured log event (`action: "idle_detected"`) is emitted.
5. The proxy's idle-poll loop detects `is_idle() == true` and drains the stdin
   queue via `stdin_queue::drain`.

Messages enqueued in the stdin queue before the `idle` event fires are
guaranteed to be present in the drain on that idle cycle.

### idle_flag reset on activity

`idle_flag` is reset to `false` whenever any non-idle, non-done event arrives
(`agent_message`, `tool_call`, `tool_result`, `file_change`).  This signals
that the agent is processing again and the proxy must not drain until the next
`idle` event.

### Fallback drain: 30-second timeout

In addition to idle-triggered drains, the proxy drains the stdin queue every
30 seconds as a safety net.  This ensures messages are not indefinitely delayed
when the agent does not reach an idle state (e.g. because of a long-running
tool call).

### `done` event shape

The `done` event has no required additional fields beyond `"type":"done"`.  The
parser is intentionally lenient: extra fields (if present) are ignored.  The
`done` event resets `idle_flag` to `false` and transitions `cli_json_turn_state`
to `TurnState::Terminal { status: TurnStatus::Completed }`, then emits
`DaemonStreamEvent::TurnCompleted { transport: "cli-json", ... }`.

## Usage in atm-agent-mcp

The `JsonCodecTransport` handles this protocol:

1. Spawns `codex exec --json` with piped stdin/stdout
2. A background task reads child stdout line-by-line
3. Each line is parsed for event type (`idle`, `done`, `agent_message`, etc.)
4. `idle` events set the idle flag and trigger queue drain; `done` emits a C.1 log event
5. All lines (including `idle` and `done`) are forwarded to a duplex stream for the proxy reader
6. The proxy reader processes the forwarded lines as it would MCP JSON-RPC messages
