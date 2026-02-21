# Codex JSON Event Schema

The `codex exec --json` command communicates via a JSONL (newline-delimited JSON) event stream on stdout. Each line is a self-contained JSON object with a `type` field indicating the event kind.

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

## Usage in atm-agent-mcp

The `JsonCodecTransport` handles this protocol:

1. Spawns `codex exec --json` with piped stdin/stdout
2. A background task reads child stdout line-by-line
3. Each line is parsed for event type (`idle`, `done`, `agent_message`, etc.)
4. `idle` events set the idle flag and trigger queue drain; `done` emits a C.1 log event
5. All lines (including `idle` and `done`) are forwarded to a duplex stream for the proxy reader
6. The proxy reader processes the forwarded lines as it would MCP JSON-RPC messages
