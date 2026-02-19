# Codex MCP Interface

Codex CLI (`@openai/codex`) can run as an MCP server, exposing two tools for starting and continuing agentic coding sessions.

## Starting the Server

```bash
claude mcp add codex -s user \
  -e PATH="/your/node/bin:/usr/local/bin:/usr/bin:/bin" \
  -- codex mcp-server
```

With model and reasoning effort overrides:

```bash
claude mcp add codex -s user \
  -e PATH="/your/node/bin:/usr/local/bin:/usr/bin:/bin" \
  -- codex -m o3 -c model_reasoning_effort="high" mcp-server
```

Auth is handled via `codex login` (stored in `~/.codex/`). No `OPENAI_API_KEY` env var needed if already authenticated through your account.

Server info: `codex-mcp-server` v0.103.0, protocol version `2025-03-26`.

---

## Tools

### `codex`

Start a new Codex session.

**Input schema:**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `prompt` | string | **yes** | Initial user prompt / task instruction |
| `cwd` | string | no | Working directory (resolved against server's cwd if relative) |
| `model` | string | no | Model override, e.g. `o3`, `gpt-5.2-codex` |
| `approval-policy` | enum | no | `untrusted` \| `on-failure` \| `on-request` \| `never` |
| `sandbox` | enum | no | `read-only` \| `workspace-write` \| `danger-full-access` |
| `base-instructions` | string | no | Replace default system instructions entirely |
| `developer-instructions` | string | no | Injected as a developer role message |
| `compact-prompt` | string | no | Prompt used when compacting the conversation |
| `profile` | string | no | Config profile from `~/.codex/config.toml` |
| `config` | object | no | Arbitrary config overrides (key/value, same as `-c` flags) |

**Output schema:**

```json
{
  "threadId": "string",
  "content": "string"
}
```

**Example:**

```json
{
  "name": "codex",
  "arguments": {
    "prompt": "Fix the failing tests in src/",
    "cwd": "/path/to/project",
    "sandbox": "workspace-write",
    "approval-policy": "on-failure"
  }
}
```

---

### `codex-reply`

Continue an existing Codex session using a thread ID returned from a prior `codex` call.

**Input schema:**

| Parameter | Type | Required | Description |
|---|---|---|---|
| `prompt` | string | **yes** | Next user message to continue the conversation |
| `threadId` | string | no* | Thread ID from a previous `codex` response |

*`threadId` is optional for backward compatibility but should always be provided to resume the correct session.

**Output schema:**

```json
{
  "threadId": "string",
  "content": "string"
}
```

**Example:**

```json
{
  "name": "codex-reply",
  "arguments": {
    "threadId": "<id from prior codex call>",
    "prompt": "Now add tests for the changes you made."
  }
}
```

---

## Multi-Turn Pattern

Session continuity is built in via `threadId`. Claude can orchestrate a multi-turn Codex session:

```
1. codex(prompt="...", cwd="...", sandbox="workspace-write")
   → { threadId: "abc123", content: "..." }

2. codex-reply(threadId="abc123", prompt="...")
   → { threadId: "abc123", content: "..." }

3. codex-reply(threadId="abc123", prompt="...")
   → ...
```

### Thread Persistence

`threadId` is durable — it survives across Claude sessions. This enables:

- **Async long-running tasks** — start a Codex session, close Claude, resume later in a new session using the saved `threadId`
- **Handoff between orchestrators** — one Claude session starts the task, another continues it
- **Checkpointing** — store `threadId` externally and re-attach at any point

To resume a previous thread, simply call `codex-reply` with the stored `threadId` in any session:

```json
{
  "name": "codex-reply",
  "arguments": {
    "threadId": "abc123",
    "prompt": "Continue where you left off."
  }
}
```

---

## Approval Policy

Controls when Codex pauses to ask for confirmation on shell commands:

| Value | Behavior |
|---|---|
| `untrusted` | Prompt for all commands |
| `on-failure` | Prompt only if a command fails |
| `on-request` | Prompt only when model explicitly requests it |
| `never` | Never prompt — fully autonomous |

For use as a Claude subagent, `never` or `on-failure` are typical since there is no interactive user in the loop.

---

## Sandbox Modes

| Value | Behavior |
|---|---|
| `read-only` | No writes allowed |
| `workspace-write` | Writes scoped to the working directory |
| `danger-full-access` | Unrestricted filesystem and network access |
